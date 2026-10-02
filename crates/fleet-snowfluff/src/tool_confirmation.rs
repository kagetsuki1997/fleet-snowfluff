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

use fleet_snowfluff_ai::{PendingToolCall, PermissionDecider, Tool, ToolRegistry};
use serde_json::Value;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tokio::sync::oneshot;

use crate::{
    chat_commands::{ChatRuntimeState, RememberKey},
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
}
