//! Tauri commands backing the chat window: session state, sending a
//! message (streamed via `Channel<T>`), stopping, and starting a new
//! session. See `design.md`'s "In-flight generation decoupled from
//! window lifecycle" -- the session log is the source of truth
//! regardless of whether any window is listening; the `Channel` is
//! best-effort delivery to whichever window happens to be open right
//! now. A reopened window that missed the live stream recovers via
//! `get_chat_state`'s `partial_text`/`is_pending` fields rather than
//! re-attaching to the original channel, which Tauri has no mechanism
//! for -- a deliberate Stage 1 simplification.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use fleet_snowfluff_ai::{
    detect_escalation, log::LogRole, with_task_router_rules, AemeathAgentRuntime,
    AemeathContextManager, AgentRuntime, AiProvider, AiSettings, AuthMethod, ChatStream,
    ClaudeCodeToolAccess, CliContext, ConnectMcpServerTool, ContextManager, DefaultTaskRouter,
    DelegateTool, EscalationDecision, ExecutionPath, GetSystemContextTool, Language,
    ListDirectoryTool, LogEntry, Message, NeverConnectMcp, PendingToolCall, PermissionDecider,
    PermissionTier, Persona, ProfileKey, ProviderCredentials, ProviderKind, ProviderProfile,
    ReadFileTool, ResponseLanguage, RoutingContext, RunCommandTool, Task, TaskRouter,
    TaskRouterMode, Tool, ToolCallRecord, ToolCallStream, ToolCallStreamItem, ToolCallingProvider,
    ToolContext, ToolInvocation, ToolOutcome, ToolRegistry, ToolResult, WebSearchTool,
};
use futures_util::StreamExt;
use tauri::{ipc::Channel, AppHandle, Manager, State};

use crate::{
    ai_commands::{self, RoutedExecution},
    chat_log_store::{self, ChatLogStore},
    chat_pause, chat_window, cli_workdir, execution_log_store,
    execution_recorder::ExecutionRecorder,
    manager::PetManager,
    persona_store,
    session_domain::{CliSessionEntry, ConversationId, ExecutionId, ExternalSessionRef},
    status_bubble, task_router_rules_store,
};

/// `ContextManager` has no other state to hold onto between calls --
/// constructed fresh at each call site, the same way `ChatLogStore`
/// (one of its two adapters) already is everywhere else in this file.
fn context_manager() -> AemeathContextManager {
    AemeathContextManager::new(
        Box::new(ChatLogStore),
        Box::new(execution_log_store::ExecutionLogStore),
    )
}

pub struct PendingGeneration {
    handle: tokio::task::JoinHandle<()>,
    partial_text: Arc<Mutex<String>>,
}

/// What the status bubble should show once a generation resolves while
/// the chat window isn't focused to see it happen
/// (`ai-chat`'s "Status bubble").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadKind {
    Reply,
    Failure,
}

/// What a session-scoped "remember this for the session" tool-call
/// approval actually matches against on a later call
/// (`agent-core-and-task-router`'s "Session-scoped trust for repeated
/// tool use"). Path-less for most tools (remembering by tool name alone
/// is enough), but a call whose own arguments carry a `"path"` field
/// (`read_file`/`list_directory` today) folds the path in too --
/// path-less remembering would otherwise silently trust every future
/// path the moment any single one was approved once.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RememberKey {
    Tool(String),
    ToolPath(String, String),
}

impl RememberKey {
    pub fn for_call(tool_name: &str, arguments: &serde_json::Value) -> Self {
        match arguments.get("path").and_then(serde_json::Value::as_str) {
            Some(path) => RememberKey::ToolPath(tool_name.to_string(), path.to_string()),
            None => RememberKey::Tool(tool_name.to_string()),
        }
    }
}

#[derive(Default)]
pub struct ChatRuntimeState {
    session_path: Mutex<Option<PathBuf>>,
    pending: Mutex<Option<PendingGeneration>>,
    unread: Mutex<Option<UnreadKind>>,
    /// The underlying CLI's own session/thread id, per (conversation,
    /// profile), for CLI-backed subscription providers' warm-session
    /// model (both `ClaudeCodeCli` and `Codex` --
    /// `subscription-first-chat`'s "Session continuity for CLI-backed
    /// subscription providers"). The hot path only: every entry is
    /// also written through to the conversation's execution log
    /// (`execution_log_store`, `execution-log-and-context` -- this used
    /// to be a dedicated sidecar file, `cli_session_store`, before the
    /// execution log replaced it as the one persisted record of what
    /// happened each turn), which is read back lazily on a miss, so a
    /// session survives an app restart (`cli-session-continuity`).
    /// Each entry records the working directory it was created under
    /// and is used only from that same directory. Keyed by
    /// `ConversationId` as well as `ProfileKey`, not `ProfileKey` alone
    /// -- Aemeath has exactly one conversation at a time today, so this
    /// makes no observable difference yet, but a `ProfileKey`-only key
    /// would silently hand one conversation's session to another's
    /// request for the same profile the moment that's no longer true
    /// (see design.md's Decisions for why this is treated differently
    /// from `PendingGeneration`, which is deliberately left alone).
    cli_sessions: Mutex<HashMap<(ConversationId, ProfileKey), CliSessionEntry>>,
    /// Tool calls the user has approved for the rest of the current
    /// conversation (task 6.4) -- never persisted, cleared on the same
    /// `new_chat_session` lifecycle as `cli_sessions`.
    remembered_tools: Mutex<std::collections::HashSet<(ConversationId, RememberKey)>>,
}

impl ChatRuntimeState {
    pub fn is_pending(&self) -> bool { self.pending.lock().unwrap().is_some() }

    pub fn unread(&self) -> Option<UnreadKind> { *self.unread.lock().unwrap() }

    pub fn clear_unread(&self) { *self.unread.lock().unwrap() = None; }

    pub fn is_tool_remembered(&self, conversation_id: &ConversationId, key: &RememberKey) -> bool {
        self.remembered_tools.lock().unwrap().contains(&(conversation_id.clone(), key.clone()))
    }

    pub fn remember_tool(&self, conversation_id: ConversationId, key: RememberKey) {
        self.remembered_tools.lock().unwrap().insert((conversation_id, key));
    }

    /// Cancels any pending generation and clears the per-conversation
    /// state (`cli_sessions`, `remembered_tools`, unread) that must not
    /// carry over to a different conversation -- shared by
    /// `new_chat_session` and `open_conversation`, which differ only in
    /// what `session_path` is pointed at afterward (design.md's Decision
    /// 8). Returns whether a generation was actually cancelled, mostly
    /// so this is directly testable without an `AppHandle` (the
    /// `on_chat_activity_changed` notification that also belongs to this
    /// reset is the caller's job, at the Tauri-command layer).
    fn reset_for_new_conversation(&self) -> bool {
        let cancelled = match self.pending.lock().unwrap().take() {
            Some(pending) => {
                pending.handle.abort();
                true
            }
            None => false,
        };
        self.clear_unread();
        self.cli_sessions.lock().unwrap().clear();
        self.remembered_tools.lock().unwrap().clear();
        cancelled
    }

    /// Everything a CLI-backed provider needs about the conversation it
    /// is continuing (`CliContext`): the session to resume (if a
    /// still-usable one is stored), the transcript, how much of it that
    /// session has seen, and the working directory. The one place this is
    /// built, used by both `send_chat_message`'s direct branch and
    /// `run_generation_fallback` (the escalation path), so the two cannot
    /// disagree about what a CLI is told.
    pub fn cli_context(
        &self,
        session_path: &Path,
        conversation_id: &ConversationId,
        profile_key: ProfileKey,
        history: Vec<Message>,
        working_dir: PathBuf,
    ) -> CliContext {
        let (resume_session_id, seen_turns) =
            match self.stored_session(session_path, conversation_id, profile_key, &working_dir) {
                Some((id, seen)) => (Some(id), seen),
                None => (None, 0),
            };
        CliContext { resume_session_id, history, seen_turns, working_dir }
    }

    /// The `(session id, turns seen)` to resume for `(conversation,
    /// profile)` when the CLI is about to run in `working_dir`, or `None`
    /// to start a fresh (history-seeded) one. Checks the in-memory map
    /// first and, on a miss, the conversation's execution log, so a session
    /// survives an app restart. A session created under a different
    /// working directory is never returned.
    fn stored_session(
        &self,
        session_path: &Path,
        conversation_id: &ConversationId,
        profile_key: ProfileKey,
        working_dir: &Path,
    ) -> Option<(String, usize)> {
        let key = (conversation_id.clone(), profile_key);
        let mut sessions = self.cli_sessions.lock().unwrap();
        if let Some(entry) = sessions.get(&key) {
            return (entry.cwd == working_dir).then(|| (entry.session.0.clone(), entry.seen_turns));
        }
        let (session_id, seen_turns) =
            execution_log_store::latest_usable_session(session_path, profile_key, working_dir)?;
        sessions.insert(
            key,
            CliSessionEntry {
                session: ExternalSessionRef(session_id.clone()),
                cwd: working_dir.to_path_buf(),
                seen_turns,
            },
        );
        Some((session_id, seen_turns))
    }

    /// Replaces the old free function `store_cli_session_id` (which used
    /// to call the since-removed `record_session`): updates the
    /// in-memory session cache *and* marks `turn`'s `ExecutionRecorder`
    /// with this turn's outcome, in one place, so the two can never
    /// disagree about what happened. `provider` is read once for its
    /// `session_id()`, exactly as the old function did; `completed`
    /// distinguishes a normal finish from an error.
    ///
    /// `seen_turns` is computed here, not passed in: a completed turn's
    /// session now holds `turn.history_len` plus the user message and
    /// the reply; an errored turn's own effect on what the session
    /// absorbed is unknown, so `turn.prior_seen_turns` (the cursor that
    /// session already had when this turn began) is reused verbatim --
    /// under-counting only re-sends history on the next resume,
    /// over-counting would skip turns the CLI never actually saw.
    pub fn record_turn_outcome(
        &self,
        turn: &mut TurnCtx,
        conversation_id: ConversationId,
        profile_key: ProfileKey,
        provider: &dyn AiProvider,
        completed: bool,
    ) {
        let Some(id) = provider.session_id() else {
            if completed {
                turn.recorder.mark_completed(None, None, Vec::new());
            } else {
                turn.recorder.mark_errored(None, None, Vec::new());
            }
            return;
        };
        let seen_turns = if completed { turn.history_len + 2 } else { turn.prior_seen_turns };
        self.cli_sessions.lock().unwrap().insert(
            (conversation_id, profile_key),
            CliSessionEntry {
                session: ExternalSessionRef(id.clone()),
                cwd: turn.working_dir.clone(),
                seen_turns,
            },
        );
        if completed {
            turn.recorder.mark_completed(Some(id), Some(seen_turns), Vec::new());
        } else {
            turn.recorder.mark_errored(Some(id), Some(seen_turns), Vec::new());
        }
    }
}

/// Called from every point where "is a generation pending" or "is
/// there an unread result" could have changed: keeps the pause
/// (`chat_pause::recompute`) and the status bubble's visibility
/// (`status_bubble::sync`) both in sync with the same underlying
/// state, rather than each call site remembering to update both.
fn on_chat_activity_changed(app: &AppHandle) {
    chat_pause::recompute(app);
    status_bubble::sync(app);
}

/// Marks `kind` as unread unless the chat window is currently focused
/// -- a user actively watching the reply arrive doesn't need a bubble
/// telling them it arrived.
fn mark_unread_unless_focused(app: &AppHandle, kind: UnreadKind) {
    let is_focused = app
        .get_webview_window(chat_window::CHAT_WINDOW_LABEL)
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false);
    if !is_focused {
        *app.state::<ChatRuntimeState>().unread.lock().unwrap() = Some(kind);
        on_chat_activity_changed(app);
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    Chunk { delta: String },
    Done { content: String },
    Error { message: String },
}

#[derive(serde::Serialize)]
pub struct ChatStateSnapshot {
    entries: Vec<LogEntry>,
    is_pending: bool,
    /// Text streamed so far for a still-pending generation -- lets a
    /// window that (re)opened after the generation started show
    /// something immediately rather than waiting for the next chunk.
    partial_text: String,
    ai_ready: bool,
    /// `"disabled"` or `"no_provider"`, for the frontend to localize;
    /// `None` when `ai_ready` is `true`.
    not_ready_reason: Option<&'static str>,
    /// The conversation currently open -- lets the picker (6.4) mark/
    /// grey out the open conversation using data it already has to
    /// fetch anyway, with no separate "is this the open one" query
    /// command (design.md's Decision 8).
    session_path: PathBuf,
}

/// Whether a chat message can be sent right now and, if not, the reason
/// code the frontend localizes. The third reason is new with
/// `api-key-tool-calling`: an *enabled* profile whose disclosure is not
/// acknowledged (the API-key disclosure text changed, so earlier
/// acknowledgements were cleared once) must not be used until the user
/// has seen the updated text -- otherwise the disclosure could be skipped
/// simply by carrying on chatting, and tool results would reach the
/// provider without it.
fn chat_readiness(settings: &AiSettings) -> (bool, Option<&'static str>) {
    if !settings.ai_enabled {
        (false, Some("disabled"))
    } else if settings.default_profile.is_none() {
        (false, Some("no_provider"))
    } else if !ai_commands::disclosure_ok(settings, settings.default_profile) {
        (false, Some("disclosure_pending"))
    } else {
        (true, None)
    }
}

fn resolve_session_path(app: &AppHandle, chat_state: &ChatRuntimeState) -> PathBuf {
    let mut guard = chat_state.session_path.lock().unwrap();
    if let Some(path) = guard.as_ref() {
        return path.clone();
    }
    let path = chat_log_store::current_or_new_session(app)
        .unwrap_or_else(|| PathBuf::from("chat-logs/unresolved-session.jsonl"));
    *guard = Some(path.clone());
    path
}

#[tauri::command]
pub fn get_chat_state(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    chat_state: State<ChatRuntimeState>,
) -> ChatStateSnapshot {
    let session_path = resolve_session_path(&app, &chat_state);
    let entries = chat_log_store::read_session(&session_path);

    let (is_pending, partial_text) = match chat_state.pending.lock().unwrap().as_ref() {
        Some(pending) => (true, pending.partial_text.lock().unwrap().clone()),
        None => (false, String::new()),
    };

    let settings = ai_settings.lock().unwrap();
    let (ai_ready, not_ready_reason) = chat_readiness(&settings);

    ChatStateSnapshot {
        entries,
        is_pending,
        partial_text,
        ai_ready,
        not_ready_reason,
        session_path,
    }
}

/// Every past conversation, newest-first (`execution-log-and-context`'s
/// "List and open past conversations").
#[tauri::command]
pub fn list_conversations(app: AppHandle) -> Vec<chat_log_store::ConversationSummary> {
    chat_log_store::list_conversations(&app)
}

/// Thin Tauri-command-layer wrapper around
/// `ChatRuntimeState::reset_for_new_conversation` that also fires the
/// `AppHandle`-dependent activity notification -- kept separate so the
/// state-reset logic itself stays testable without an `AppHandle`.
fn reset_chat_runtime_state(app: &AppHandle, chat_state: &ChatRuntimeState) {
    chat_state.reset_for_new_conversation();
    on_chat_activity_changed(app);
}

/// Opens an existing conversation (`execution-log-and-context`'s "List
/// and open past conversations"): cancels any pending generation and
/// clears the same per-conversation state `new_chat_session` does, then
/// points `chat_state.session_path` at `path` instead of a freshly
/// created one. Opening the conversation that is already open is a
/// harmless no-op (same cancel-and-clear happens either way, matching
/// what clicking "New Chat" on the current conversation would also do).
#[tauri::command]
pub fn open_conversation(app: AppHandle, chat_state: State<ChatRuntimeState>, path: PathBuf) {
    reset_chat_runtime_state(&app, &chat_state);
    *chat_state.session_path.lock().unwrap() = Some(path);
}

/// The testable core of `delete_conversation` -- takes the currently-open
/// path directly rather than `State<ChatRuntimeState>`, which cannot be
/// constructed in a unit test here (same reason `reset_for_new_conversation`
/// is a `ChatRuntimeState` method rather than a free function taking
/// `&AppHandle`). Refuses (returns `false`) when `path` is the open
/// conversation; otherwise best-effort removes the transcript and its
/// execution-log sibling (`.ok()` on each, matching every other
/// file-removal/write in this codebase) and never touches an external
/// CLI runtime's own session store -- Aemeath has never owned that
/// lifecycle (`cli-session-continuity`'s own standing principle).
fn delete_conversation_files(current_session_path: Option<&Path>, path: &Path) -> bool {
    if current_session_path == Some(path) {
        return false;
    }
    std::fs::remove_file(path).ok();
    std::fs::remove_file(execution_log_store::log_path(path)).ok();
    true
}

/// Deletes a conversation (`execution-log-and-context`'s "Delete a
/// conversation"). The UI's own confirm step (6.4/7.2) is a separate,
/// earlier gate -- this command does not re-confirm.
#[tauri::command]
pub fn delete_conversation(chat_state: State<ChatRuntimeState>, path: PathBuf) -> bool {
    let current = chat_state.session_path.lock().unwrap();
    delete_conversation_files(current.as_deref(), &path)
}

fn map_ui_language(ui: fleet_snowfluff_core::UiLanguage) -> Language {
    use fleet_snowfluff_core::UiLanguage;
    match ui {
        UiLanguage::ZhHant => Language::ZhHant,
        UiLanguage::ZhHans => Language::ZhHans,
        UiLanguage::En => Language::En,
        UiLanguage::Ja => Language::Ja,
        UiLanguage::Ko => Language::Ko,
    }
}

/// `Auto` defers to the detected system UI language; any explicit
/// choice in the persona file overrides it (`personas/aemeath.yaml`'s
/// own `response_language` comment documents this precedence).
fn resolve_language(persona: &Persona, detected: Language) -> Language {
    match persona.response_language {
        ResponseLanguage::Auto => detected,
        ResponseLanguage::ZhHant => Language::ZhHant,
        ResponseLanguage::ZhHans => Language::ZhHans,
        ResponseLanguage::En => Language::En,
        ResponseLanguage::Ja => Language::Ja,
        ResponseLanguage::Ko => Language::Ko,
    }
}

fn now_rfc3339() -> String { chrono::Utc::now().to_rfc3339() }

/// Enforces `ai-chat`'s "Single in-flight generation": sends the
/// user's message and starts streaming the reply, refusing if one is
/// already pending. Gates on the master switch/active provider first
/// (`ai-provider`'s "AI features disabled by default" / "No provider
/// configured by default") -- neither case sends any request.
#[tauri::command]
pub async fn send_chat_message(
    app: AppHandle,
    ai_settings: State<'_, Mutex<AiSettings>>,
    credentials: State<'_, Mutex<ProviderCredentials>>,
    manager: State<'_, Mutex<PetManager>>,
    chat_state: State<'_, ChatRuntimeState>,
    channel: Channel<ChatEvent>,
    message: String,
) -> Result<(), String> {
    if chat_state.pending.lock().unwrap().is_some() {
        return Err("a generation is already pending".to_string());
    }

    let (settings_snapshot, creds_snapshot) = {
        let settings = ai_settings.lock().unwrap();
        let creds = credentials.lock().unwrap();
        (settings.clone(), creds.clone())
    };

    if !settings_snapshot.ai_enabled {
        channel.send(ChatEvent::Error { message: "disabled".to_string() }).ok();
        return Ok(());
    }
    let Some(default_profile) = settings_snapshot.default_profile().cloned() else {
        channel.send(ChatEvent::Error { message: "no_provider".to_string() }).ok();
        return Ok(());
    };
    // Backstop for `chat_readiness` (the input is normally already
    // disabled): never send to a profile whose disclosure is pending.
    if !ai_commands::disclosure_ok(&settings_snapshot, Some(default_profile.key())) {
        channel.send(ChatEvent::Error { message: "disclosure_pending".to_string() }).ok();
        return Ok(());
    }

    let local_profile_key =
        ProfileKey { provider: ProviderKind::Ollama, auth_method: AuthMethod::Local };
    let local_profile = settings_snapshot.profile(local_profile_key).cloned();

    // Task Router's initial route (`agent-core-and-task-router`'s
    // "Task routing mode") -- called exactly once per message. `single`
    // always resolves back to `default_profile`; `mix` resolves to
    // `local_profile` when Ollama is enabled, or to `default_profile`
    // directly otherwise (knowable upfront, not a runtime failure).
    // Anything that happens *after* this -- the local model's own
    // escalation-marker response, or an infra-level failure of the
    // local attempt -- is handled below without calling `route()`
    // again; see `task_router.rs`'s own module doc for why.
    let routing_context = RoutingContext {
        task: Task::new(message.clone()),
        mode: settings_snapshot.task_router_mode,
        default_profile: default_profile.clone(),
        local_profile: local_profile.clone(),
    };
    let route = match DefaultTaskRouter.route(routing_context).await {
        Ok(route) => route,
        Err(err) => {
            channel.send(ChatEvent::Error { message: err.to_string() }).ok();
            return Ok(());
        }
    };
    // Bug found during Group 6 wiring, fixed here: comparing only
    // `route.profile_key` against `local_profile_key` is not enough --
    // `single` mode always resolves to `default_profile` (see
    // `DefaultTaskRouter::route`), so if the user happens to have set
    // *Ollama* as their `default_profile` (a normal, supported choice,
    // nothing stops it), `route.profile_key` trivially equals
    // `local_profile_key` even in `single` mode, and this would
    // incorrectly run the mix-mode local-classification dance
    // (`task-router-rules.md` injected, `<<ESCALATE>>` detection) for a
    // mode that's supposed to mean "always use `default_profile`
    // directly, no routing games at all". Gating on `mode == Mix` too
    // closes that.
    let routed_to_local = settings_snapshot.task_router_mode == TaskRouterMode::Mix
        && route.profile_key == local_profile_key;

    // Where a CLI-backed provider runs this turn -- resolved once, up
    // front, so the resume lookup below and the spawn agree on it.
    // Only a CLI-backed profile has a working directory; resolving one
    // creates the fallback folder, which an Ollama- or API-key-only user
    // should never get.
    let cli_working_dir = if default_profile.uses_cli() {
        cli_workdir::for_app(&app, settings_snapshot.project_root.as_deref())
    } else {
        PathBuf::new()
    };

    let session_path = resolve_session_path(&app, &chat_state);
    let conversation_id = ConversationId::from_session_path(&session_path);
    let execution_id = ExecutionId::new();
    let history_entries = chat_log_store::read_session(&session_path);
    let context = fleet_snowfluff_ai::log::entries_to_context(&history_entries);

    let persona = persona_store::load(&app).persona;
    let detected_language = map_ui_language(manager.lock().unwrap().ui_language());
    let language = resolve_language(&persona, detected_language);
    // Persona-only message list -- what `default_profile` always uses,
    // whether that's because `mode` is `single`, because `mix` resolved
    // directly to it (Ollama not enabled), or because it's serving as
    // the fallback for a mix-mode local attempt that escalates or fails
    // below. Never has `task-router-rules.md` appended -- only the
    // local attempt's own message list does. Built before the user
    // entry is appended below, since `build_context` reads the log
    // itself and appends `message` on its own.
    let default_messages =
        context_manager().build_context(&session_path, &persona, language, &message).await;

    // Appended immediately -- part of history regardless of what
    // happens to the reply.
    chat_log_store::append_entry(
        &session_path,
        &LogEntry { role: LogRole::User, content: message.clone(), timestamp: now_rfc3339() },
    );

    let partial_text = Arc::new(Mutex::new(String::new()));
    let task_app = app.clone();
    let task_channel = channel.clone();
    let task_partial_text = partial_text.clone();
    let task_session_path = session_path.clone();

    let handle = if routed_to_local {
        // `local_profile` is guaranteed `Some` here: `route()` only
        // ever resolves to `local_profile_key` when `local_profile` was
        // itself `Some` (see `DefaultTaskRouter::route`).
        let local_profile = local_profile.expect("routed to local profile, but none is enabled");
        let local_messages =
            with_task_router_rules(default_messages.clone(), &task_router_rules_store::load(&app));
        let fallback = FallbackAttempt {
            profile: default_profile,
            messages: default_messages,
            history: context,
            working_dir: cli_working_dir,
            credentials: creds_snapshot.clone(),
            project_root: settings_snapshot.project_root.clone(),
            claude_code_tool_access: settings_snapshot.claude_code_tool_access,
        };
        tokio::spawn(async move {
            run_generation_mix_local(
                task_app,
                local_profile,
                local_messages,
                creds_snapshot,
                fallback,
                execution_id,
                conversation_id,
                task_channel,
                task_partial_text,
                task_session_path,
            )
            .await;
        })
    } else {
        let profile_key = default_profile.key();
        let cli = chat_state.cli_context(
            &session_path,
            &conversation_id,
            profile_key,
            context,
            cli_working_dir,
        );
        let project_root = settings_snapshot.project_root.clone();
        let claude_code_tool_access = settings_snapshot.claude_code_tool_access;
        tokio::spawn(async move {
            run_generation_routed(
                task_app,
                creds_snapshot,
                default_profile,
                cli,
                ExecutionPath::Direct,
                claude_code_tool_access,
                project_root,
                execution_id,
                conversation_id,
                default_messages,
                task_channel,
                task_partial_text,
                task_session_path,
            )
            .await;
        })
    };

    *chat_state.pending.lock().unwrap() = Some(PendingGeneration { handle, partial_text });
    on_chat_activity_changed(&app);
    Ok(())
}

/// Per-turn facts a run needs to persist its CLI session and its
/// `Execution` record, bundled so `run_generation` / `stream_to_completion`
/// do not each take them as loose parameters.
pub struct TurnCtx {
    session_path: PathBuf,
    working_dir: PathBuf,
    /// How many transcript messages preceded this turn's user message.
    /// A completed turn leaves a CLI session holding that many plus the
    /// user message and the reply.
    history_len: usize,
    /// The resumed session's own catch-up cursor at the start of this
    /// turn, if one was found -- 0 if none was (including for a
    /// `MixLocal` attempt, which never has one). Reused verbatim if this
    /// same turn also errors after capturing a session id, since an
    /// errored turn's own effect on what the session absorbed is
    /// unknown (`ChatRuntimeState::record_turn_outcome`).
    prior_seen_turns: usize,
    /// Owns this turn's `Execution` record for its whole lifetime:
    /// constructed (writing the `start` event) before the provider is
    /// ever called, marked with its outcome right before this function
    /// returns, and -- if neither happens, e.g. the task is aborted --
    /// recorded as `Cancelled` by its own `Drop`
    /// (`execution-log-and-context`, design.md's Decision 5).
    recorder: ExecutionRecorder,
}

/// Everything needed to retry against `default_profile` if a mix-mode
/// local attempt escalates or fails -- bundled together so
/// `run_generation_mix_local` has one thing to hold onto rather than
/// several loose clones.
#[derive(Clone)]
struct FallbackAttempt {
    profile: ProviderProfile,
    messages: Vec<Message>,
    /// The conversation's real prior turns (not `messages`, which also
    /// carries persona few-shot examples) -- seeds a CLI-backed
    /// fallback target's fresh session so an escalated message isn't
    /// answered without the turns a local provider handled earlier.
    history: Vec<Message>,
    /// Where a CLI-backed fallback target runs (`cli_workdir`).
    working_dir: PathBuf,
    credentials: ProviderCredentials,
    project_root: Option<PathBuf>,
    claude_code_tool_access: ClaudeCodeToolAccess,
}

/// Retries the message against `fallback.profile` -- the destination
/// for both an escalated and an infra-failed mix-mode local attempt
/// (`run_generation_mix_local`'s three call sites). Takes `fallback` by
/// value since each call site only ever runs once per message (they're
/// mutually exclusive branches); cloned at the call site rather than
/// here so a caller with `fallback` still available afterward (there
/// isn't one today, but this keeps the function itself agnostic to
/// that).
#[allow(clippy::too_many_arguments)]
async fn run_generation_fallback(
    app: AppHandle,
    fallback: FallbackAttempt,
    execution_id: ExecutionId,
    conversation_id: ConversationId,
    channel: Channel<ChatEvent>,
    partial_text: Arc<Mutex<String>>,
    session_path: PathBuf,
) {
    let profile_key = fallback.profile.key();
    let cli = app.state::<ChatRuntimeState>().cli_context(
        &session_path,
        &conversation_id,
        profile_key,
        fallback.history,
        fallback.working_dir,
    );
    run_generation_routed(
        app,
        fallback.credentials,
        fallback.profile,
        cli,
        ExecutionPath::Fallback,
        fallback.claude_code_tool_access,
        fallback.project_root,
        execution_id,
        conversation_id,
        fallback.messages,
        channel,
        partial_text,
        session_path,
    )
    .await;
}

/// Decides, for a message's *final* destination profile, whether to run
/// the existing plain `chat()` path or the tool-calling Agent Loop --
/// the two places that decision needs making (`send_chat_message`'s own
/// direct-to-`default_profile` branch, and `run_generation_fallback`
/// above). Not applied inside `run_generation_mix_local`'s own
/// local-classification attempt, which hand-rolls its own, narrower
/// tool-calling path for the same reason it never routed through this
/// function at all: `route_provider`'s `ToolCapable` branch hands off
/// to `AemeathAgentRuntime::run()` wholesale, which has no way to
/// abort mid-stream the instant `<<ESCALATE>>` is detected
/// (`mix-mode-local-tools`'s design.md).
#[allow(clippy::too_many_arguments)]
async fn run_generation_routed(
    app: AppHandle,
    credentials: ProviderCredentials,
    profile: ProviderProfile,
    cli: CliContext,
    route: ExecutionPath,
    claude_code_tool_access: ClaudeCodeToolAccess,
    project_root: Option<PathBuf>,
    execution_id: ExecutionId,
    conversation_id: ConversationId,
    messages: Vec<Message>,
    channel: Channel<ChatEvent>,
    partial_text: Arc<Mutex<String>>,
    session_path: PathBuf,
) {
    let profile_key = profile.key();
    let recorder = ExecutionRecorder::start(
        session_path.clone(),
        conversation_id.clone(),
        route,
        profile_key,
        cli.working_dir.clone(),
    );
    let turn = TurnCtx {
        session_path: session_path.clone(),
        working_dir: cli.working_dir.clone(),
        history_len: cli.history.len(),
        prior_seen_turns: cli.seen_turns,
        recorder,
    };
    match ai_commands::route_provider(&credentials, &profile, cli, &claude_code_tool_access) {
        RoutedExecution::PlainChat(provider) => {
            run_generation(
                app,
                provider,
                execution_id,
                conversation_id,
                profile_key,
                messages,
                channel,
                partial_text,
                turn,
            )
            .await;
        }
        RoutedExecution::ToolCapable(provider) => {
            run_generation_with_tools(
                app,
                provider,
                execution_id,
                conversation_id,
                profile_key,
                project_root,
                messages,
                channel,
                partial_text,
                turn,
            )
            .await;
        }
    }
}

/// `execution_id` identifies this one turn's execution
/// (`session_domain::ExecutionId`) -- logged for traceability, not
/// persisted or exposed over IPC (see `session_domain`'s own module
/// doc for why this is a typing-only addition, not a status-tracking
/// one). Used for `single` mode, for `mix` mode when it resolves
/// directly to `default_profile` (Ollama not enabled), and as the
/// fallback retry when a mix-mode local attempt escalates or fails --
/// in every case, this is simply "run one attempt against one provider
/// to completion," with no further fallback behind it.
#[allow(clippy::too_many_arguments)]
async fn run_generation(
    app: AppHandle,
    provider: Box<dyn AiProvider>,
    execution_id: ExecutionId,
    conversation_id: ConversationId,
    profile_key: ProfileKey,
    messages: Vec<Message>,
    channel: Channel<ChatEvent>,
    partial_text: Arc<Mutex<String>>,
    mut turn: TurnCtx,
) {
    log::debug!("{execution_id:?} starting for {profile_key:?}");

    let mut stream = match provider.chat(messages).await {
        Ok(stream) => stream,
        Err(err) => {
            app.state::<ChatRuntimeState>().record_turn_outcome(
                &mut turn,
                conversation_id,
                profile_key,
                provider.as_ref(),
                false,
            );
            finish_with_error(&app, &turn.session_path, &channel, &err.to_string());
            return;
        }
    };

    stream_to_completion(
        app,
        provider,
        &mut stream,
        conversation_id,
        profile_key,
        channel,
        partial_text,
        turn,
    )
    .await;
}

/// Reads whatever remains of an already-started `stream` to
/// completion, forwarding each chunk live and appending the full reply
/// to the chat log at the end -- shared by `run_generation`'s own
/// stream (from the very first chunk) and `run_generation_mix_local`'s
/// continuation once it has decided a local attempt is simple (from
/// wherever it stopped buffering). `partial_text` may already contain
/// content flushed by the caller before this is invoked -- appended to,
/// never reset.
#[allow(clippy::too_many_arguments)]
async fn stream_to_completion(
    app: AppHandle,
    provider: Box<dyn AiProvider>,
    stream: &mut ChatStream,
    conversation_id: ConversationId,
    profile_key: ProfileKey,
    channel: Channel<ChatEvent>,
    partial_text: Arc<Mutex<String>>,
    mut turn: TurnCtx,
) {
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                partial_text.lock().unwrap().push_str(&chunk.delta);
                channel.send(ChatEvent::Chunk { delta: chunk.delta }).ok();
            }
            Err(err) => {
                app.state::<ChatRuntimeState>().record_turn_outcome(
                    &mut turn,
                    conversation_id,
                    profile_key,
                    provider.as_ref(),
                    false,
                );
                finish_with_error(&app, &turn.session_path, &channel, &err.to_string());
                return;
            }
        }
    }

    app.state::<ChatRuntimeState>().record_turn_outcome(
        &mut turn,
        conversation_id,
        profile_key,
        provider.as_ref(),
        true,
    );
    let final_text = partial_text.lock().unwrap().clone();
    context_manager().record_execution(&turn.session_path, &final_text).await;
    channel.send(ChatEvent::Done { content: final_text }).ok();
    clear_pending(&app);
    mark_unread_unless_focused(&app, UnreadKind::Reply);
}

/// Every native tool a `ToolCapable` profile is offered, including
/// `delegate_task` (`sub-agent-delegation`) and `connect_mcp_server`
/// (`mcp-client-support`). Kept as a plain `Vec`, not a `ToolRegistry`,
/// so `run_generation_with_tools` can extend it with this turn's
/// MCP-sourced tools (`mcp_commands::mcp_sourced_tools`) before building
/// the registry -- every one of these is a plain, app-handle-free
/// `Arc::new(DefaultStruct)` construction already, so pulling the list
/// itself out costs nothing.
fn native_tool_list() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(WebSearchTool::default()),
        Arc::new(ReadFileTool),
        Arc::new(ListDirectoryTool),
        Arc::new(RunCommandTool::default()),
        Arc::new(GetSystemContextTool),
        Arc::new(DelegateTool),
        Arc::new(ConnectMcpServerTool),
    ]
}

/// The local-first mix-mode attempt's own, deliberately narrower
/// registry (`mix-mode-local-tools`): read-only, side-effect-free
/// tools only -- never `run_command` or `delegate_task`, in any
/// state, ever (see design.md's Non-Goals). `read_file`/`list_directory`
/// are only ever `Auto`-tier inside a configured project folder; with
/// none configured, every path asks for confirmation
/// (`AI_FEATURES.md`), which would make both tools either an
/// immediate escalate or a silent denial on every attempt -- dead
/// weight in the model's own tool list. `web_search`/
/// `get_system_context` are always `Auto` regardless, so they're
/// offered unconditionally.
fn mix_local_tool_registry(project_root: Option<&Path>) -> ToolRegistry {
    let mut tools: Vec<Arc<dyn Tool>> =
        vec![Arc::new(WebSearchTool::default()), Arc::new(GetSystemContextTool)];
    if project_root.is_some() {
        tools.push(Arc::new(ReadFileTool));
        tools.push(Arc::new(ListDirectoryTool));
    }
    ToolRegistry::new(tools)
}

/// Same contract as `run_generation`, but for a `ToolCapable` provider:
/// runs `AemeathAgentRuntime` instead of a plain `chat()` stream. Text
/// deltas stream live via the `on_text_delta` callback (see
/// `AgentRuntime::run`'s own doc comment -- every iteration's text
/// streams, not just the final one that ends the loop); tool calls and
/// their results never reach the UI or the chat log, only the model's
/// own narration and final answer do. `partial_text` (accumulated by
/// the same callback) is used as the definitive final content once the
/// loop finishes, exactly like `stream_to_completion` does for a plain
/// stream, rather than `AgentRuntime::run`'s own `Ok(AgentOutcome)`
/// return value's `text` field alone (which is only the *last*
/// iteration's text) -- its `trace` field, however, *is* used, to mark
/// this turn's `Execution` record (`execution-log-and-context`).
#[allow(clippy::too_many_arguments)]
async fn run_generation_with_tools(
    app: AppHandle,
    provider: Box<dyn ToolCallingProvider>,
    execution_id: ExecutionId,
    conversation_id: ConversationId,
    profile_key: ProfileKey,
    project_root: Option<PathBuf>,
    messages: Vec<Message>,
    channel: Channel<ChatEvent>,
    partial_text: Arc<Mutex<String>>,
    mut turn: TurnCtx,
) {
    log::debug!("{execution_id:?} starting tool-calling generation for {profile_key:?}");

    // No `ChatRuntimeState::record_turn_outcome` call here (unlike
    // `run_generation`/`stream_to_completion`): `route_provider` only
    // resolves `ToolCapable` for Ollama and the OpenAI/Anthropic API-key
    // profiles, and none of them has a resumable-session concept
    // (`AiProvider::session_id`'s default `None`, never overridden --
    // only the CLI-backed subscription providers keep one, and those are
    // never `ToolCapable`). `turn.recorder` is still marked directly
    // below, with the loop's own tool trace -- every turn gets an
    // `Execution` record uniformly, CLI session or not.
    let ctx = ToolContext { project_root, conversation_id: conversation_id.clone() };
    // MCP-sourced tools ride as *lazy* (`tool-list-optimization`): a
    // server like codebase-memory-mcp alone can expose a dozen-plus
    // tools, and sending every one's full schema every iteration is
    // exactly the "tool list too large, model gets slow and forgets
    // its own persona" failure mode real usage surfaced. Only offered
    // in full once the model actually searches for one
    // (`search_tools`, added as an eager tool alongside the native
    // ones whenever there's at least one MCP tool to search for).
    let mcp_tools = crate::mcp_commands::mcp_sourced_tools(&app).await;
    let registry = if mcp_tools.is_empty() {
        ToolRegistry::new(native_tool_list())
    } else {
        let lazy_summaries = mcp_tools
            .iter()
            .map(|tool| {
                let definition = tool.definition();
                (definition.name, definition.description)
            })
            .collect();
        let mut eager = native_tool_list();
        // Inserted right before `connect_mcp_server` (always last in
        // `native_tool_list()`), not appended after it: some models
        // show a real position bias toward earlier-listed tools, and
        // real usage showed a model reaching for `connect_mcp_server`
        // out of habit instead of this one -- putting this one first
        // is a cheap, low-risk nudge in the right direction, on top of
        // `connect_mcp_server`'s own description now explicitly
        // deferring to this tool.
        let connect_mcp_server_index = eager.len() - 1;
        eager.insert(
            connect_mcp_server_index,
            Arc::new(fleet_snowfluff_ai::SearchToolsTool::new(lazy_summaries)),
        );
        ToolRegistry::with_lazy(eager, mcp_tools)
    };
    {
        let (eager_count, lazy_count) = registry.tool_counts();
        // The exact JSON this iteration's own eager definitions (plus
        // whichever lazy ones, if any, are already unlocked -- none,
        // on the very first iteration) serialize to, character-counted
        // as a rough proxy for tokens (roughly 4 chars/token for
        // English; MCP tool descriptions are often denser than that).
        // Logged as real measurement, not another guess, after a real
        // report of persona/language instructions apparently being
        // lost -- consistent with this content alone pushing the
        // request past whatever context window the model is actually
        // honoring.
        let first_iteration_chars: usize = registry
            .definitions()
            .iter()
            .map(|d| d.name.len() + d.description.len() + d.parameters.to_string().len())
            .sum();
        log::info!(
            "{execution_id:?} offering {eager_count} eager tool(s), {lazy_count} lazy tool(s) \
             this turn -- eager tool definitions alone are {first_iteration_chars} chars (~{} \
             tokens at a rough 4 chars/token) before the system prompt or conversation history \
             are even counted",
            first_iteration_chars / 4
        );
    }
    let runtime = AemeathAgentRuntime::default();
    let permission = crate::tool_confirmation::PopupPermissionDecider {
        app: app.clone(),
        conversation_id: conversation_id.clone(),
        registry: &registry,
    };
    let mcp_connector = crate::tool_confirmation::PopupMcpConnector { app: app.clone() };

    let result = {
        let mut on_text_delta = |delta: &str| {
            partial_text.lock().unwrap().push_str(delta);
            channel.send(ChatEvent::Chunk { delta: delta.to_string() }).ok();
        };
        runtime
            .run(
                provider.as_ref(),
                messages,
                &registry,
                &ctx,
                &permission,
                &mcp_connector,
                &mut on_text_delta,
            )
            .await
    };

    match result {
        Ok(outcome) => {
            turn.recorder.mark_completed(None, None, outcome.trace.clone());
            let final_text = partial_text.lock().unwrap().clone();
            if final_text.is_empty() && outcome.trace.is_empty() {
                // Diagnostic-only: the model produced neither visible
                // text nor a single tool call -- a genuinely empty
                // reply, not an error (this turn still completed
                // `Ok`), so nothing else would otherwise report it.
                log::warn!(
                    "{execution_id:?} completed with a genuinely empty reply -- no text, no tool \
                     calls, for {profile_key:?}"
                );
            }
            context_manager().record_execution(&turn.session_path, &final_text).await;
            channel.send(ChatEvent::Done { content: final_text }).ok();
            clear_pending(&app);
            mark_unread_unless_focused(&app, UnreadKind::Reply);
        }
        Err(err) => {
            turn.recorder.mark_errored(None, None, Vec::new());
            finish_with_error(&app, &turn.session_path, &channel, &err.to_string());
        }
    }
}

/// Always denies every `Confirm`-tier call -- used once the mix-mode
/// local attempt below has already committed to running locally
/// (iteration 2 onward of `AemeathAgentRuntime::run()`). This path
/// never opens the real confirmation window (`mix-mode-local-tools`'s
/// design.md): a later `Confirm`-tier call is just reported back to
/// the model as not permitted, the same fail-closed shape
/// `agent-core-and-task-router`'s own `DenyAllConfirm` placeholder had
/// before `PopupPermissionDecider` replaced it.
struct AlwaysDenyConfirm;

/// One tool call from the mix-local attempt's first iteration, paired
/// with the tool it resolved to (`None` for an unknown name) and the
/// permission tier that resolved to.
type MixLocalCallPlan = (PendingToolCall, Option<Arc<dyn Tool>>, PermissionTier);

#[async_trait::async_trait]
impl PermissionDecider for AlwaysDenyConfirm {
    async fn decide(&self, _calls: &[PendingToolCall]) -> HashSet<String> { HashSet::new() }
}

/// What the mix-local attempt's first iteration resolved to, read
/// directly off `provider.chat_with_tools()`'s own stream -- no
/// `AppHandle` involved, so this (and [`read_mix_local_first_iteration`]
/// below) is directly unit-testable with a scripted `ToolCallStream`,
/// unlike the `AppHandle`-touching orchestration around it.
#[derive(Debug, PartialEq, Eq)]
enum MixLocalFirstIteration {
    /// The full `<<ESCALATE>>` marker matched -- the stream was
    /// dropped without reading further.
    Escalate,
    /// The stream ended with no tool calls at all -- `text` is
    /// already the complete answer.
    Simple { text: String },
    /// At least one tool call was requested. `pre_commitment` is
    /// whether the *first* one arrived while text was still
    /// [`EscalationDecision::Undecided`] -- the turn's very first
    /// action, not merely the state once the whole iteration is read.
    Committed { text: String, calls: Vec<PendingToolCall>, pre_commitment: bool },
    /// The stream itself errored mid-read.
    ProviderError,
}

/// Reads one `chat_with_tools()` iteration to completion, watching
/// text for [`detect_escalation`] exactly as the old text-only mix-local
/// attempt did, while also collecting any tool calls. Drops out of the
/// loop immediately on a conclusive `Escalate` (the caller is
/// responsible for actually dropping the stream/provider to cancel the
/// underlying request -- this function only *reads*, since the caller
/// still owns the stream after a `Simple`/`Committed`/`ProviderError`
/// outcome too).
async fn read_mix_local_first_iteration(stream: &mut ToolCallStream) -> MixLocalFirstIteration {
    let mut buffer = String::new();
    let mut calls: Vec<PendingToolCall> = Vec::new();
    let mut first_call_was_pre_commitment: Option<bool> = None;
    loop {
        match stream.next().await {
            Some(Ok(ToolCallStreamItem::TextDelta(delta))) => {
                buffer.push_str(&delta);
                if let EscalationDecision::Escalate = detect_escalation(&buffer) {
                    return MixLocalFirstIteration::Escalate;
                }
            }
            Some(Ok(ToolCallStreamItem::ToolCall { id, name, arguments })) => {
                if first_call_was_pre_commitment.is_none() {
                    first_call_was_pre_commitment =
                        Some(matches!(detect_escalation(&buffer), EscalationDecision::Undecided));
                }
                calls.push(PendingToolCall { id, name, arguments });
            }
            Some(Err(_)) => return MixLocalFirstIteration::ProviderError,
            None => break,
        }
    }
    if calls.is_empty() {
        MixLocalFirstIteration::Simple { text: buffer }
    } else {
        MixLocalFirstIteration::Committed {
            text: buffer,
            calls,
            pre_commitment: first_call_was_pre_commitment.unwrap_or(false),
        }
    }
}

/// Resolves each call to the tool it names (`None` for an unknown
/// name, classified as `Deny` since it's not `Auto` either way) and
/// the permission tier that call requires. No `AppHandle` -- directly
/// unit-testable alongside [`read_mix_local_first_iteration`].
fn classify_mix_local_calls(
    calls: Vec<PendingToolCall>,
    registry: &ToolRegistry,
    ctx: &ToolContext,
) -> Vec<MixLocalCallPlan> {
    calls
        .into_iter()
        .map(|call| match registry.find(&call.name) {
            Some(tool) => {
                let tier = tool.required_permission(&call.arguments, ctx);
                (call, Some(tool), tier)
            }
            None => (call, None, PermissionTier::Deny),
        })
        .collect()
}

/// `mode: mix`'s local-first attempt (`agent-core-and-task-router`'s
/// "Local-first classification in mixed mode", extended by
/// `mix-mode-local-tools` to let the attempt use a small, read-only
/// set of native tools -- see that change's own design.md for why
/// this hand-rolls its first iteration against
/// `provider.chat_with_tools()` directly rather than going through
/// `AemeathAgentRuntime::run()`: today's escalation mechanism depends
/// on being able to `drop(stream)` mid-read the instant
/// `<<ESCALATE>>` is detected, and `run()` offers no such abort hook.
///
/// [`detect_escalation`] now governs a *commitment* point covering
/// both text and an early tool call, not just text:
///
/// - a conclusive [`EscalationDecision::Escalate`] (the full marker matched)
///   drops the stream immediately, exactly as before -- nothing shown, nothing
///   logged;
/// - once the whole first iteration has been read, an outcome of
///   [`EscalationDecision::Simple`] (divergence, or the stream ending while
///   still a strict prefix) with no tool calls at all finishes the turn
///   directly with the now-complete text;
/// - a tool call requested while text was still [`EscalationDecision::
///   Undecided`] is itself a commitment, *unless* it (or any other call in that
///   same first batch) would need confirmation or is denied, in which case the
///   whole attempt is discarded the same way a textual `Escalate` is;
/// - once anything has been shown or executed, the attempt runs to completion
///   locally no matter what: a later tool call needing confirmation is silently
///   denied (never a popup -- see [`AlwaysDenyConfirm`]), and iteration 2
///   onward hands off to the ordinary, unmodified `AemeathAgentRuntime::run()`.
///
/// Either way, `TaskRouter::route()` is never called a second time --
/// see `task_router.rs`'s own module doc.
#[allow(clippy::too_many_arguments)]
async fn run_generation_mix_local(
    app: AppHandle,
    local_profile: ProviderProfile,
    local_messages: Vec<Message>,
    credentials: ProviderCredentials,
    fallback: FallbackAttempt,
    execution_id: ExecutionId,
    conversation_id: ConversationId,
    channel: Channel<ChatEvent>,
    partial_text: Arc<Mutex<String>>,
    session_path: PathBuf,
) {
    let local_profile_key = local_profile.key();
    log::debug!("{execution_id:?} starting mix-mode local attempt for {local_profile_key:?}");

    // This attempt's own `Execution` record -- recorded even though
    // its content, win or lose, is either discarded (escalated/
    // errored) or marked directly below once the attempt finishes.
    let mut recorder = ExecutionRecorder::start(
        session_path.clone(),
        conversation_id.clone(),
        ExecutionPath::MixLocal,
        local_profile_key,
        fallback.working_dir.clone(),
    );

    // Always Ollama+Local (`send_chat_message`'s own hardcoded
    // `local_profile_key`), which always `supports_tool_calling()` --
    // so `route_provider` provably never resolves `PlainChat` here.
    // No `cli_sessions` lookup: Ollama has no resumable-session
    // concept, so this is always a fresh call.
    let provider = match ai_commands::route_provider(
        &credentials,
        &local_profile,
        CliContext::fresh(std::env::temp_dir()),
        &fallback.claude_code_tool_access,
    ) {
        RoutedExecution::ToolCapable(provider) => provider,
        RoutedExecution::PlainChat(_) => unreachable!(
            "local_profile is always Ollama+Local, which always supports_tool_calling()"
        ),
    };
    let registry = mix_local_tool_registry(fallback.project_root.as_deref());
    let ctx = ToolContext {
        project_root: fallback.project_root.clone(),
        conversation_id: conversation_id.clone(),
    };

    let mut stream =
        match provider.chat_with_tools(local_messages.clone(), registry.definitions()).await {
            Ok(stream) => stream,
            Err(_) => {
                // Infra-level failure before the local attempt even
                // started (e.g. `RuntimeUnavailable` -- Ollama enabled
                // in settings but not actually reachable). Nothing was
                // shown or logged for this attempt; fall back directly.
                // `Errored`, not `Escalated` -- an infra failure, not
                // the local model's own content decision.
                recorder.mark_errored(None, None, Vec::new());
                drop(recorder);
                run_generation_fallback(
                    app,
                    fallback,
                    execution_id,
                    conversation_id,
                    channel,
                    partial_text,
                    session_path,
                )
                .await;
                return;
            }
        };

    let first_iteration = read_mix_local_first_iteration(&mut stream).await;

    let (buffer, calls, first_call_was_pre_commitment) = match first_iteration {
        MixLocalFirstIteration::Escalate => {
            // Nothing in the buffer was ever shown or logged. Drop the
            // stream/provider (ends the underlying HTTP request) and
            // retry fresh against `default_profile`.
            drop(stream);
            drop(provider);
            recorder.mark_escalated();
            drop(recorder);
            run_generation_fallback(
                app,
                fallback,
                execution_id,
                conversation_id,
                channel,
                partial_text,
                session_path,
            )
            .await;
            return;
        }
        MixLocalFirstIteration::ProviderError => {
            // Mid-stream infra failure. Whatever text there was was
            // never shown or logged -- fall back directly.
            recorder.mark_errored(None, None, Vec::new());
            drop(recorder);
            run_generation_fallback(
                app,
                fallback,
                execution_id,
                conversation_id,
                channel,
                partial_text,
                session_path,
            )
            .await;
            return;
        }
        MixLocalFirstIteration::Simple { text } => {
            // A pure text answer -- `Simple` via divergence, or the
            // stream ending while still `Undecided` (can't be
            // escalating if it never finished saying the marker). The
            // whole iteration has already been read to reach this
            // point, so this is already the complete answer; finish
            // directly rather than handing off to `stream_to_completion`
            // (there is nothing left on this stream for it to read).
            if !text.is_empty() {
                partial_text.lock().unwrap().push_str(&text);
                channel.send(ChatEvent::Chunk { delta: text }).ok();
            }
            recorder.mark_completed(None, None, Vec::new());
            let final_text = partial_text.lock().unwrap().clone();
            context_manager().record_execution(&session_path, &final_text).await;
            channel.send(ChatEvent::Done { content: final_text }).ok();
            clear_pending(&app);
            mark_unread_unless_focused(&app, UnreadKind::Reply);
            return;
        }
        MixLocalFirstIteration::Committed { text, calls, pre_commitment } => {
            (text, calls, pre_commitment)
        }
    };

    // At least one tool call. Classify each sequentially -- no
    // `buffer_unordered` concurrency: a first-iteration classification
    // realistically has 0-1 calls, not enough to justify duplicating
    // `AemeathAgentRuntime::run()`'s own bounded-concurrent execution
    // pass too.
    let plans = classify_mix_local_calls(calls, &registry, &ctx);

    let first_action_needs_escalation = first_call_was_pre_commitment
        && plans.iter().any(|(_, _, tier)| !matches!(tier, PermissionTier::Auto));

    if first_action_needs_escalation {
        // The turn's very first action, before anything else was shown
        // or run, would need confirmation or is denied -- treated the
        // same as a textual `Escalate`: discard everything local and
        // retry fresh against `default_profile`.
        drop(stream);
        drop(provider);
        recorder.mark_escalated();
        drop(recorder);
        run_generation_fallback(
            app,
            fallback,
            execution_id,
            conversation_id,
            channel,
            partial_text,
            session_path,
        )
        .await;
        return;
    }

    // Committed: flush any buffered text (the turn's own narration
    // before this tool call), execute `Auto`-tier calls for real,
    // silently deny the rest (never a popup), and build the follow-up
    // turn for `AemeathAgentRuntime::run()` to continue from.
    if !buffer.is_empty() {
        partial_text.lock().unwrap().push_str(&buffer);
        channel.send(ChatEvent::Chunk { delta: buffer.clone() }).ok();
    }

    let tool_call_records: Vec<ToolCallRecord> = plans
        .iter()
        .map(|(call, ..)| ToolCallRecord {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        })
        .collect();
    let mut next_messages = local_messages;
    next_messages.push(Message::assistant_with_tool_calls(buffer, tool_call_records));

    let mut trace: Vec<ToolInvocation> = Vec::new();
    for (call, tool, tier) in plans {
        match tool {
            None => {
                next_messages.push(Message::tool_result(
                    call.id.clone(),
                    format!("Error: unknown tool \"{}\"", call.name),
                ));
                trace.push(ToolInvocation {
                    name: call.name.clone(),
                    outcome: ToolOutcome::Rejected {
                        arguments_preview: call.arguments.to_string(),
                    },
                });
            }
            Some(tool) if matches!(tier, PermissionTier::Auto) => {
                let result = match tool.execute(call.arguments.clone(), &ctx).await {
                    Ok(result) => result,
                    Err(err) => ToolResult::error(err.to_string()),
                };
                let ok = !result.is_error;
                let content = if result.is_error {
                    format!("Error: {}", result.content)
                } else {
                    result.content
                };
                next_messages.push(Message::tool_result(call.id.clone(), content));
                trace.push(ToolInvocation {
                    name: call.name.clone(),
                    outcome: ToolOutcome::Executed { ok },
                });
            }
            Some(_) => {
                // `Confirm`/`Deny`-tier, already committed -- silently
                // denied, never a popup.
                next_messages.push(Message::tool_result(
                    call.id.clone(),
                    format!("Error: \"{}\" was not permitted to run", call.name),
                ));
                trace.push(ToolInvocation {
                    name: call.name.clone(),
                    outcome: ToolOutcome::DeniedByPolicy,
                });
            }
        }
    }

    let runtime = AemeathAgentRuntime::default();
    let decider = AlwaysDenyConfirm;
    let result = {
        let mut on_text_delta = |delta: &str| {
            partial_text.lock().unwrap().push_str(delta);
            channel.send(ChatEvent::Chunk { delta: delta.to_string() }).ok();
        };
        runtime
            .run(
                provider.as_ref(),
                next_messages,
                &registry,
                &ctx,
                &decider,
                &NeverConnectMcp,
                &mut on_text_delta,
            )
            .await
    };

    match result {
        Ok(outcome) => {
            trace.extend(outcome.trace);
            recorder.mark_completed(None, None, trace);
            let final_text = partial_text.lock().unwrap().clone();
            context_manager().record_execution(&session_path, &final_text).await;
            channel.send(ChatEvent::Done { content: final_text }).ok();
            clear_pending(&app);
            mark_unread_unless_focused(&app, UnreadKind::Reply);
        }
        Err(err) => {
            recorder.mark_errored(None, None, trace);
            finish_with_error(&app, &session_path, &channel, &err.to_string());
        }
    }
}

fn finish_with_error(
    app: &AppHandle,
    session_path: &Path,
    channel: &Channel<ChatEvent>,
    message: &str,
) {
    chat_log_store::append_entry(
        session_path,
        &LogEntry { role: LogRole::Error, content: message.to_string(), timestamp: now_rfc3339() },
    );
    channel.send(ChatEvent::Error { message: message.to_string() }).ok();
    clear_pending(app);
    mark_unread_unless_focused(app, UnreadKind::Failure);
}

fn clear_pending(app: &AppHandle) {
    *app.state::<ChatRuntimeState>().pending.lock().unwrap() = None;
    on_chat_activity_changed(app);
}

/// Explicit cancellation (`ai-chat`'s "Explicit stop cancels"). Aborts
/// the task outright -- an aborted task never reaches its own
/// completion code, so nothing is appended to the log for this turn,
/// matching the spec exactly ("no further content is appended").
#[tauri::command]
pub fn stop_generation(app: AppHandle, chat_state: State<ChatRuntimeState>) {
    if let Some(pending) = chat_state.pending.lock().unwrap().take() {
        pending.handle.abort();
    }
    on_chat_activity_changed(&app);
}

/// Starts a fresh session, implicitly cancelling any pending
/// generation first (`ai-chat`'s "New chat cancels a pending
/// generation").
#[tauri::command]
pub fn new_chat_session(app: AppHandle, chat_state: State<ChatRuntimeState>) {
    reset_chat_runtime_state(&app, &chat_state);
    if let Some(new_path) = chat_log_store::create_new_session(&app) {
        *chat_state.session_path.lock().unwrap() = Some(new_path);
    }
}

#[cfg(test)]
mod tests {
    use fleet_snowfluff_ai::{AuthMethod, ProviderKind};

    use super::*;

    fn claude() -> ProfileKey {
        ProfileKey { provider: ProviderKind::Anthropic, auth_method: AuthMethod::Subscription }
    }

    /// A log path in a fresh temp dir -- `name` keeps concurrent tests apart.
    fn log_path(name: &str, file: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-chat-commands-test-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(file)
    }

    fn conversation(path: &Path) -> ConversationId { ConversationId::from_session_path(path) }

    fn turns(n: usize) -> Vec<Message> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(format!("q{i}"))
                } else {
                    Message::assistant(format!("a{i}"))
                }
            })
            .collect()
    }

    /// What `send_chat_message` / `run_generation_fallback` hand a CLI
    /// provider.
    fn ctx(state: &ChatRuntimeState, log: &Path, history: Vec<Message>, cwd: &str) -> CliContext {
        state.cli_context(log, &conversation(log), claude(), history, PathBuf::from(cwd))
    }

    /// A trivial `AiProvider` double whose only real behavior is
    /// `session_id()` -- everything `record_turn_outcome` reads a
    /// provider for. `chat`/`list_models` are never called by these
    /// tests.
    struct FixedSessionProvider(Option<String>);

    #[async_trait::async_trait]
    impl AiProvider for FixedSessionProvider {
        fn kind(&self) -> fleet_snowfluff_ai::ProviderKind {
            fleet_snowfluff_ai::ProviderKind::Mock
        }

        async fn chat(
            &self,
            _messages: Vec<Message>,
        ) -> Result<ChatStream, fleet_snowfluff_ai::ProviderError> {
            unimplemented!("not exercised by these tests")
        }

        async fn list_models(
            &self,
        ) -> Result<Vec<fleet_snowfluff_ai::ModelInfo>, fleet_snowfluff_ai::ProviderError> {
            unimplemented!("not exercised by these tests")
        }

        fn session_id(&self) -> Option<String> { self.0.clone() }
    }

    /// Records a turn that *completed*, via the same `record_turn_outcome`
    /// path a real `run_generation`/`stream_to_completion` call uses. The
    /// resulting `seen_turns` is `history_len + 2` (the history plus this
    /// turn's own user message and reply), matching
    /// `record_turn_outcome`'s own formula -- so a test wanting a specific
    /// `seen_turns` picks `history_len` accordingly (`seen_turns - 2`).
    fn record_completed(
        state: &ChatRuntimeState,
        log: &Path,
        id: &str,
        cwd: &str,
        history_len: usize,
    ) {
        let mut turn = TurnCtx {
            session_path: log.to_path_buf(),
            working_dir: PathBuf::from(cwd),
            history_len,
            prior_seen_turns: 0,
            recorder: ExecutionRecorder::start(
                log.to_path_buf(),
                conversation(log),
                ExecutionPath::Direct,
                claude(),
                PathBuf::from(cwd),
            ),
        };
        state.record_turn_outcome(
            &mut turn,
            conversation(log),
            claude(),
            &FixedSessionProvider(Some(id.to_string())),
            true,
        );
    }

    /// Records a turn that *errored* after capturing `id` -- `seen_turns`
    /// is `prior_seen_turns` verbatim (what the session already held
    /// before this turn began; the real code reads this from
    /// `CliContext::seen_turns` at turn start), never re-derived from
    /// `history_len`.
    fn record_errored(
        state: &ChatRuntimeState,
        log: &Path,
        id: &str,
        cwd: &str,
        prior_seen_turns: usize,
    ) {
        let mut turn = TurnCtx {
            session_path: log.to_path_buf(),
            working_dir: PathBuf::from(cwd),
            history_len: 0,
            prior_seen_turns,
            recorder: ExecutionRecorder::start(
                log.to_path_buf(),
                conversation(log),
                ExecutionPath::Direct,
                claude(),
                PathBuf::from(cwd),
            ),
        };
        state.record_turn_outcome(
            &mut turn,
            conversation(log),
            claude(),
            &FixedSessionProvider(Some(id.to_string())),
            false,
        );
    }

    #[test]
    fn a_recorded_session_is_resumed_from_the_same_working_directory() {
        let state = ChatRuntimeState::default();
        let log = log_path("same-cwd", "a.jsonl");
        record_completed(&state, &log, "sess-1", "/p", 0);

        let cli = ctx(&state, &log, turns(2), "/p");
        assert_eq!(cli.resume_session_id.as_deref(), Some("sess-1"));
        assert_eq!(cli.seen_turns, 2);
    }

    #[test]
    fn a_recorded_session_is_not_resumed_from_a_different_working_directory() {
        let state = ChatRuntimeState::default();
        let log = log_path("other-cwd", "a.jsonl");
        record_completed(&state, &log, "sess-1", "/p", 0);

        let cli = ctx(&state, &log, turns(2), "/changed");
        assert_eq!(
            cli.resume_session_id, None,
            "project_root changed, so the old session is skipped"
        );
        assert_eq!(cli.seen_turns, 0);
        assert_eq!(
            cli.history.len(),
            2,
            "the history is still supplied, to seed the fresh session"
        );
    }

    #[test]
    fn recording_writes_through_to_the_execution_log() {
        let state = ChatRuntimeState::default();
        let log = log_path("write-through", "a.jsonl");
        record_completed(&state, &log, "sess-1", "/p", 2);

        let found = execution_log_store::latest_usable_session(&log, claude(), Path::new("/p"));
        assert_eq!(found, Some(("sess-1".to_string(), 4)));
    }

    #[test]
    fn a_session_and_its_seen_count_survive_an_app_restart() {
        let log = log_path("restart", "a.jsonl");
        record_completed(&ChatRuntimeState::default(), &log, "sess-1", "/p", 4);

        // A brand-new state, as after a restart: nothing in memory, only the execution
        // log.
        let cli = ctx(&ChatRuntimeState::default(), &log, turns(6), "/p");
        assert_eq!(cli.resume_session_id.as_deref(), Some("sess-1"));
        assert_eq!(cli.seen_turns, 6);
    }

    #[test]
    fn a_restarted_session_from_a_different_working_directory_is_skipped() {
        let log = log_path("restart-cwd", "a.jsonl");
        record_completed(&ChatRuntimeState::default(), &log, "sess-1", "/p", 0);

        let cli = ctx(&ChatRuntimeState::default(), &log, turns(2), "/other");
        assert_eq!(cli.resume_session_id, None);
    }

    #[test]
    fn a_new_conversation_never_sees_the_previous_conversations_session() {
        let old_log = log_path("new-chat", "old.jsonl");
        let state = ChatRuntimeState::default();
        record_completed(&state, &old_log, "old-session", "/p", 0);

        // What "New Chat" produces: a different log path, hence a different
        // conversation id and a different (absent) execution log.
        let new_log = old_log.with_file_name("new.jsonl");
        assert_eq!(ctx(&state, &new_log, vec![], "/p").resume_session_id, None);
    }

    #[test]
    fn a_new_session_id_replaces_the_stored_one_for_the_same_profile() {
        let state = ChatRuntimeState::default();
        let log = log_path("replace", "a.jsonl");
        record_completed(&state, &log, "stale", "/p", 0);
        // The provider fell back to a fresh session and captured a new id.
        record_completed(&state, &log, "fresh", "/p", 2);

        let cli = ctx(&ChatRuntimeState::default(), &log, turns(4), "/p");
        assert_eq!(cli.resume_session_id.as_deref(), Some("fresh"));
        assert_eq!(cli.seen_turns, 4);
    }

    // -- what a CLI is told when other providers answered turns in between --

    #[test]
    fn turns_answered_elsewhere_show_up_as_unseen_when_the_session_is_resumed() {
        let state = ChatRuntimeState::default();
        let log = log_path("catch-up", "a.jsonl");
        // Claude's session holds the first two messages...
        record_completed(&state, &log, "sess-1", "/p", 0);
        // ...then Ollama answered another exchange, so the transcript is now 4 long.
        let cli = ctx(&state, &log, turns(4), "/p");

        assert_eq!(cli.resume_session_id.as_deref(), Some("sess-1"));
        assert_eq!(cli.seen_turns, 2, "the session has seen only the first two of four");
        assert_eq!(cli.history.len(), 4);
    }

    #[test]
    fn an_escalation_target_with_no_session_is_handed_the_whole_history() {
        // `run_generation_fallback` builds its context through this same
        // method, from `FallbackAttempt.history`: Ollama answered turns 1-2,
        // and the CLI has never been used in this conversation.
        let state = ChatRuntimeState::default();
        let log = log_path("escalation", "a.jsonl");
        let cli = ctx(&state, &log, turns(2), "/p");

        assert_eq!(cli.resume_session_id, None, "nothing to resume, so the provider will seed");
        assert_eq!(cli.history, turns(2));
        assert_eq!(cli.working_dir, Path::new("/p"));
    }

    // -- how many turns a session is recorded as having seen --

    #[test]
    fn a_failed_turn_keeps_the_seen_count_already_stored_for_that_session() {
        let state = ChatRuntimeState::default();
        let log = log_path("failed-same", "a.jsonl");
        record_completed(&state, &log, "sess-1", "/p", 2);
        // A later turn failed (e.g. a rate limit) after the session id was
        // captured -- it had already seen 4 turns when this one began.
        record_errored(&state, &log, "sess-1", "/p", 4);

        assert_eq!(
            ctx(&state, &log, turns(8), "/p").seen_turns,
            4,
            "not advanced past what is known"
        );
        assert_eq!(
            execution_log_store::latest_usable_session(&log, claude(), Path::new("/p")),
            Some(("sess-1".to_string(), 4))
        );
    }

    #[test]
    fn a_failed_turn_on_a_new_session_starts_from_zero() {
        let state = ChatRuntimeState::default();
        let log = log_path("failed-new", "a.jsonl");
        record_completed(&state, &log, "old", "/p", 2);
        // The provider fell back to a fresh session, then failed mid-reply.
        // A genuinely fresh session's own prior cursor is 0, never borrowed
        // from the unrelated "old" session it replaced.
        record_errored(&state, &log, "new", "/p", 0);

        assert_eq!(
            ctx(&state, &log, turns(6), "/p").seen_turns,
            0,
            "over-counting would skip turns"
        );
    }

    // -- chat readiness, incl. a pending disclosure (api-key-tool-calling) --

    fn settings_with_default(profile: ProviderProfile, acknowledged: bool) -> AiSettings {
        let key = profile.key();
        AiSettings {
            ai_enabled: true,
            enabled_profiles: vec![profile],
            default_profile: Some(key),
            acknowledged_disclosures: if acknowledged { vec![key] } else { vec![] },
            ..AiSettings::default()
        }
    }

    fn api_key_profile() -> ProviderProfile {
        ProviderProfile {
            provider: ProviderKind::OpenAi,
            auth_method: AuthMethod::ApiKey,
            model: None,
            base_url: None,
        }
    }

    #[test]
    fn chat_is_not_ready_while_ai_is_disabled() {
        let settings = AiSettings { ai_enabled: false, ..AiSettings::default() };
        assert_eq!(chat_readiness(&settings), (false, Some("disabled")));
    }

    #[test]
    fn chat_is_not_ready_without_a_default_provider() {
        let settings = AiSettings { ai_enabled: true, ..AiSettings::default() };
        assert_eq!(chat_readiness(&settings), (false, Some("no_provider")));
    }

    #[test]
    fn an_enabled_default_profile_with_a_pending_disclosure_blocks_chat() {
        // The state a one-time re-acknowledgement creates: enabled, default, not
        // acknowledged.
        let settings = settings_with_default(api_key_profile(), false);
        assert_eq!(chat_readiness(&settings), (false, Some("disclosure_pending")));
    }

    #[test]
    fn chat_is_ready_once_the_disclosure_is_acknowledged() {
        let settings = settings_with_default(api_key_profile(), true);
        assert_eq!(chat_readiness(&settings), (true, None));
    }

    #[test]
    fn a_local_default_profile_never_needs_a_disclosure() {
        let ollama = ProviderProfile {
            provider: ProviderKind::Ollama,
            auth_method: AuthMethod::Local,
            model: None,
            base_url: None,
        };
        assert_eq!(chat_readiness(&settings_with_default(ollama, false)), (true, None));
    }

    // `open_conversation`/`new_chat_session` themselves take an
    // `AppHandle`, which cannot be constructed in a unit test (no
    // existing test in this module exercises a Tauri command directly,
    // for the same reason) -- `ChatRuntimeState::reset_for_new_conversation`
    // is where the actually-testable behavior lives, so these two tests
    // exercise that directly, matching task 6.3's "cancels it first" /
    // "resulting session_path matches" requirements one level down.

    #[tokio::test]
    async fn resetting_for_a_new_conversation_cancels_a_pending_generation() {
        let state = ChatRuntimeState::default();
        let handle = tokio::spawn(futures_util::future::pending::<()>());
        *state.pending.lock().unwrap() =
            Some(PendingGeneration { handle, partial_text: Arc::new(Mutex::new(String::new())) });

        let cancelled = state.reset_for_new_conversation();

        assert!(cancelled, "a pending generation must be reported as cancelled");
        assert!(state.pending.lock().unwrap().is_none());
    }

    #[test]
    fn resetting_with_nothing_pending_reports_no_cancellation() {
        let state = ChatRuntimeState::default();
        assert!(!state.reset_for_new_conversation());
    }

    #[test]
    fn opening_a_conversation_points_session_path_at_what_was_requested() {
        let state = ChatRuntimeState::default();
        state.reset_for_new_conversation();
        let requested = PathBuf::from("some-other-conversation.jsonl");
        *state.session_path.lock().unwrap() = Some(requested.clone());

        assert_eq!(state.session_path.lock().unwrap().as_ref(), Some(&requested));
    }

    fn hi_entry() -> LogEntry {
        LogEntry { role: LogRole::User, content: "hi".to_string(), timestamp: now_rfc3339() }
    }

    #[test]
    fn deleting_a_non_open_conversation_removes_both_files() {
        let transcript = log_path("delete-non-open", "session.jsonl");
        chat_log_store::append_entry(&transcript, &hi_entry());
        let executions = execution_log_store::log_path(&transcript);
        std::fs::write(&executions, "{}\n").unwrap();

        let deleted =
            delete_conversation_files(Some(Path::new("some-other-open.jsonl")), &transcript);

        assert!(deleted);
        assert!(!transcript.exists());
        assert!(!executions.exists());
    }

    #[test]
    fn deleting_the_open_conversation_is_refused_and_leaves_both_files() {
        let transcript = log_path("delete-open", "session.jsonl");
        chat_log_store::append_entry(&transcript, &hi_entry());
        let executions = execution_log_store::log_path(&transcript);
        std::fs::write(&executions, "{}\n").unwrap();

        let deleted = delete_conversation_files(Some(transcript.as_path()), &transcript);

        assert!(!deleted);
        assert!(transcript.exists());
        assert!(executions.exists());
    }

    #[test]
    fn deleting_a_conversation_with_only_a_transcript_does_not_error() {
        let transcript = log_path("delete-transcript-only", "session.jsonl");
        chat_log_store::append_entry(&transcript, &hi_entry());
        // No .executions.jsonl ever written -- a conversation that never
        // had a tool-calling turn.

        let deleted = delete_conversation_files(None, &transcript);

        assert!(deleted);
        assert!(!transcript.exists());
    }

    #[test]
    fn native_tool_registry_offers_delegate_task_alongside_the_five_native_tools() {
        let registry = ToolRegistry::new(native_tool_list());
        let names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names.len(), 7, "5 native tools, plus delegate_task and connect_mcp_server");
        assert!(names.contains(&"delegate_task".to_string()));
        assert!(registry.find("delegate_task").is_some());
        assert!(names.contains(&"connect_mcp_server".to_string()));
        assert!(registry.find("connect_mcp_server").is_some());
    }

    #[test]
    fn mix_local_tool_registry_excludes_filesystem_tools_without_a_project_root() {
        let registry = mix_local_tool_registry(None);
        let names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"get_system_context".to_string()));
        assert!(registry.find("read_file").is_none());
        assert!(registry.find("list_directory").is_none());
        assert!(registry.find("run_command").is_none());
        assert!(registry.find("delegate_task").is_none());
        assert!(
            registry.find("connect_mcp_server").is_none(),
            "mix-mode's own local attempt must never offer connect_mcp_server (4.6)"
        );
    }

    #[test]
    fn mix_local_tool_registry_includes_filesystem_tools_with_a_project_root() {
        let registry = mix_local_tool_registry(Some(Path::new("/some/project")));
        let names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names.len(), 4);
        assert!(registry.find("read_file").is_some());
        assert!(registry.find("list_directory").is_some());
        assert!(registry.find("web_search").is_some());
        assert!(registry.find("get_system_context").is_some());
        assert!(registry.find("run_command").is_none());
        assert!(registry.find("delegate_task").is_none());
        assert!(
            registry.find("connect_mcp_server").is_none(),
            "mix-mode's own local attempt must never offer connect_mcp_server (4.6)"
        );
    }

    /// Builds a `ToolCallStream` yielding exactly `items`, in order --
    /// no `AppHandle` or real provider involved, matching
    /// `agent_runtime.rs`'s own `ScriptedProvider` test pattern.
    fn scripted_stream(
        items: Vec<Result<ToolCallStreamItem, fleet_snowfluff_ai::ProviderError>>,
    ) -> ToolCallStream {
        Box::pin(futures_util::stream::iter(items))
    }

    fn delta(text: &str) -> Result<ToolCallStreamItem, fleet_snowfluff_ai::ProviderError> {
        Ok(ToolCallStreamItem::TextDelta(text.to_string()))
    }

    fn call(id: &str, name: &str, arguments: serde_json::Value) -> PendingToolCall {
        PendingToolCall { id: id.to_string(), name: name.to_string(), arguments }
    }

    // -- mix-mode-local-tools: read_mix_local_first_iteration --

    #[tokio::test]
    async fn a_stream_matching_the_marker_with_no_tool_calls_resolves_escalate() {
        let mut stream = scripted_stream(vec![delta("<<ESCALATE>>")]);
        let outcome = read_mix_local_first_iteration(&mut stream).await;
        assert_eq!(outcome, MixLocalFirstIteration::Escalate);
    }

    #[tokio::test]
    async fn a_stream_that_diverges_with_no_tool_calls_resolves_simple() {
        let mut stream = scripted_stream(vec![delta("Rust ownership "), delta("means...")]);
        let outcome = read_mix_local_first_iteration(&mut stream).await;
        assert_eq!(
            outcome,
            MixLocalFirstIteration::Simple { text: "Rust ownership means...".to_string() }
        );
    }

    #[tokio::test]
    async fn a_tool_call_with_no_preceding_text_is_pre_commitment() {
        let mut stream = scripted_stream(vec![Ok(ToolCallStreamItem::ToolCall {
            id: "call_0".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "Cargo.toml"}),
        })]);
        let outcome = read_mix_local_first_iteration(&mut stream).await;
        match outcome {
            MixLocalFirstIteration::Committed { pre_commitment, calls, text } => {
                assert!(pre_commitment, "no text preceded the call -- this is the first action");
                assert_eq!(calls.len(), 1);
                assert_eq!(text, "");
            }
            other => panic!("expected Committed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_tool_call_after_committing_text_is_not_pre_commitment() {
        let mut stream = scripted_stream(vec![
            delta("Sure, let me check that file. "),
            Ok(ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "Cargo.toml"}),
            }),
        ]);
        let outcome = read_mix_local_first_iteration(&mut stream).await;
        match outcome {
            MixLocalFirstIteration::Committed { pre_commitment, .. } => {
                assert!(!pre_commitment, "text already diverged before the call arrived");
            }
            other => panic!("expected Committed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_mid_stream_provider_error_is_reported_as_such() {
        let mut stream = scripted_stream(vec![
            delta("partial"),
            Err(fleet_snowfluff_ai::ProviderError::Network("connection reset".to_string())),
        ]);
        let outcome = read_mix_local_first_iteration(&mut stream).await;
        assert_eq!(outcome, MixLocalFirstIteration::ProviderError);
    }

    #[tokio::test]
    async fn a_stream_ending_still_undecided_with_no_calls_resolves_simple() {
        // Shorter than the marker and never diverges -- can't be
        // escalating if it never finished saying the marker.
        let mut stream = scripted_stream(vec![delta("<<ESC")]);
        let outcome = read_mix_local_first_iteration(&mut stream).await;
        assert_eq!(outcome, MixLocalFirstIteration::Simple { text: "<<ESC".to_string() });
    }

    // -- mix-mode-local-tools: classify_mix_local_calls --

    /// `required_permission_for_path`'s containment check canonicalizes
    /// both the root and the candidate path, which requires both to
    /// actually exist on disk -- a real temp dir with a real file in
    /// it, not a fictitious path, same reasoning as this module's own
    /// `log_path` helper.
    fn project_root_with_a_file(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir()
            .join(format!("fleet-snowfluff-chat-commands-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("Cargo.toml");
        std::fs::write(&file, "[package]").unwrap();
        (root, file)
    }

    #[test]
    fn an_in_root_read_file_call_classifies_as_auto() {
        let (root, file) = project_root_with_a_file("in-root");
        let registry = mix_local_tool_registry(Some(&root));
        let ctx = ToolContext {
            project_root: Some(root.clone()),
            conversation_id: conversation(&root.join("session.jsonl")),
        };
        let plans = classify_mix_local_calls(
            vec![call("call_0", "read_file", serde_json::json!({"path": file.to_str().unwrap()}))],
            &registry,
            &ctx,
        );
        assert_eq!(plans.len(), 1);
        assert!(plans[0].1.is_some());
        assert_eq!(plans[0].2, PermissionTier::Auto);
    }

    #[test]
    fn an_out_of_root_read_file_call_classifies_as_confirm() {
        let (root, _file) = project_root_with_a_file("out-of-root");
        let registry = mix_local_tool_registry(Some(&root));
        let ctx = ToolContext {
            project_root: Some(root.clone()),
            conversation_id: conversation(&root.join("session.jsonl")),
        };
        let plans = classify_mix_local_calls(
            vec![call("call_0", "read_file", serde_json::json!({"path": "/etc/passwd"}))],
            &registry,
            &ctx,
        );
        assert_eq!(plans.len(), 1);
        assert!(plans[0].1.is_some());
        assert_eq!(plans[0].2, PermissionTier::Confirm);
    }

    #[test]
    fn an_unknown_tool_name_classifies_as_deny_with_no_tool() {
        let registry = mix_local_tool_registry(None);
        let ctx = ToolContext {
            project_root: None,
            conversation_id: conversation(Path::new("/project/session.jsonl")),
        };
        let plans = classify_mix_local_calls(
            vec![call("call_0", "run_command", serde_json::json!({"command": "ls"}))],
            &registry,
            &ctx,
        );
        assert_eq!(plans.len(), 1);
        assert!(plans[0].1.is_none());
        assert_eq!(plans[0].2, PermissionTier::Deny);
    }
}
