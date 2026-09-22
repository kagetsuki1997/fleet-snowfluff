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
    AgentRuntime, AiProvider, AiSettings, AuthMethod, ChatStream, DefaultTaskRouter,
    EscalationDecision, GetSystemContextTool, Language, ListDirectoryTool, LogEntry, Message,
    Persona, ProfileKey, ProviderCredentials, ProviderKind, ProviderProfile, ReadFileTool,
    ResponseLanguage, RoutingContext, RunCommandTool, Task, TaskRouter, TaskRouterMode,
    ToolCallingProvider, ToolContext, ToolRegistry, WebSearchTool,
};
use futures_util::StreamExt;
use tauri::{ipc::Channel, AppHandle, Manager, State};

use crate::{
    ai_commands::{self, RoutedExecution},
    chat_log_store, chat_pause, chat_window,
    manager::PetManager,
    persona_store,
    session_domain::{ConversationId, ExecutionId, ExternalSessionRef},
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
    /// subscription providers"). Runtime-only, never written to disk
    /// (design.md's "session id is runtime-only" decision) -- an app
    /// restart just starts fresh sessions, same as `new_chat_session`
    /// already clears this map for a new Aemeath chat session. Keyed by
    /// `ConversationId` as well as `ProfileKey`, not `ProfileKey` alone
    /// -- Aemeath has exactly one conversation at a time today, so this
    /// makes no observable difference yet, but a `ProfileKey`-only key
    /// would silently hand one conversation's session to another's
    /// request for the same profile the moment that's no longer true
    /// (see design.md's Decisions for why this is treated differently
    /// from `PendingGeneration`, which is deliberately left alone).
    cli_sessions: Mutex<HashMap<(ConversationId, ProfileKey), ExternalSessionRef>>,
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
            credentials: creds_snapshot.clone(),
            project_root: settings_snapshot.project_root.clone(),
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
        let resume_session_id = chat_state
            .cli_sessions
            .lock()
            .unwrap()
            .get(&(conversation_id.clone(), profile_key))
            .map(|r| r.0.clone());
        let project_root = settings_snapshot.project_root.clone();
        tokio::spawn(async move {
            run_generation_routed(
                task_app,
                creds_snapshot,
                default_profile,
                resume_session_id,
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
    credentials: ProviderCredentials,
    project_root: Option<PathBuf>,
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
    let resume_session_id = app
        .state::<ChatRuntimeState>()
        .cli_sessions
        .lock()
        .unwrap()
        .get(&(conversation_id.clone(), profile_key))
        .map(|r| r.0.clone());
    run_generation_routed(
        app,
        fallback.credentials,
        fallback.profile,
        resume_session_id,
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
    project_root: Option<PathBuf>,
    execution_id: ExecutionId,
    conversation_id: ConversationId,
    messages: Vec<Message>,
    channel: Channel<ChatEvent>,
    partial_text: Arc<Mutex<String>>,
    session_path: PathBuf,
) {
    let profile_key = profile.key();
    match ai_commands::route_provider(&credentials, &profile, resume_session_id) {
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
    conversation_id: ConversationId,
    profile_key: ProfileKey,
    provider: &dyn AiProvider,
) {
    if let Some(id) = provider.session_id() {
        app.state::<ChatRuntimeState>()
            .cli_sessions
            .lock()
            .unwrap()
            .insert((conversation_id, profile_key), ExternalSessionRef(id));
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
) {
    log::debug!("{execution_id:?} starting for {profile_key:?}");

    let mut stream = match provider.chat(messages).await {
        Ok(stream) => stream,
        Err(err) => {
            store_cli_session_id(&app, conversation_id, profile_key, provider.as_ref());
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
) {
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                partial_text.lock().unwrap().push_str(&chunk.delta);
                channel.send(ChatEvent::Chunk { delta: chunk.delta }).ok();
            }
            Err(err) => {
                store_cli_session_id(&app, conversation_id, profile_key, provider.as_ref());
                finish_with_error(&app, &session_path, &channel, &err.to_string());
                return;
            }
        }
    }

    store_cli_session_id(&app, conversation_id, profile_key, provider.as_ref());
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
    let local_provider = ai_commands::build_provider(&credentials, &local_profile, None);
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
