//! The `"tool-confirmation"` popup (`agent-core-and-task-router`'s
//! Group 6): batches every `Confirm`-tier tool call from one Agent Loop
//! turn into a single window, blocks the loop on a `oneshot` until the
//! user responds (or the window closes, treated as deny), and folds in
//! the session-scoped "remember" allowlist so a repeat call doesn't
//! need to ask again. Same `WebviewWindowBuilder`/`index.html`-reuse/
//! label-branching pattern as `status_bubble.rs`, but `focused(true)`
//! -- the user must actively respond, unlike the bubble's deliberate
//! `focused(false)`.

use std::{
    collections::{HashSet, VecDeque},
    sync::Mutex,
};

use fleet_snowfluff_ai::{
    build_authorization_url, discover, exchange_code_for_token, generate_pkce, generate_state,
    probe_authorization, register_client_or_explain, AuthProbeOutcome, McpConnectOutcome,
    McpConnectRequest, McpConnector, McpServerConfig, McpServerCredential, McpServerRecord,
    McpServerStatus, McpServerTransportConfig, McpToolSummary, PendingToolCall, PermissionDecider,
    RedirectListener, Tool, ToolRegistry, CONNECT_MCP_SERVER_TOOL_NAME,
};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_shell::ShellExt;
use tokio::sync::oneshot;

use crate::{
    chat_commands::{ChatRuntimeState, RememberKey},
    mcp_connection::{McpConnectionState, PendingOAuthAttempt, PendingOAuthState},
    session_domain::ConversationId,
};

pub const CONFIRMATION_WINDOW_LABEL: &str = "tool-confirmation";
const CONFIRMATION_WIDTH: f64 = 420.0;
const CONFIRMATION_HEIGHT: f64 = 320.0;

/// One tool call the frontend needs to show and let the user decide on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PendingConfirmationItem {
    pub id: String,
    pub tool_name: String,
    pub summary: String,
    pub allows_remember: bool,
}

/// The frontend's response for one item, submitted together as a batch
/// via `resolve_tool_confirmations` (the "Batched confirmation for one
/// turn" requirement -- one window, one submit, not one popup per call).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ToolConfirmationResponse {
    pub id: String,
    pub approved: bool,
    pub remember: bool,
}

struct PendingBatch {
    id: u64,
    items: Vec<PendingConfirmationItem>,
    responder: oneshot::Sender<Vec<ToolConfirmationResponse>>,
}

/// A queue, not a single slot: `sub-agent-delegation` is the first
/// thing to make more than one `AemeathAgentRuntime::run()` call
/// genuinely concurrent (a parent delegating to several children at
/// once), so more than one `confirm_via_popup` call can now be
/// in-flight at the same time. Before that, `ai-chat`'s "Single
/// in-flight generation" guaranteed only one Agent Loop -- and so only
/// one confirmation round -- was ever active, which is the only reason
/// a single slot was ever safe. Each batch is presented in the order it
/// arrived (FIFO); the window stays open across batches rather than
/// closing between them (see `confirm_via_popup`).
#[derive(Default)]
pub struct ToolConfirmationState {
    pending: Mutex<VecDeque<PendingBatch>>,
}

#[tauri::command]
pub fn get_pending_tool_confirmations(
    state: State<ToolConfirmationState>,
) -> Vec<PendingConfirmationItem> {
    state.pending.lock().unwrap().front().map(|batch| batch.items.clone()).unwrap_or_default()
}

#[tauri::command]
pub fn resolve_tool_confirmations(
    state: State<ToolConfirmationState>,
    responses: Vec<ToolConfirmationResponse>,
) {
    if let Some(batch) = state.pending.lock().unwrap().pop_front() {
        batch.responder.send(responses).ok();
    }
}

/// Opens the window fresh, wiring a `Destroyed` handler so closing it
/// without an explicit `resolve_tool_confirmations` call (e.g. the OS
/// close button) still unblocks every waiting Agent Loop, not only
/// whichever batch happened to be shown -- clearing the whole queue
/// drops every `PendingBatch`'s `oneshot::Sender`, which resolves each
/// paired `Receiver` to `Err`, treated by `confirm_via_popup` as an
/// empty response list (deny everything in that batch). Registered
/// once, at creation time, since the handler stays valid for the
/// window's whole lifetime even as it's shown/focused again for later
/// batches.
fn open_or_focus(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(CONFIRMATION_WINDOW_LABEL) {
        window.show().ok();
        window.set_focus().ok();
        return;
    }

    let result = WebviewWindowBuilder::new(
        app,
        CONFIRMATION_WINDOW_LABEL,
        WebviewUrl::App("index.html".into()),
    )
    .inner_size(CONFIRMATION_WIDTH, CONFIRMATION_HEIGHT)
    .resizable(false)
    .focused(true)
    .build();

    match result {
        Ok(window) => {
            let app_handle = app.clone();
            window.on_window_event(move |event| {
                if matches!(event, WindowEvent::Destroyed) {
                    app_handle.state::<ToolConfirmationState>().pending.lock().unwrap().clear();
                }
            });
        }
        Err(err) => log::error!("failed to create tool-confirmation window: {err}"),
    }
}

fn close(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(CONFIRMATION_WINDOW_LABEL) {
        window.close().ok();
    }
}

/// Guarantees this call's own batch is removed from the queue once
/// `confirm_via_popup` is done with it -- whether that's because
/// `resolve_tool_confirmations` already popped it (the normal path;
/// removing it again here is then a harmless no-op), or because the
/// generation that requested it was aborted (Stop) while still
/// awaiting a response. Without this, an aborted generation's batch
/// was never removed -- it isn't a local variable's problem, it was
/// already pushed into `ToolConfirmationState.pending`, global state
/// with no lifetime tied to the task that pushed it -- and sat
/// orphaned in the queue, resurfacing (with a responder nothing will
/// ever read from again) the next time *any* generation needed
/// confirmation, ahead of its own real batch. Same RAII-on-abort
/// pattern as `ExecutionRecorder`: `tokio::task::JoinHandle::abort()`
/// drops the task at its next await point, so this guard's `Drop` is
/// the only code that reliably runs either way. Folds in the
/// queue-is-empty-so-close-the-window check too, so both the normal
/// and aborted paths get exactly one cleanup, not two slightly
/// different ones.
struct PendingBatchGuard {
    app: AppHandle,
    id: u64,
}

impl Drop for PendingBatchGuard {
    fn drop(&mut self) {
        let state = self.app.state::<ToolConfirmationState>();
        let mut pending = state.pending.lock().unwrap();
        pending.retain(|batch| batch.id != self.id);
        let now_empty = pending.is_empty();
        drop(pending);
        if now_empty {
            close(&self.app);
        }
    }
}

/// Opens (or focuses) the confirmation window, enqueues `items` as a
/// new batch, and blocks until `resolve_tool_confirmations` is called
/// for *this* batch specifically (or the window closes without one).
/// Concurrent callers (`sub-agent-delegation`'s own concurrent children)
/// each enqueue their own batch and each await only their own
/// `oneshot::Receiver` -- `resolve_tool_confirmations` always resolves
/// whichever batch is at the front of the queue, so batches are
/// presented in the order they arrived. The window only closes once the
/// queue is empty again; otherwise it stays open for the next batch.
async fn confirm_via_popup(
    app: &AppHandle,
    items: Vec<PendingConfirmationItem>,
) -> Vec<ToolConfirmationResponse> {
    let id: u64 = rand::random();
    let (tx, rx) = oneshot::channel();
    app.state::<ToolConfirmationState>().pending.lock().unwrap().push_back(PendingBatch {
        id,
        items,
        responder: tx,
    });
    open_or_focus(app);
    let _guard = PendingBatchGuard { app: app.clone(), id };
    rx.await.unwrap_or_default()
}

/// A short, human-legible description of `args` for the confirmation
/// list -- the one field a native tool's arguments are actually about
/// (a path, a query, a command), falling back to the raw JSON for
/// anything else rather than showing nothing.
fn summarize_args(args: &Value) -> String {
    for field in ["path", "query", "command"] {
        if let Some(value) = args.get(field).and_then(Value::as_str) {
            return value.to_string();
        }
    }
    args.to_string()
}

fn build_item(call: &PendingToolCall, tool: &dyn Tool) -> PendingConfirmationItem {
    PendingConfirmationItem {
        id: call.id.clone(),
        tool_name: call.name.clone(),
        summary: summarize_args(&call.arguments),
        allows_remember: tool.allows_session_remember(),
    }
}

/// Splits `calls` into what the session-scoped remember allowlist
/// already approves (no popup needed for these at all) and what still
/// needs a real decision -- pure aside from reading `state`, so
/// directly testable without ever touching a window.
fn partition_remembered(
    conversation_id: &ConversationId,
    calls: &[PendingToolCall],
    state: &ChatRuntimeState,
    registry: &ToolRegistry,
) -> (HashSet<String>, Vec<PendingConfirmationItem>) {
    let mut already_approved = HashSet::new();
    let mut needs_decision = Vec::new();
    for call in calls {
        let Some(tool) = registry.find(&call.name) else { continue };
        let key = RememberKey::for_call(&call.name, &call.arguments);
        if state.is_tool_remembered(conversation_id, &key) {
            already_approved.insert(call.id.clone());
        } else {
            needs_decision.push(build_item(call, tool.as_ref()));
        }
    }
    (already_approved, needs_decision)
}

/// The real, popup-backed `PermissionDecider` (tasks 6.2-6.4) --
/// replaces `DenyAllConfirm`, task 6.1's fail-closed placeholder.
/// Borrows `registry` rather than owning it since the caller
/// (`run_generation_with_tools`) already holds one for the whole
/// `AgentRuntime::run` call this decider is used within.
pub struct PopupPermissionDecider<'a> {
    pub app: AppHandle,
    pub conversation_id: ConversationId,
    pub registry: &'a ToolRegistry,
}

#[async_trait::async_trait]
impl PermissionDecider for PopupPermissionDecider<'_> {
    async fn decide(&self, calls: &[PendingToolCall]) -> HashSet<String> {
        let chat_state = self.app.state::<ChatRuntimeState>();
        let (mut approved, needs_decision) =
            partition_remembered(&self.conversation_id, calls, &chat_state, self.registry);
        if needs_decision.is_empty() {
            return approved;
        }

        let responses = confirm_via_popup(&self.app, needs_decision).await;
        for response in responses {
            if !response.approved {
                continue;
            }
            approved.insert(response.id.clone());
            if response.remember {
                if let Some(call) = calls.iter().find(|call| call.id == response.id) {
                    chat_state.remember_tool(
                        self.conversation_id.clone(),
                        RememberKey::for_call(&call.name, &call.arguments),
                    );
                }
            }
        }
        approved
    }
}

/// A short, human-legible summary of what would be connected -- shown
/// in the confirmation popup exactly as `summarize_args` already does
/// for other tool calls, just specialized to this one request shape
/// rather than a generic args-field scan.
fn summarize_connect_request(request: &McpConnectRequest) -> String {
    match request {
        McpConnectRequest::Stdio { command, args, .. } => {
            if args.is_empty() {
                command.clone()
            } else {
                format!("{command} {}", args.join(" "))
            }
        }
        McpConnectRequest::Http { url } => url.clone(),
    }
}

/// There's nothing in the request itself to name a server by other
/// than what it resolves to -- no separate "display name" field exists
/// on `connect_mcp_server`'s own schema, so the resolved command/URL
/// doubles as the name shown everywhere (the popup, the Settings UI,
/// a success message).
fn display_name_for(request: &McpConnectRequest) -> String {
    match request {
        McpConnectRequest::Stdio { command, .. } => command.clone(),
        McpConnectRequest::Http { url } => url.clone(),
    }
}

fn generate_server_id() -> String { format!("mcp-{:x}", rand::random::<u64>()) }

/// Replaces (by id) or appends `record` in the persisted
/// `mcp-servers.json`, then writes it back -- every write to this file
/// goes through here, so "connect", "finish OAuth", and (Group 4.4/4.5)
/// "remove"/"refresh" all share one read-modify-write shape.
fn upsert_server_record(app: &AppHandle, record: McpServerRecord) {
    let mut servers = crate::mcp_servers_store::load(app);
    servers.servers.retain(|existing| existing.config.id != record.config.id);
    servers.servers.push(record);
    crate::mcp_servers_store::save(app, &servers);
}

pub(crate) fn summarize_tools(
    tools: Vec<fleet_snowfluff_ai::McpToolDescriptor>,
) -> Vec<McpToolSummary> {
    tools
        .into_iter()
        .map(|tool| McpToolSummary {
            name: tool.name,
            description: tool.description.unwrap_or_default(),
        })
        .collect()
}

/// Finishes an OAuth flow once a code is in hand -- from the redirect
/// listener catching it (run as a detached background task from
/// `start_oauth_flow`, specifically so nothing in the chat turn that
/// triggered it ever awaits this -- the "does not block the rest of
/// the conversation" scenario), or from a manual paste (Group 5.3,
/// `submit_mcp_oauth_code`). Exchanges the code for a token, stores
/// it, and completes the connection.
pub(crate) async fn finish_oauth(
    app: &AppHandle,
    attempt: PendingOAuthAttempt,
    code: &str,
) -> McpConnectOutcome {
    let token = match exchange_code_for_token(
        &attempt.client,
        &attempt.token_endpoint,
        code,
        &attempt.redirect_uri,
        &attempt.client_id,
        &attempt.code_verifier,
    )
    .await
    {
        Ok(token) => token,
        Err(err) => {
            log::warn!(
                "MCP OAuth token exchange for \"{}\" failed: {err}",
                attempt.config.display_name
            );
            notify_mcp_servers_changed(app);
            return McpConnectOutcome::Failed { reason: err.to_string() };
        }
    };

    let config = attempt.config;
    let mut creds = crate::secrets_store::load(app);
    creds.mcp_server_credentials.insert(
        config.id.clone(),
        McpServerCredential::OAuthToken {
            access_token: token.access_token.clone(),
            refresh_token: token.refresh_token.clone(),
        },
    );
    crate::secrets_store::save(app, &creds);

    let connection_state = app.state::<McpConnectionState>();
    match connection_state.connection_for(&config, Some(&token.access_token)).await {
        Ok(client_conn) => {
            let tools = client_conn.list_tools().await.map(summarize_tools).unwrap_or_default();
            let tool_count = tools.len();
            let display_name = config.display_name.clone();
            upsert_server_record(
                app,
                McpServerRecord {
                    config,
                    status: McpServerStatus::Ready,
                    tools,
                    disabled_tools: std::collections::HashSet::new(),
                },
            );
            notify_mcp_servers_changed(app);
            McpConnectOutcome::Connected { display_name, tool_count }
        }
        Err(err) => {
            log::warn!(
                "MCP connection for \"{}\" failed right after authorization: {err}",
                config.display_name
            );
            notify_mcp_servers_changed(app);
            McpConnectOutcome::Failed { reason: err.to_string() }
        }
    }
}

/// Broadcasts that connected-MCP-server state changed, so an open
/// Settings window re-renders -- needed only for `finish_oauth`'s own
/// *detached* completion path (the redirect listener's background
/// task; design.md's "does not block the rest of the conversation"
/// means nothing is awaiting this to know it's done). Every other
/// mutation (`add_mcp_server`, `remove_mcp_server`,
/// `refresh_mcp_server_tools`, `submit_mcp_oauth_code`) is already a
/// synchronous Tauri command whose own frontend caller re-renders
/// right after `await`ing it -- harmless, not wrong, for this to also
/// fire on those paths (`finish_oauth` is shared), just redundant with
/// a render that already happened.
fn notify_mcp_servers_changed(app: &AppHandle) { app.emit("mcp-servers-changed", ()).ok(); }

/// Runs the full OAuth bootstrap (discovery, Dynamic Client
/// Registration, PKCE) for an HTTP server that needs it, opens the
/// browser, and returns `PendingAuthorization` immediately -- the rest
/// (waiting for the redirect, exchanging the code, persisting the
/// result) happens in a detached background task
/// ([`finish_oauth`]), never awaited by this function's own caller,
/// which is what actually keeps this from blocking the conversation.
async fn start_oauth_flow(
    app: &AppHandle,
    config: McpServerConfig,
    url: String,
    www_authenticate: Option<String>,
) -> McpConnectOutcome {
    let client = reqwest::Client::new();
    let metadata = match discover(&client, &url, www_authenticate.as_deref()).await {
        Ok(metadata) => metadata,
        Err(err) => return McpConnectOutcome::Failed { reason: err.to_string() },
    };
    let listener = match RedirectListener::bind().await {
        Ok(listener) => listener,
        Err(err) => return McpConnectOutcome::Failed { reason: err.to_string() },
    };
    let redirect_uri = listener.redirect_uri();
    let registration =
        match register_client_or_explain(&client, &metadata, &config.display_name, &redirect_uri)
            .await
        {
            Ok(registration) => registration,
            Err(err) => return McpConnectOutcome::Failed { reason: err.to_string() },
        };
    let pkce = generate_pkce();
    let state = generate_state();
    let authorization_url = build_authorization_url(
        &metadata,
        &registration.client_id,
        &redirect_uri,
        &pkce,
        &state,
        &url,
    );

    // Persisted as pending *before* the browser even opens, so the
    // Settings UI reflects this attempt even if the app restarts
    // before the user finishes (or never does).
    upsert_server_record(
        app,
        McpServerRecord {
            config: config.clone(),
            status: McpServerStatus::Pending,
            tools: Vec::new(),
            disabled_tools: std::collections::HashSet::new(),
        },
    );

    // Registered *before* opening the browser: a manual code paste
    // (Group 5.3) racing the redirect listener must find this attempt
    // the moment it could plausibly exist, not after some further
    // setup here.
    let pending_state = app.state::<PendingOAuthState>();
    pending_state
        .insert(
            config.id.clone(),
            PendingOAuthAttempt {
                config: config.clone(),
                client: client.clone(),
                token_endpoint: metadata.token_endpoint.clone(),
                redirect_uri: redirect_uri.clone(),
                client_id: registration.client_id.clone(),
                code_verifier: pkce.verifier.clone(),
            },
        )
        .await;

    // `Shell::open` is deprecated in favor of `tauri-plugin-opener`, but
    // remains functional; this app already depends on
    // `tauri-plugin-shell` for other reasons, and adding a second
    // plugin + its own capability grant for this one call isn't worth
    // it yet.
    #[allow(deprecated)]
    let opened = app.shell().open(&authorization_url, None);
    if let Err(err) = opened {
        pending_state.take(&config.id).await;
        return McpConnectOutcome::Failed { reason: format!("failed to open the browser: {err}") };
    }

    let display_name = config.display_name.clone();
    let task_display_name = display_name.clone();
    let server_id = config.id.clone();
    let app = app.clone();
    tokio::spawn(async move {
        let code = match listener.wait_for_code(&state).await {
            Ok(code) => code,
            Err(err) => {
                log::warn!("MCP OAuth redirect for \"{task_display_name}\" failed: {err}");
                return;
            }
        };
        // `take`, not a plain lookup: if a manual code paste (Group
        // 5.3) already consumed this attempt, there is nothing left
        // for the redirect to finish -- it arriving late is expected,
        // not an error.
        if let Some(attempt) = app.state::<PendingOAuthState>().take(&server_id).await {
            finish_oauth(&app, attempt, &code).await;
        }
    });

    McpConnectOutcome::PendingAuthorization { display_name }
}

/// Attempts the connection itself, after the user has already approved
/// it: resolves a fresh server id, probes an HTTP server for whether it
/// needs OAuth (never declared upfront -- see `mcp::oauth`'s own doc
/// comment), and either connects directly (stdio, or HTTP needing no
/// auth) or hands off to [`start_oauth_flow`].
pub(crate) async fn attempt_connection(
    app: &AppHandle,
    request: McpConnectRequest,
) -> McpConnectOutcome {
    let display_name = display_name_for(&request);
    let id = generate_server_id();

    let (config, credential): (McpServerConfig, Option<String>) = match request {
        McpConnectRequest::Stdio { command, args, credential_env_var, credential_value } => (
            McpServerConfig {
                id,
                display_name: display_name.clone(),
                transport: McpServerTransportConfig::Stdio { command, args, credential_env_var },
            },
            credential_value,
        ),
        McpConnectRequest::Http { url } => {
            match probe_authorization(&reqwest::Client::new(), &url).await {
                Ok(AuthProbeOutcome::Required { www_authenticate }) => {
                    let config = McpServerConfig {
                        id,
                        display_name: display_name.clone(),
                        transport: McpServerTransportConfig::Http { url: url.clone() },
                    };
                    return start_oauth_flow(app, config, url, www_authenticate).await;
                }
                Ok(AuthProbeOutcome::NotRequired) => (
                    McpServerConfig {
                        id,
                        display_name: display_name.clone(),
                        transport: McpServerTransportConfig::Http { url },
                    },
                    None,
                ),
                Err(err) => return McpConnectOutcome::Failed { reason: err.to_string() },
            }
        }
    };

    let connection_state = app.state::<McpConnectionState>();
    match connection_state.connection_for(&config, credential.as_deref()).await {
        Ok(client) => {
            let tools = client.list_tools().await.map(summarize_tools).unwrap_or_default();
            let tool_count = tools.len();
            upsert_server_record(
                app,
                McpServerRecord {
                    config,
                    status: McpServerStatus::Ready,
                    tools,
                    disabled_tools: std::collections::HashSet::new(),
                },
            );
            McpConnectOutcome::Connected { display_name, tool_count }
        }
        Err(err) => McpConnectOutcome::Failed { reason: err.to_string() },
    }
}

/// The real, popup-backed `McpConnector` (Group 4.2): shows the exact
/// resolved command/URL via the same confirmation popup flow
/// `PopupPermissionDecider` already uses, and only on approval attempts
/// the connection.
pub struct PopupMcpConnector {
    pub app: AppHandle,
}

#[async_trait::async_trait]
impl McpConnector for PopupMcpConnector {
    async fn connect(&self, request: McpConnectRequest) -> McpConnectOutcome {
        let id = rand::random::<u64>().to_string();
        let item = PendingConfirmationItem {
            id: id.clone(),
            tool_name: CONNECT_MCP_SERVER_TOOL_NAME.to_string(),
            summary: summarize_connect_request(&request),
            allows_remember: false,
        };

        let responses = confirm_via_popup(&self.app, vec![item]).await;
        let approved = responses.iter().any(|response| response.id == id && response.approved);
        if !approved {
            return McpConnectOutcome::Declined;
        }

        attempt_connection(&self.app, request).await
    }
}

#[cfg(test)]
mod tests {
    use fleet_snowfluff_ai::{PermissionTier, ToolContext, ToolError, ToolResult};
    use serde_json::json;

    use super::*;

    struct StubTool {
        name: &'static str,
        allows_remember: bool,
    }

    #[async_trait::async_trait]
    impl Tool for StubTool {
        fn definition(&self) -> fleet_snowfluff_ai::ToolDefinition {
            fleet_snowfluff_ai::ToolDefinition {
                name: self.name.to_string(),
                description: String::new(),
                parameters: json!({"type": "object", "properties": {}}),
            }
        }

        fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
            PermissionTier::Confirm
        }

        fn allows_session_remember(&self) -> bool { self.allows_remember }

        async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::ok(""))
        }
    }

    fn conversation_id() -> ConversationId {
        ConversationId::from_session_path(std::path::Path::new("/tmp/x"))
    }

    #[test]
    fn summarize_args_prefers_the_first_meaningful_field() {
        assert_eq!(summarize_args(&json!({"path": "a.txt"})), "a.txt");
        assert_eq!(summarize_args(&json!({"command": "ls -la"})), "ls -la");
        assert_eq!(summarize_args(&json!({"other": 1})), "{\"other\":1}");
    }

    #[test]
    fn build_item_reports_the_tools_own_remember_setting() {
        let call = PendingToolCall {
            id: "call_0".to_string(),
            name: "run_command".to_string(),
            arguments: json!({"command": "ls"}),
        };
        let tool = StubTool { name: "run_command", allows_remember: false };
        let item = build_item(&call, &tool);
        assert!(!item.allows_remember, "run_command must never offer the remember option");

        let read_call = PendingToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: json!({"path": "a.txt"}),
        };
        let read_tool = StubTool { name: "read_file", allows_remember: true };
        assert!(build_item(&read_call, &read_tool).allows_remember);
    }

    #[test]
    fn partition_remembered_skips_the_popup_entirely_when_everything_is_remembered() {
        let state = ChatRuntimeState::default();
        let conv = conversation_id();
        state.remember_tool(conv.clone(), RememberKey::Tool("get_system_context".to_string()));
        let registry = ToolRegistry::new(vec![std::sync::Arc::new(StubTool {
            name: "get_system_context",
            allows_remember: false,
        })]);
        let calls = vec![PendingToolCall {
            id: "call_0".to_string(),
            name: "get_system_context".to_string(),
            arguments: json!({}),
        }];

        let (approved, needs_decision) = partition_remembered(&conv, &calls, &state, &registry);
        assert_eq!(approved, HashSet::from(["call_0".to_string()]));
        assert!(needs_decision.is_empty());
    }

    #[test]
    fn a_remembered_path_does_not_grant_a_different_out_of_root_path() {
        let state = ChatRuntimeState::default();
        let conv = conversation_id();
        state.remember_tool(
            conv.clone(),
            RememberKey::ToolPath("read_file".to_string(), "/tmp/a.txt".to_string()),
        );
        let registry = ToolRegistry::new(vec![std::sync::Arc::new(StubTool {
            name: "read_file",
            allows_remember: true,
        })]);
        let calls = vec![PendingToolCall {
            id: "call_0".to_string(),
            name: "read_file".to_string(),
            arguments: json!({"path": "/tmp/b.txt"}),
        }];

        let (approved, needs_decision) = partition_remembered(&conv, &calls, &state, &registry);
        assert!(approved.is_empty(), "a different path must still need a real decision");
        assert_eq!(needs_decision.len(), 1);
    }

    #[tokio::test]
    async fn resolve_responses_via_a_dropped_sender_is_treated_as_deny() {
        // Mirrors what `confirm_via_popup` does when the window closes
        // without an explicit response: the `Sender` is dropped, `rx`
        // resolves to `Err`, and `unwrap_or_default()` yields an empty
        // response list -- nothing gets approved.
        let (tx, rx) = oneshot::channel::<Vec<ToolConfirmationResponse>>();
        drop(tx);
        let responses = rx.await.unwrap_or_default();
        assert!(responses.is_empty());
    }

    #[tokio::test]
    async fn resolve_responses_via_a_normal_send_is_honored() {
        let (tx, rx) = oneshot::channel();
        tx.send(vec![ToolConfirmationResponse {
            id: "call_0".to_string(),
            approved: true,
            remember: false,
        }])
        .unwrap();
        let responses = rx.await.unwrap_or_default();
        assert_eq!(responses.len(), 1);
        assert!(responses[0].approved);
    }

    fn item(id: &str) -> PendingConfirmationItem {
        PendingConfirmationItem {
            id: id.to_string(),
            tool_name: "run_command".to_string(),
            summary: String::new(),
            allows_remember: false,
        }
    }

    /// Mirrors what two concurrent `confirm_via_popup` calls do to
    /// `ToolConfirmationState` directly -- no `AppHandle` is needed to
    /// exercise the queue itself, the same reasoning every other test
    /// in this module already relies on.
    #[tokio::test]
    async fn a_second_batch_does_not_clobber_the_first() {
        let state = ToolConfirmationState::default();
        let (tx_a, rx_a) = oneshot::channel();
        let (tx_b, mut rx_b) = oneshot::channel();
        state.pending.lock().unwrap().push_back(PendingBatch {
            id: 1,
            items: vec![item("call_a")],
            responder: tx_a,
        });
        state.pending.lock().unwrap().push_back(PendingBatch {
            id: 2,
            items: vec![item("call_b")],
            responder: tx_b,
        });

        // The front of the queue is batch A -- not silently overwritten
        // by batch B arriving while A is still unresolved.
        let front_items =
            state.pending.lock().unwrap().front().map(|b| b.items.clone()).unwrap_or_default();
        assert_eq!(front_items, vec![item("call_a")]);

        // Resolving the front pops batch A and sends to *its* responder.
        let popped = state.pending.lock().unwrap().pop_front().unwrap();
        popped
            .responder
            .send(vec![ToolConfirmationResponse {
                id: "call_a".to_string(),
                approved: true,
                remember: false,
            }])
            .ok();
        let response_a = rx_a.await.unwrap_or_default();
        assert_eq!(response_a.len(), 1, "batch A's own responder received its own response");

        // Batch B is still intact, now at the front -- it was never
        // dropped or denied by batch A's arrival or resolution.
        let front_items =
            state.pending.lock().unwrap().front().map(|b| b.items.clone()).unwrap_or_default();
        assert_eq!(front_items, vec![item("call_b")]);
        assert!(
            matches!(rx_b.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "batch B's own receiver must not have resolved yet"
        );
    }

    #[tokio::test]
    async fn clearing_the_queue_denies_every_outstanding_batch() {
        // Mirrors the window's `Destroyed` handler: every batch still
        // in the queue when the window closes is denied, not only
        // whichever one happened to be shown.
        let state = ToolConfirmationState::default();
        let (tx_a, rx_a) = oneshot::channel();
        let (tx_b, rx_b) = oneshot::channel();
        state.pending.lock().unwrap().push_back(PendingBatch {
            id: 1,
            items: vec![item("call_a")],
            responder: tx_a,
        });
        state.pending.lock().unwrap().push_back(PendingBatch {
            id: 2,
            items: vec![item("call_b")],
            responder: tx_b,
        });

        state.pending.lock().unwrap().clear();

        assert!(rx_a.await.unwrap_or_default().is_empty());
        assert!(rx_b.await.unwrap_or_default().is_empty());
    }

    /// Reproduces the bug `PendingBatchGuard` fixes: a generation
    /// aborted (Stop) mid-`rx.await` must not leave its own batch
    /// orphaned in the queue to resurface for a later, unrelated
    /// generation. This mirrors `PendingBatchGuard::drop`'s own
    /// `retain`-by-id step directly against the queue -- no `AppHandle`
    /// is needed to exercise it, same reasoning as the other tests in
    /// this module -- standing in for the guard being dropped by task
    /// abortion before its `oneshot::Receiver` ever resolved.
    #[tokio::test]
    async fn an_abandoned_batch_is_removed_by_id_not_left_to_resurface() {
        let state = ToolConfirmationState::default();
        let (tx_a, _rx_a) = oneshot::channel();
        let (tx_b, mut rx_b) = oneshot::channel();
        state.pending.lock().unwrap().push_back(PendingBatch {
            id: 1,
            items: vec![item("call_a")],
            responder: tx_a,
        });
        state.pending.lock().unwrap().push_back(PendingBatch {
            id: 2,
            items: vec![item("call_b")],
            responder: tx_b,
        });

        // Simulates `PendingBatchGuard { id: 1, .. }` being dropped
        // (its task aborted) before batch A was ever resolved.
        state.pending.lock().unwrap().retain(|batch| batch.id != 1);

        // Batch A is gone -- not sitting in the queue waiting to
        // resurface ahead of some later, unrelated generation's batch.
        let front_items =
            state.pending.lock().unwrap().front().map(|b| b.items.clone()).unwrap_or_default();
        assert_eq!(front_items, vec![item("call_b")]);
        assert_eq!(state.pending.lock().unwrap().len(), 1);

        // Batch B, never targeted by the abort, is untouched.
        assert!(
            matches!(rx_b.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "batch B's own receiver must not have resolved yet"
        );
        state.pending.lock().unwrap().pop_front().unwrap().responder.send(vec![]).ok();
        rx_b.await.ok();
    }

    // -- mcp-client-support: PopupMcpConnector's own pure helpers.
    // `PopupMcpConnector::connect` itself, like `PopupPermissionDecider::decide`
    // above (never directly unit-tested in this file either), touches
    // `AppHandle`-backed state (`confirm_via_popup`, `McpConnectionState`,
    // `mcp_servers_store`) and is verified manually, consistent with
    // this file's own established boundary -- only the AppHandle-free
    // logic is unit-tested here.

    #[test]
    fn summarize_connect_request_shows_the_exact_command_or_url() {
        assert_eq!(
            summarize_connect_request(&McpConnectRequest::Stdio {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "server-filesystem".to_string()],
                credential_env_var: None,
                credential_value: None,
            }),
            "npx -y server-filesystem"
        );
        assert_eq!(
            summarize_connect_request(&McpConnectRequest::Stdio {
                command: "cat".to_string(),
                args: vec![],
                credential_env_var: None,
                credential_value: None,
            }),
            "cat",
            "no trailing space when there are no args"
        );
        assert_eq!(
            summarize_connect_request(&McpConnectRequest::Http {
                url: "https://mcp.example.com/sse".to_string()
            }),
            "https://mcp.example.com/sse"
        );
    }

    #[test]
    fn display_name_for_uses_the_resolved_command_or_url() {
        assert_eq!(
            display_name_for(&McpConnectRequest::Stdio {
                command: "github-mcp-server".to_string(),
                args: vec!["--flag".to_string()],
                credential_env_var: None,
                credential_value: None,
            }),
            "github-mcp-server",
            "the command alone, not its args"
        );
        assert_eq!(
            display_name_for(&McpConnectRequest::Http { url: "https://example.com".to_string() }),
            "https://example.com"
        );
    }

    #[test]
    fn generate_server_id_produces_distinct_values() {
        assert_ne!(generate_server_id(), generate_server_id());
    }
}
