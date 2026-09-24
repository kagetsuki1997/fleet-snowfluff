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
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use fleet_snowfluff_ai::{
    detect_escalation, log::LogRole, prompt, with_task_router_rules, AemeathAgentRuntime,
    AgentRuntime, AiProvider, AiSettings, AuthMethod, ChatStream, ClaudeCodeToolAccess,
    DefaultTaskRouter, EscalationDecision, GetSystemContextTool, Language, ListDirectoryTool,
    LogEntry, Message, Persona, ProfileKey, ProviderCredentials, ProviderKind, ProviderProfile,
    ReadFileTool, ResponseLanguage, RoutingContext, RunCommandTool, Task, TaskRouter,
    TaskRouterMode, ToolCallingProvider, ToolContext, ToolRegistry, WebSearchTool,
};
use futures_util::StreamExt;
use tauri::{ipc::Channel, AppHandle, Manager, State};

use crate::{
    ai_commands::{self, RoutedExecution},
    chat_log_store, chat_pause, chat_window, cli_session_store, cli_workdir,
    manager::PetManager,
    persona_store,
    session_domain::{CliSessionEntry, ConversationId, ExecutionId, ExternalSessionRef},
    status_bubble, task_router_rules_store,
};

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
    /// also written through to the conversation's sidecar file
    /// (`cli_session_store`), which is read back lazily on a miss, so a
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

    /// The CLI session to resume for `(conversation, profile)` when the
    /// CLI is about to run in `working_dir`, or `None` to start a fresh
    /// (history-seeded) one. The one lookup used by every path that
    /// spawns a CLI (`send_chat_message`'s direct branch and
    /// `run_generation_fallback`): checks the in-memory map first, and
    /// on a miss reads the conversation's sidecar, so a session
    /// survives an app restart. A session created under a different
    /// working directory is never returned.
    pub fn resume_session_id(
        &self,
        session_path: &Path,
        conversation_id: &ConversationId,
        profile_key: ProfileKey,
        working_dir: &Path,
    ) -> Option<String> {
        let key = (conversation_id.clone(), profile_key);
        let mut sessions = self.cli_sessions.lock().unwrap();
        if let Some(entry) = sessions.get(&key) {
            return (entry.cwd == working_dir).then(|| entry.session.0.clone());
        }
        let stored = cli_session_store::load(session_path);
        let found = cli_session_store::usable(&stored, profile_key, working_dir)?;
        sessions.insert(
            key,
            CliSessionEntry {
                session: ExternalSessionRef(found.session_id.clone()),
                cwd: found.cwd.clone(),
            },
        );
        Some(found.session_id.clone())
    }

    /// Remembers `session_id` for `(conversation, profile)` in memory and
    /// writes it through to the conversation's sidecar, recording the
    /// working directory it was created under.
    pub fn record_session(
        &self,
        session_path: &Path,
        conversation_id: ConversationId,
        profile_key: ProfileKey,
        session_id: String,
        working_dir: &Path,
    ) {
        cli_session_store::upsert(
            session_path,
            cli_session_store::StoredSession {
                profile: profile_key,
                session_id: session_id.clone(),
                cwd: working_dir.to_path_buf(),
            },
        );
        self.cli_sessions.lock().unwrap().insert(
            (conversation_id, profile_key),
            CliSessionEntry {
                session: ExternalSessionRef(session_id),
                cwd: working_dir.to_path_buf(),
            },
        );
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
    let (ai_ready, not_ready_reason) = if !settings.ai_enabled {
        (false, Some("disabled"))
    } else if settings.default_profile.is_none() {
        (false, Some("no_provider"))
    } else {
        (true, None)
    };

    ChatStateSnapshot { entries, is_pending, partial_text, ai_ready, not_ready_reason }
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
    let cli_working_dir = cli_workdir::for_app(&app, settings_snapshot.project_root.as_deref());

    let session_path = resolve_session_path(&app, &chat_state);
    let conversation_id = ConversationId::from_session_path(&session_path);
    let execution_id = ExecutionId::new();
    let history_entries = chat_log_store::read_session(&session_path);
    let context = fleet_snowfluff_ai::log::entries_to_context(&history_entries);

    // Appended immediately -- part of history regardless of what
    // happens to the reply.
    chat_log_store::append_entry(
        &session_path,
        &LogEntry { role: LogRole::User, content: message.clone(), timestamp: now_rfc3339() },
    );

    let persona = persona_store::load(&app).persona;
    let detected_language = map_ui_language(manager.lock().unwrap().ui_language());
    let language = resolve_language(&persona, detected_language);
    // Persona-only message list -- what `default_profile` always uses,
    // whether that's because `mode` is `single`, because `mix` resolved
    // directly to it (Ollama not enabled), or because it's serving as
    // the fallback for a mix-mode local attempt that escalates or fails
    // below. Never has `task-router-rules.md` appended -- only the
    // local attempt's own message list does.
    let default_messages = prompt::assemble_messages(&persona, language, &context, &message);

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
        let resume_session_id = chat_state.resume_session_id(
            &session_path,
            &conversation_id,
            profile_key,
            &cli_working_dir,
        );
        let project_root = settings_snapshot.project_root.clone();
        let claude_code_tool_access = settings_snapshot.claude_code_tool_access;
        tokio::spawn(async move {
            run_generation_routed(
                task_app,
                creds_snapshot,
                default_profile,
                resume_session_id,
                context,
                cli_working_dir,
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
    let resume_session_id = app.state::<ChatRuntimeState>().resume_session_id(
        &session_path,
        &conversation_id,
        profile_key,
        &fallback.working_dir,
    );
    run_generation_routed(
        app,
        fallback.credentials,
        fallback.profile,
        resume_session_id,
        fallback.history,
        fallback.working_dir,
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
/// above). Deliberately not applied inside `run_generation_mix_local`'s
/// own local-classification attempt: that's a separate, text-only
/// escalation-detection mechanism (`task-router-rules.md`,
/// `<<ESCALATE>>` detection) that doesn't yet interact with tool
/// calling -- see design.md for why combining the two is future work,
/// not resolved here.
#[allow(clippy::too_many_arguments)]
async fn run_generation_routed(
    app: AppHandle,
    credentials: ProviderCredentials,
    profile: ProviderProfile,
    resume_session_id: Option<String>,
    history: Vec<Message>,
    working_dir: PathBuf,
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
    match ai_commands::route_provider(
        &credentials,
        &profile,
        resume_session_id,
        history,
        working_dir.clone(),
        &claude_code_tool_access,
    ) {
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
                session_path,
                working_dir,
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
                session_path,
            )
            .await;
        }
    }
}

/// Persists `provider`'s captured session/thread id (if any) for
/// `(conversation_id, profile_key)`, so the *next* `send_chat_message`
/// for the same conversation and profile can resume it. A no-op for
/// every provider without a resumable session concept
/// (`AiProvider::session_id`'s default `None`), and a no-op if this
/// call captured nothing (e.g. it failed before a CLI session was ever
/// established) -- a prior mapping, if any, is left untouched rather
/// than cleared.
fn store_cli_session_id(
    app: &AppHandle,
    session_path: &Path,
    working_dir: &Path,
    conversation_id: ConversationId,
    profile_key: ProfileKey,
    provider: &dyn AiProvider,
) {
    if let Some(id) = provider.session_id() {
        app.state::<ChatRuntimeState>().record_session(
            session_path,
            conversation_id,
            profile_key,
            id,
            working_dir,
        );
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
    session_path: PathBuf,
    working_dir: PathBuf,
) {
    log::debug!("{execution_id:?} starting for {profile_key:?}");

    let mut stream = match provider.chat(messages).await {
        Ok(stream) => stream,
        Err(err) => {
            store_cli_session_id(
                &app,
                &session_path,
                &working_dir,
                conversation_id,
                profile_key,
                provider.as_ref(),
            );
            finish_with_error(&app, &session_path, &channel, &err.to_string());
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
        session_path,
        working_dir,
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
    session_path: PathBuf,
    working_dir: PathBuf,
) {
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                partial_text.lock().unwrap().push_str(&chunk.delta);
                channel.send(ChatEvent::Chunk { delta: chunk.delta }).ok();
            }
            Err(err) => {
                store_cli_session_id(
                    &app,
                    &session_path,
                    &working_dir,
                    conversation_id,
                    profile_key,
                    provider.as_ref(),
                );
                finish_with_error(&app, &session_path, &channel, &err.to_string());
                return;
            }
        }
    }

    store_cli_session_id(
        &app,
        &session_path,
        &working_dir,
        conversation_id,
        profile_key,
        provider.as_ref(),
    );
    let final_text = partial_text.lock().unwrap().clone();
    chat_log_store::append_entry(
        &session_path,
        &LogEntry {
            role: LogRole::Assistant,
            content: final_text.clone(),
            timestamp: now_rfc3339(),
        },
    );
    channel.send(ChatEvent::Done { content: final_text }).ok();
    clear_pending(&app);
    mark_unread_unless_focused(&app, UnreadKind::Reply);
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
/// stream, rather than `AgentRuntime::run`'s own `Ok(String)` return
/// value alone (which is only the *last* iteration's text).
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
    session_path: PathBuf,
) {
    log::debug!("{execution_id:?} starting tool-calling generation for {profile_key:?}");

    // No `store_cli_session_id` call here: `route_provider` only ever
    // resolves `ToolCapable` for `(Ollama, Local)`, and Ollama has no
    // resumable-session concept (`AiProvider::session_id`'s default
    // `None`, never overridden -- same reasoning as
    // `run_generation_mix_local`'s own local attempt). Revisit if a
    // future `ToolCapable` provider ever does have one.
    let ctx = ToolContext { project_root, conversation_id: conversation_id.clone() };
    let registry = ToolRegistry::new(vec![
        Arc::new(WebSearchTool::default()),
        Arc::new(ReadFileTool),
        Arc::new(ListDirectoryTool),
        Arc::new(RunCommandTool::default()),
        Arc::new(GetSystemContextTool),
    ]);
    let runtime = AemeathAgentRuntime::default();
    let permission = crate::tool_confirmation::PopupPermissionDecider {
        app: app.clone(),
        conversation_id: conversation_id.clone(),
        registry: &registry,
    };

    let result = {
        let mut on_text_delta = |delta: &str| {
            partial_text.lock().unwrap().push_str(delta);
            channel.send(ChatEvent::Chunk { delta: delta.to_string() }).ok();
        };
        runtime
            .run(provider.as_ref(), messages, &registry, &ctx, &permission, &mut on_text_delta)
            .await
    };

    match result {
        Ok(_) => {
            let final_text = partial_text.lock().unwrap().clone();
            chat_log_store::append_entry(
                &session_path,
                &LogEntry {
                    role: LogRole::Assistant,
                    content: final_text.clone(),
                    timestamp: now_rfc3339(),
                },
            );
            channel.send(ChatEvent::Done { content: final_text }).ok();
            clear_pending(&app);
            mark_unread_unless_focused(&app, UnreadKind::Reply);
        }
        Err(err) => finish_with_error(&app, &session_path, &channel, &err.to_string()),
    }
}

/// `mode: mix`'s local-first attempt (`agent-core-and-task-router`'s
/// "Local-first classification in mixed mode"). Buffers the local
/// provider's own reply through [`detect_escalation`] *before*
/// forwarding anything to the UI or the chat log:
///
/// - a conclusive [`EscalationDecision::Simple`] (a mismatch, or the stream
///   ending while still a strict prefix of the marker) flushes the buffered
///   prefix and hands the rest of the same stream to [`stream_to_completion`],
///   exactly as if this had been a normal attempt from the start;
/// - [`EscalationDecision::Escalate`], or any failure to even start or continue
///   the local stream, discards everything buffered so far -- nothing shown,
///   nothing logged -- and retries the message against `fallback.profile` via a
///   fresh [`run_generation`] call instead.
///
/// Either way, `TaskRouter::route()` is never called a second time --
/// see `task_router.rs`'s own module doc. Deliberately never routes
/// through `run_generation_with_tools`, even when `local_profile`
/// happens to be `ToolCapable` -- this classification attempt is a
/// separate, text-only escalation-detection mechanism that doesn't yet
/// interact with tool calling (see `run_generation_routed`'s own doc
/// comment).
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

    // No `cli_sessions` lookup here: Ollama has no resumable-session
    // concept (`AiProvider::session_id`'s default `None`, never
    // overridden), so a mix-mode local attempt is always a fresh call.
    // `claude_code_tool_access` is read off `fallback` purely because
    // it's already in scope there -- this call is always Ollama, so the
    // value is provably unused by it (see `route_provider`'s own doc
    // comment).
    let local_provider = ai_commands::build_provider(
        &credentials,
        &local_profile,
        None,
        &fallback.claude_code_tool_access,
    );
    let mut stream = match local_provider.chat(local_messages).await {
        Ok(stream) => stream,
        Err(_) => {
            // Infra-level failure before the local attempt even
            // started (e.g. `RuntimeUnavailable` -- Ollama enabled in
            // settings but not actually reachable). Nothing was shown
            // or logged for this attempt; fall back directly.
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

    let mut buffer = String::new();
    loop {
        let chunk = match stream.next().await {
            Some(Ok(chunk)) => chunk,
            Some(Err(_)) => {
                // Mid-stream infra failure. Whatever's in `buffer` was
                // never shown or logged -- fall back directly.
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
            None => {
                // Stream ended while still deciding (or with nothing
                // ever having diverged) -- can't be escalating if it
                // never finished saying the marker, so this resolves as
                // simple no matter what `detect_escalation` last
                // reported.
                if !buffer.is_empty() {
                    partial_text.lock().unwrap().push_str(&buffer);
                    channel.send(ChatEvent::Chunk { delta: buffer }).ok();
                }
                store_cli_session_id(
                    &app,
                    &session_path,
                    &fallback.working_dir,
                    conversation_id,
                    local_profile_key,
                    local_provider.as_ref(),
                );
                let final_text = partial_text.lock().unwrap().clone();
                chat_log_store::append_entry(
                    &session_path,
                    &LogEntry {
                        role: LogRole::Assistant,
                        content: final_text.clone(),
                        timestamp: now_rfc3339(),
                    },
                );
                channel.send(ChatEvent::Done { content: final_text }).ok();
                clear_pending(&app);
                mark_unread_unless_focused(&app, UnreadKind::Reply);
                return;
            }
        };
        buffer.push_str(&chunk.delta);

        match detect_escalation(&buffer) {
            EscalationDecision::Undecided => continue,
            EscalationDecision::Simple => {
                if !buffer.is_empty() {
                    partial_text.lock().unwrap().push_str(&buffer);
                    channel.send(ChatEvent::Chunk { delta: buffer }).ok();
                }
                stream_to_completion(
                    app,
                    local_provider,
                    &mut stream,
                    conversation_id,
                    local_profile_key,
                    channel,
                    partial_text,
                    session_path,
                    fallback.working_dir.clone(),
                )
                .await;
                return;
            }
            EscalationDecision::Escalate => {
                // Nothing in `buffer` was ever shown or logged. Drop
                // the local stream/provider (dropping a `ChatStream`
                // built over an HTTP response ends that request on its
                // own -- no explicit cancellation needed) and retry
                // fresh against `default_profile`.
                drop(stream);
                drop(local_provider);
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
    if let Some(pending) = chat_state.pending.lock().unwrap().take() {
        pending.handle.abort();
    }
    chat_state.clear_unread();
    chat_state.cli_sessions.lock().unwrap().clear();
    chat_state.remembered_tools.lock().unwrap().clear();
    on_chat_activity_changed(&app);
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

    #[test]
    fn a_recorded_session_is_resumed_from_the_same_working_directory() {
        let state = ChatRuntimeState::default();
        let log = log_path("same-cwd", "a.jsonl");
        state.record_session(&log, conversation(&log), claude(), "sess-1".into(), Path::new("/p"));

        let found = state.resume_session_id(&log, &conversation(&log), claude(), Path::new("/p"));
        assert_eq!(found.as_deref(), Some("sess-1"));
    }

    #[test]
    fn a_recorded_session_is_not_resumed_from_a_different_working_directory() {
        let state = ChatRuntimeState::default();
        let log = log_path("other-cwd", "a.jsonl");
        state.record_session(&log, conversation(&log), claude(), "sess-1".into(), Path::new("/p"));

        let found =
            state.resume_session_id(&log, &conversation(&log), claude(), Path::new("/changed"));
        assert_eq!(found, None, "project_root changed, so the old session must be skipped");
    }

    #[test]
    fn recording_writes_through_to_the_sidecar() {
        let state = ChatRuntimeState::default();
        let log = log_path("write-through", "a.jsonl");
        state.record_session(&log, conversation(&log), claude(), "sess-1".into(), Path::new("/p"));

        let stored = cli_session_store::load(&log);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].session_id, "sess-1");
        assert_eq!(stored[0].cwd, Path::new("/p"));
    }

    #[test]
    fn a_session_survives_an_app_restart() {
        let log = log_path("restart", "a.jsonl");
        ChatRuntimeState::default().record_session(
            &log,
            conversation(&log),
            claude(),
            "sess-1".into(),
            Path::new("/p"),
        );

        // A brand-new state, as after a restart: nothing in memory, only the sidecar.
        let restarted = ChatRuntimeState::default();
        let found =
            restarted.resume_session_id(&log, &conversation(&log), claude(), Path::new("/p"));
        assert_eq!(found.as_deref(), Some("sess-1"));
    }

    #[test]
    fn a_restarted_session_from_a_different_working_directory_is_skipped() {
        let log = log_path("restart-cwd", "a.jsonl");
        ChatRuntimeState::default().record_session(
            &log,
            conversation(&log),
            claude(),
            "sess-1".into(),
            Path::new("/p"),
        );

        let restarted = ChatRuntimeState::default();
        let found =
            restarted.resume_session_id(&log, &conversation(&log), claude(), Path::new("/other"));
        assert_eq!(found, None);
    }

    #[test]
    fn a_new_conversation_never_sees_the_previous_conversations_session() {
        let old_log = log_path("new-chat", "old.jsonl");
        let state = ChatRuntimeState::default();
        state.record_session(
            &old_log,
            conversation(&old_log),
            claude(),
            "old-session".into(),
            Path::new("/p"),
        );

        // What "New Chat" produces: a different log path, hence a different
        // conversation id and a different (absent) sidecar.
        let new_log = old_log.with_file_name("new.jsonl");
        let found =
            state.resume_session_id(&new_log, &conversation(&new_log), claude(), Path::new("/p"));
        assert_eq!(found, None);
    }

    #[test]
    fn a_new_session_id_replaces_the_stored_one_for_the_same_profile() {
        let state = ChatRuntimeState::default();
        let log = log_path("replace", "a.jsonl");
        state.record_session(&log, conversation(&log), claude(), "stale".into(), Path::new("/p"));
        // The provider fell back to a fresh session and captured a new id.
        state.record_session(&log, conversation(&log), claude(), "fresh".into(), Path::new("/p"));

        let restarted = ChatRuntimeState::default();
        let found =
            restarted.resume_session_id(&log, &conversation(&log), claude(), Path::new("/p"));
        assert_eq!(found.as_deref(), Some("fresh"));
    }
}
