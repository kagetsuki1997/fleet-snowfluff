//! `AgentRuntime`: the tool-calling conversation loop for a
//! `ToolCallingProvider` (`agent-core-and-task-router`'s Group 4).
//! Concretely: `AemeathAgentRuntime` sends `messages`, reads back a
//! stream of text and/or tool calls, executes whichever calls are
//! permitted, feeds their results back in, and repeats until the model
//! stops requesting tools or a fixed iteration limit is hit.
//!
//! Deliberately does not take the fuller `Execution`/`AgentResult`
//! shape `docs/fleet-snowfluff-feature-planning.md` §6.7 sketches for
//! the eventual multi-runtime architecture -- this proposal has only
//! one `AgentRuntime` implementation and no sub-agent delegation yet
//! (Stage 5's job), so there is nothing for a heavier `Execution`
//! wrapper to abstract over today.

use std::{collections::HashSet, sync::Arc};

use futures_util::{stream, StreamExt};
use serde_json::Value;

use crate::{
    agent_tool::{PermissionTier, Tool, ToolContext, ToolResult},
    message::{Message, ProviderError, ToolCallRecord},
    tool_provider::{ToolCallStreamItem, ToolCallingProvider, ToolDefinition},
};

/// At most this many bytes of a rejected tool call's raw arguments are
/// kept for diagnosis (`execution-log-and-context`) -- matching the
/// magnitude of `run_command`'s and `read_file`'s own output caps, not
/// because a malformed-arguments blob is a realistic attack surface for
/// a desktop pet app, but because every other model-originated value
/// already persisted in this codebase is bounded, and this is the one
/// new place that persists one.
const MAX_REJECTED_ARGS_BYTES: usize = 20 * 1024;
const REJECTED_ARGS_TRUNCATION_MARKER: &str = " ...[truncated]";

/// How many `Auto`-tier tool calls from one model turn may actually be
/// executing at once (`sub-agent-delegation`'s own design.md Decision
/// 7). Unlike every other native tool, a delegated sub-task's own
/// blast radius (a full nested agent loop, possibly a CLI subprocess)
/// is categorically larger than `read_file`'s, so this throttles total
/// concurrency rather than letting an unbounded `join_all` run
/// everything the model asked for at once. Calls beyond this limit
/// still all execute -- they simply queue, starting as earlier ones
/// finish, never rejected or dropped.
const MAX_CONCURRENT_TOOL_CALLS: usize = 3;

/// Serializes `arguments` compactly and caps it at
/// [`MAX_REJECTED_ARGS_BYTES`], cutting on a UTF-8 character boundary
/// (never mid-character) the same way `read_file`'s own cap does. A
/// `Value` that somehow fails to serialize (never happens for anything
/// `serde_json::from_str` itself produced, which is the only source of
/// a `Rejected` call's arguments) falls back to a fixed placeholder
/// rather than panicking.
fn capped_arguments_preview(arguments: &Value) -> String {
    let full = serde_json::to_string(arguments)
        .unwrap_or_else(|_| "<arguments could not be serialized>".to_string());
    if full.len() <= MAX_REJECTED_ARGS_BYTES {
        return full;
    }
    let mut cut = MAX_REJECTED_ARGS_BYTES;
    while !full.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{REJECTED_ARGS_TRUNCATION_MARKER}", &full[..cut])
}

/// What became of one tool call the model requested, retained after the
/// agent loop returns (`execution-log-and-context`'s "Tool call outcomes
/// survive the turn that produced them"). Only `Rejected` keeps any
/// content from the call itself -- the one outcome where the call never
/// reached `execute()`, so there is no tool-side result to protect, and
/// the argument is the only available diagnostic for why it failed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    /// Reached `execute()`; `ok` is `false` for a `ToolError` or a
    /// `ToolResult { is_error: true, .. }`, exactly the same distinction
    /// `tool_result_content` already makes for the model-facing message.
    Executed { ok: bool },
    /// A `Deny`-tier tool -- a standing policy setting, not a one-off
    /// choice for this call.
    DeniedByPolicy,
    /// A `Confirm`-tier tool the user declined for this call -- a
    /// one-off choice that might go differently next time, kept
    /// distinct from `DeniedByPolicy` for exactly that reason.
    DeclinedByUser,
    /// An unknown tool name, or arguments that are not a JSON object --
    /// the two cases the loop already treats identically (an error
    /// tool-result with no execution attempted).
    Rejected { arguments_preview: String },
}

/// One tool call's name and what became of it, in the order the model
/// requested it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolInvocation {
    pub name: String,
    pub outcome: ToolOutcome,
}

/// One tool call the model requested in a single turn, before it's
/// known whether it's permitted to run -- the unit
/// [`PermissionDecider::decide`] batches over (the "Batched
/// confirmation for one turn" requirement).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// What `run()`'s classification pass decided for one call, before any
/// `Auto`-tier call has actually been executed -- kept separate from
/// execution so classification (synchronous, ordered) and `Auto`-tier
/// execution (concurrent, bounded) can be two distinct passes without
/// losing each call's original position.
enum CallPlan {
    UnknownTool,
    MalformedArguments,
    Denied,
    Auto(Arc<dyn Tool>),
    Confirm(Arc<dyn Tool>),
}

/// Decides how every `PermissionTier::Confirm` call from one model
/// turn actually resolves -- `Auto`/`Deny` need no decision (the tier
/// itself is the answer); only `Confirm` calls ever reach this. Kept as
/// its own trait so Group 6's real popup-backed implementation is
/// swappable for a trivial always-approve/always-deny one in tests,
/// without either depending on Tauri.
#[async_trait::async_trait]
pub trait PermissionDecider: Send + Sync {
    /// Returns the `id`s of every call in `calls` that is approved;
    /// any `id` not present is treated as denied.
    async fn decide(&self, calls: &[PendingToolCall]) -> HashSet<String>;
}

/// The tools available to one `AgentRuntime::run` call. A thin,
/// immutable lookup -- ownership/lifecycle of the underlying `Tool`
/// implementations (Group 5's 4 native tools) belongs to whatever
/// constructs the registry per turn, not to this type.
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self { Self { tools } }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    pub fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|tool| tool.definition().name == name).cloned()
    }
}

/// Why `AgentRuntime::run` failed to produce a final answer at all --
/// distinct from a tool call being denied or erroring, both of which
/// are reported *back to the model* as a normal tool-result message
/// (the "A denied call is reported back to the model" requirement) and
/// never surface here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    Provider(ProviderError),
    /// The model kept requesting tool calls without ever producing a
    /// final answer, for the fixed maximum number of iterations in a
    /// row -- carries whatever text accompanied that final iteration
    /// (often empty), so a caller can still show *something* rather
    /// than nothing.
    MaxIterationsReached {
        partial_text: String,
    },
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(err) => write!(f, "{err}"),
            Self::MaxIterationsReached { .. } => {
                write!(f, "the model did not produce a final answer within the iteration limit")
            }
        }
    }
}

impl std::error::Error for AgentError {}

/// The final answer text plus a trace of every tool call attempted
/// during the turn, in the order the model requested them
/// (`execution-log-and-context`'s "Tool call outcomes survive the turn
/// that produced them"). The trace is empty for a turn that made no
/// tool calls at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOutcome {
    pub text: String,
    pub trace: Vec<ToolInvocation>,
}

/// Runs the tool-calling loop for a `ToolCallingProvider`. Implementers
/// never need to manage `Conversation`/session lifecycle themselves --
/// `run`'s job ends at producing the final answer and its tool trace,
/// the same boundary `TaskRouter::route()` draws for itself.
///
/// `on_text_delta` is invoked for every [`ToolCallStreamItem::TextDelta`]
/// as it arrives, on *every* iteration -- not just the final one that
/// ends the loop -- so a caller can forward it live to the UI the same
/// way every existing (non-tool-calling) chat path already streams
/// `ChatEvent::Chunk`s. Tool-call JSON and tool execution stay
/// invisible either way; only real model-generated text ever reaches
/// this callback. A model that narrates before calling a tool
/// therefore still streams that narration live, exactly as if the tool
/// call never happened. The trace returned in `AgentOutcome` is a
/// separate, after-the-fact summary -- not something `on_text_delta`
/// or any other callback surfaces live.
#[async_trait::async_trait]
pub trait AgentRuntime: Send + Sync {
    async fn run(
        &self,
        provider: &dyn ToolCallingProvider,
        messages: Vec<Message>,
        registry: &ToolRegistry,
        ctx: &ToolContext,
        permission: &dyn PermissionDecider,
        on_text_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<AgentOutcome, AgentError>;
}

/// The runtime this change actually ships. `max_iterations` guards
/// against the "Agent loop runaway" risk the planning doc calls out --
/// 8 is a generous-but-bounded default: enough for a call, a follow-up
/// call informed by the first result, and headroom beyond that, while
/// still guaranteeing the loop cannot run forever.
pub struct AemeathAgentRuntime {
    pub max_iterations: usize,
}

impl Default for AemeathAgentRuntime {
    fn default() -> Self { Self { max_iterations: 8 } }
}

/// Turns a permitted tool's own outcome into the tool-result message
/// text the model sees next -- `is_error` gets a plain, model-legible
/// prefix rather than a separate wire field (Ollama's own tool-result
/// message shape has no error flag to carry one in).
fn tool_result_content(result: ToolResult) -> String {
    if result.is_error {
        format!("Error: {}", result.content)
    } else {
        result.content
    }
}

#[async_trait::async_trait]
impl AgentRuntime for AemeathAgentRuntime {
    async fn run(
        &self,
        provider: &dyn ToolCallingProvider,
        mut messages: Vec<Message>,
        registry: &ToolRegistry,
        ctx: &ToolContext,
        permission: &dyn PermissionDecider,
        on_text_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<AgentOutcome, AgentError> {
        let mut last_text = String::new();
        let mut trace: Vec<ToolInvocation> = Vec::new();

        for _ in 0..self.max_iterations {
            let mut stream = provider
                .chat_with_tools(messages.clone(), registry.definitions())
                .await
                .map_err(AgentError::Provider)?;

            let mut text = String::new();
            let mut calls = Vec::new();
            while let Some(item) = stream.next().await {
                match item.map_err(AgentError::Provider)? {
                    ToolCallStreamItem::TextDelta(delta) => {
                        on_text_delta(&delta);
                        text.push_str(&delta);
                    }
                    ToolCallStreamItem::ToolCall { id, name, arguments } => {
                        calls.push(PendingToolCall { id, name, arguments })
                    }
                }
            }
            last_text = text.clone();

            if calls.is_empty() {
                return Ok(AgentOutcome { text, trace });
            }

            messages.push(Message::assistant_with_tool_calls(
                text,
                calls
                    .iter()
                    .map(|call| ToolCallRecord {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .collect(),
            ));

            // Schema validation (tool exists, arguments is an object)
            // and a `Deny` decision both resolve the same way -- a
            // tool-result message telling the model the call didn't
            // run -- so they're classified in the same pass as `Auto`/
            // `Confirm`, below. Classification itself is synchronous
            // (no `.await`), so it stays a single ordered pass; only
            // `Auto`-tier *execution* is deferred and run concurrently,
            // in the next pass.
            let plans: Vec<CallPlan> = calls
                .iter()
                .map(|call| {
                    let Some(tool) = registry.find(&call.name) else {
                        return CallPlan::UnknownTool;
                    };
                    if !call.arguments.is_object() {
                        return CallPlan::MalformedArguments;
                    }
                    match tool.required_permission(&call.arguments, ctx) {
                        PermissionTier::Auto => CallPlan::Auto(tool),
                        PermissionTier::Deny => CallPlan::Denied,
                        PermissionTier::Confirm => CallPlan::Confirm(tool),
                    }
                })
                .collect();

            // `Auto`-tier calls run concurrently, bounded by
            // `MAX_CONCURRENT_TOOL_CALLS` -- but `buffer_unordered`
            // yields results in *completion* order, not the order the
            // calls were originally requested in. Ollama pairs tool
            // results with calls by position, not by id (unlike
            // OpenAI/Anthropic, which use `tool_call_id`/`tool_use_id`
            // explicitly) -- see `message.rs`'s own doc comment on
            // `Message::tool_call_id` -- so results are written into a
            // slot indexed by each call's *original* position here,
            // and only read back out in that same order in the
            // writeback pass below, regardless of which finished first.
            let mut auto_results: Vec<Option<(bool, String)>> =
                calls.iter().map(|_| None).collect();
            // Built via a plain loop, not `.filter_map(closure)` -- a
            // closure returning `impl Future` here hits rustc's HRTB
            // inference limit (it cannot unify the borrowed-`call`
            // lifetime across every closure invocation); boxing each
            // future explicitly sidesteps that by giving `stream::iter`
            // one concrete, uniform item type instead.
            let mut auto_futures: Vec<
                std::pin::Pin<
                    Box<dyn std::future::Future<Output = (usize, bool, String)> + Send + '_>,
                >,
            > = Vec::new();
            for (index, (call, plan)) in calls.iter().zip(plans.iter()).enumerate() {
                if let CallPlan::Auto(tool) = plan {
                    let tool = tool.clone();
                    let arguments = call.arguments.clone();
                    auto_futures.push(Box::pin(async move {
                        let result = execute(&tool, arguments, ctx).await;
                        (index, !result.is_error, tool_result_content(result))
                    }));
                }
            }
            let completed: Vec<(usize, bool, String)> = stream::iter(auto_futures)
                .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS)
                .collect()
                .await;
            for (index, ok, content) in completed {
                auto_results[index] = Some((ok, content));
            }

            // Writeback, in original request order -- the ordering
            // guarantee the pass above exists to preserve.
            let mut pending_confirm: Vec<(PendingToolCall, Arc<dyn Tool>)> = Vec::new();
            for (index, call) in calls.into_iter().enumerate() {
                match &plans[index] {
                    CallPlan::UnknownTool => {
                        messages.push(Message::tool_result(
                            call.id.clone(),
                            format!("Error: unknown tool \"{}\"", call.name),
                        ));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::Rejected {
                                arguments_preview: capped_arguments_preview(&call.arguments),
                            },
                        });
                    }
                    CallPlan::MalformedArguments => {
                        messages.push(Message::tool_result(
                            call.id.clone(),
                            format!("Error: arguments for \"{}\" must be a JSON object", call.name),
                        ));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::Rejected {
                                arguments_preview: capped_arguments_preview(&call.arguments),
                            },
                        });
                    }
                    CallPlan::Denied => {
                        messages.push(Message::tool_result(
                            call.id.clone(),
                            format!("Error: \"{}\" was not permitted to run", call.name),
                        ));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::DeniedByPolicy,
                        });
                    }
                    CallPlan::Auto(_) => {
                        let (ok, content) = auto_results[index]
                            .take()
                            .expect("every Auto-tier call has a result by the writeback pass");
                        messages.push(Message::tool_result(call.id.clone(), content));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::Executed { ok },
                        });
                    }
                    CallPlan::Confirm(tool) => pending_confirm.push((call, tool.clone())),
                }
            }

            if !pending_confirm.is_empty() {
                let batch: Vec<PendingToolCall> =
                    pending_confirm.iter().map(|(call, _)| call.clone()).collect();
                let approved = permission.decide(&batch).await;
                for (call, tool) in pending_confirm {
                    if approved.contains(&call.id) {
                        let id = call.id.clone();
                        let name = call.name.clone();
                        let result = execute(&tool, call.arguments, ctx).await;
                        let ok = !result.is_error;
                        messages.push(Message::tool_result(id, tool_result_content(result)));
                        trace.push(ToolInvocation { name, outcome: ToolOutcome::Executed { ok } });
                    } else {
                        messages.push(Message::tool_result(
                            call.id.clone(),
                            format!("Error: \"{}\" was not permitted to run", call.name),
                        ));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::DeclinedByUser,
                        });
                    }
                }
            }
        }

        Err(AgentError::MaxIterationsReached { partial_text: last_text })
    }
}

async fn execute(tool: &Arc<dyn Tool>, args: Value, ctx: &ToolContext) -> ToolResult {
    match tool.execute(args, ctx).await {
        Ok(result) => result,
        Err(err) => ToolResult::error(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures_util::stream::{iter, BoxStream};
    use serde_json::json;

    use super::*;
    use crate::{
        conversation::ConversationId,
        message::{ModelInfo, ProviderKind},
        provider::{AiProvider, ChatStream},
        tool_provider::ToolCallStream,
    };

    fn ctx() -> ToolContext {
        ToolContext {
            project_root: None,
            conversation_id: ConversationId::from_session_path(std::path::Path::new("/tmp/x")),
        }
    }

    // -- execution-log-and-context: ToolOutcome/ToolInvocation/AgentOutcome --

    #[test]
    fn every_tool_outcome_round_trips_through_construction_and_equality() {
        let outcomes = [
            ToolOutcome::Executed { ok: true },
            ToolOutcome::Executed { ok: false },
            ToolOutcome::DeniedByPolicy,
            ToolOutcome::DeclinedByUser,
            ToolOutcome::Rejected { arguments_preview: "{}".to_string() },
        ];
        for outcome in outcomes {
            let invocation = ToolInvocation { name: "alpha".to_string(), outcome: outcome.clone() };
            assert_eq!(invocation.outcome, outcome);
            assert_eq!(invocation.clone(), invocation);
        }
    }

    #[test]
    fn an_agent_outcome_round_trips() {
        let outcome = AgentOutcome {
            text: "done".to_string(),
            trace: vec![ToolInvocation {
                name: "alpha".to_string(),
                outcome: ToolOutcome::Executed { ok: true },
            }],
        };
        assert_eq!(outcome.clone(), outcome);
    }

    #[test]
    fn arguments_within_the_cap_are_kept_verbatim() {
        let args = json!({"path": "a.txt"});
        assert_eq!(capped_arguments_preview(&args), r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn arguments_exactly_at_the_cap_are_not_marked_truncated() {
        // Build a JSON string of exactly MAX_REJECTED_ARGS_BYTES bytes.
        let filler = "x".repeat(MAX_REJECTED_ARGS_BYTES - 2); // minus the quotes
        let args = Value::String(filler);
        let preview = capped_arguments_preview(&args);
        assert_eq!(preview.len(), MAX_REJECTED_ARGS_BYTES);
        assert!(!preview.contains("truncated"));
    }

    #[test]
    fn arguments_over_the_cap_are_truncated_with_a_visible_marker() {
        let filler = "x".repeat(MAX_REJECTED_ARGS_BYTES * 3);
        let args = Value::String(filler);
        let preview = capped_arguments_preview(&args);
        assert!(preview.starts_with("\"xxxx"));
        assert!(preview.ends_with(REJECTED_ARGS_TRUNCATION_MARKER));
        assert_eq!(preview.len(), MAX_REJECTED_ARGS_BYTES + REJECTED_ARGS_TRUNCATION_MARKER.len());
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character_in_the_arguments_preview() {
        // "雪" is 3 bytes in UTF-8; cutting at a raw byte count with no
        // regard for character boundaries would, for most lengths, land
        // mid-character. This only exercises the function's own
        // boundary-walking, not a specific cap-size/char-width
        // coincidence, so it doesn't matter whether `MAX_REJECTED_ARGS_BYTES`
        // happens to be a multiple of 3.
        let filler = "雪".repeat(MAX_REJECTED_ARGS_BYTES);
        let args = Value::String(filler);
        let preview = capped_arguments_preview(&args);
        assert!(preview.ends_with(REJECTED_ARGS_TRUNCATION_MARKER));
        let kept = preview.strip_suffix(REJECTED_ARGS_TRUNCATION_MARKER).unwrap();
        assert!(kept.is_char_boundary(kept.len()), "the kept prefix must end on a char boundary");
        assert!(!kept.contains('\u{FFFD}'), "no replacement characters from a split boundary");
        assert!(kept.len() <= MAX_REJECTED_ARGS_BYTES);
    }

    /// Approves every call it's asked about -- the default for tests
    /// that aren't exercising confirmation behavior itself.
    struct AlwaysApprove;

    #[async_trait::async_trait]
    impl PermissionDecider for AlwaysApprove {
        async fn decide(&self, calls: &[PendingToolCall]) -> HashSet<String> {
            calls.iter().map(|call| call.id.clone()).collect()
        }
    }

    /// Denies every call -- used to prove a denied `Confirm` call never
    /// executes.
    struct AlwaysDeny;

    #[async_trait::async_trait]
    impl PermissionDecider for AlwaysDeny {
        async fn decide(&self, _calls: &[PendingToolCall]) -> HashSet<String> { HashSet::new() }
    }

    /// A tool that records how many times it actually ran, so tests can
    /// assert a denied/unpermitted call never reached `execute`.
    struct CountingTool {
        name: &'static str,
        tier: PermissionTier,
        calls: Mutex<u32>,
    }

    impl CountingTool {
        fn new(name: &'static str, tier: PermissionTier) -> Self {
            Self { name, tier, calls: Mutex::new(0) }
        }
    }

    #[async_trait::async_trait]
    impl Tool for CountingTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name.to_string(),
                description: String::new(),
                parameters: json!({"type": "object", "properties": {}}),
            }
        }

        fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
            self.tier
        }

        async fn execute(
            &self,
            _args: Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, crate::agent_tool::ToolError> {
            *self.calls.lock().unwrap() += 1;
            Ok(ToolResult::ok(format!("{} ran", self.name)))
        }
    }

    /// A `ToolCallingProvider` double whose `chat_with_tools` yields the
    /// next scripted response on each call -- one entry per expected
    /// model round-trip.
    struct ScriptedProvider {
        responses: Mutex<std::collections::VecDeque<Vec<ToolCallStreamItem>>>,
        /// The message list handed to each `chat_with_tools` call, in
        /// order -- what the model actually receives on each round trip.
        received: Mutex<Vec<Vec<Message>>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<Vec<ToolCallStreamItem>>) -> Self {
            Self { responses: Mutex::new(responses.into()), received: Mutex::new(Vec::new()) }
        }

        /// Never runs out -- every call gets the same tool-call
        /// response, for testing that the loop terminates via the
        /// iteration limit rather than the script running dry.
        fn repeating(response: Vec<ToolCallStreamItem>) -> Self {
            let mut queue = std::collections::VecDeque::new();
            for _ in 0..100 {
                queue.push_back(response.clone());
            }
            Self { responses: Mutex::new(queue), received: Mutex::new(Vec::new()) }
        }
    }

    #[async_trait::async_trait]
    impl AiProvider for ScriptedProvider {
        fn kind(&self) -> ProviderKind { ProviderKind::Ollama }

        async fn chat(&self, _messages: Vec<Message>) -> Result<ChatStream, ProviderError> {
            unimplemented!("ScriptedProvider only exercises chat_with_tools")
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> { Ok(vec![]) }
    }

    #[async_trait::async_trait]
    impl ToolCallingProvider for ScriptedProvider {
        async fn chat_with_tools(
            &self,
            messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<ToolCallStream, ProviderError> {
            self.received.lock().unwrap().push(messages);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("ScriptedProvider called more times than scripted");
            let stream: BoxStream<'static, Result<ToolCallStreamItem, ProviderError>> =
                Box::pin(iter(response.into_iter().map(Ok)));
            Ok(stream)
        }
    }

    #[tokio::test]
    async fn a_multi_tool_call_turn_resolves_correctly() {
        let provider = ScriptedProvider::new(vec![
            vec![
                ToolCallStreamItem::ToolCall {
                    id: "call_0".to_string(),
                    name: "alpha".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_1".to_string(),
                    name: "beta".to_string(),
                    arguments: json!({}),
                },
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Auto));
        let beta = Arc::new(CountingTool::new("beta", PermissionTier::Auto));
        let registry = ToolRegistry::new(vec![alpha.clone(), beta.clone()]);
        let runtime = AemeathAgentRuntime::default();

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done");
        assert_eq!(
            result.trace,
            vec![
                ToolInvocation {
                    name: "alpha".to_string(),
                    outcome: ToolOutcome::Executed { ok: true }
                },
                ToolInvocation {
                    name: "beta".to_string(),
                    outcome: ToolOutcome::Executed { ok: true }
                },
            ],
            "the trace accumulates across iterations, not just the final text-only one"
        );
        assert_eq!(*alpha.calls.lock().unwrap(), 1);
        assert_eq!(*beta.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn every_tool_result_carries_the_id_of_the_call_it_answers() {
        // One turn exercising every result path: executed, unknown tool,
        // malformed arguments, denied by tier, and denied at confirmation.
        let call = |id: &str, name: &str, arguments: Value| ToolCallStreamItem::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
        };
        let provider = ScriptedProvider::new(vec![
            vec![
                call("c_ok", "alpha", json!({})),
                call("c_unknown", "no_such_tool", json!({})),
                call("c_badargs", "alpha", json!("not an object")),
                call("c_denied", "gamma", json!({})),
                call("c_confirm_no", "delta", json!({})),
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![
            Arc::new(CountingTool::new("alpha", PermissionTier::Auto)),
            Arc::new(CountingTool::new("gamma", PermissionTier::Deny)),
            Arc::new(CountingTool::new("delta", PermissionTier::Confirm)),
        ]);

        AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysDeny,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        let received = provider.received.lock().unwrap();
        let second_turn = &received[1];
        let results: Vec<(&str, &str)> = second_turn
            .iter()
            .filter(|m| m.role == crate::message::Role::Tool)
            .map(|m| {
                (
                    m.tool_call_id.as_deref().expect("every tool result has an id"),
                    m.content.as_str(),
                )
            })
            .collect();
        let ids: Vec<&str> = results.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, ["c_ok", "c_unknown", "c_badargs", "c_denied", "c_confirm_no"]);
        assert!(results[0].1.contains("alpha ran"), "the executed call's own output");
        assert!(results[1].1.contains("unknown tool"));
    }

    #[tokio::test]
    async fn all_five_tool_outcomes_in_one_turn_produce_a_trace_in_call_order() {
        let provider = ScriptedProvider::new(vec![
            vec![
                ToolCallStreamItem::ToolCall {
                    id: "call_0".to_string(),
                    name: "does_not_exist".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_1".to_string(),
                    name: "needs_an_object".to_string(),
                    arguments: json!("not an object"),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_2".to_string(),
                    name: "alpha".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_3".to_string(),
                    name: "gamma".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_4".to_string(),
                    name: "delta".to_string(),
                    arguments: json!({}),
                },
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![
            Arc::new(CountingTool::new("needs_an_object", PermissionTier::Auto)),
            Arc::new(CountingTool::new("alpha", PermissionTier::Auto)),
            Arc::new(CountingTool::new("gamma", PermissionTier::Deny)),
            Arc::new(CountingTool::new("delta", PermissionTier::Confirm)),
        ]);
        let runtime = AemeathAgentRuntime::default();

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysDeny,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done");
        let names: Vec<&str> = result.trace.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["does_not_exist", "needs_an_object", "alpha", "gamma", "delta"]);
        assert!(matches!(result.trace[0].outcome, ToolOutcome::Rejected { .. }));
        assert!(matches!(result.trace[1].outcome, ToolOutcome::Rejected { .. }));
        assert_eq!(result.trace[2].outcome, ToolOutcome::Executed { ok: true });
        assert_eq!(result.trace[3].outcome, ToolOutcome::DeniedByPolicy);
        assert_eq!(result.trace[4].outcome, ToolOutcome::DeclinedByUser);
    }

    /// Blocks until `gate` is signaled before returning -- lets a test
    /// force one `Auto`-tier call to finish strictly *after* another,
    /// deterministically, with no reliance on real-time sleeps that
    /// could flake under load.
    struct WaitsForGate {
        name: &'static str,
        gate: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl Tool for WaitsForGate {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name.to_string(),
                description: String::new(),
                parameters: json!({"type": "object", "properties": {}}),
            }
        }

        fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
            PermissionTier::Auto
        }

        async fn execute(
            &self,
            _args: Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, crate::agent_tool::ToolError> {
            self.gate.notified().await;
            Ok(ToolResult::ok(format!("{} ran", self.name)))
        }
    }

    /// The counterpart to `WaitsForGate`: signals `gate` and returns
    /// immediately, with no `.await` point of its own before doing so --
    /// guarantees this call's own future resolves on its very first
    /// poll, strictly before whatever is waiting on the same gate can.
    struct SignalsGate {
        name: &'static str,
        gate: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl Tool for SignalsGate {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name.to_string(),
                description: String::new(),
                parameters: json!({"type": "object", "properties": {}}),
            }
        }

        fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
            PermissionTier::Auto
        }

        async fn execute(
            &self,
            _args: Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, crate::agent_tool::ToolError> {
            self.gate.notify_one();
            Ok(ToolResult::ok(format!("{} ran", self.name)))
        }
    }

    #[tokio::test]
    async fn concurrent_auto_calls_are_written_back_in_request_order_not_completion_order() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let provider = ScriptedProvider::new(vec![
            vec![
                ToolCallStreamItem::ToolCall {
                    id: "call_0".to_string(),
                    name: "slow".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_1".to_string(),
                    name: "fast".to_string(),
                    arguments: json!({}),
                },
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        // "slow" is requested first but can only finish once "fast"
        // signals the gate -- "fast" (requested second) is therefore
        // guaranteed to complete first, regardless of scheduling.
        let registry = ToolRegistry::new(vec![
            Arc::new(WaitsForGate { name: "slow", gate: gate.clone() }),
            Arc::new(SignalsGate { name: "fast", gate: gate.clone() }),
        ]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(
            result.trace,
            vec![
                ToolInvocation {
                    name: "slow".to_string(),
                    outcome: ToolOutcome::Executed { ok: true }
                },
                ToolInvocation {
                    name: "fast".to_string(),
                    outcome: ToolOutcome::Executed { ok: true }
                },
            ],
            "written back in request order even though \"fast\" completed first"
        );
    }

    #[tokio::test]
    async fn more_auto_calls_than_the_concurrency_cap_all_still_execute() {
        // MAX_CONCURRENT_TOOL_CALLS is 3; five calls in one turn must
        // all still run exactly once each, just throttled in how many
        // start at once -- none rejected or silently dropped.
        let provider = ScriptedProvider::new(vec![
            vec![
                ToolCallStreamItem::ToolCall {
                    id: "call_0".to_string(),
                    name: "t0".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_1".to_string(),
                    name: "t1".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_2".to_string(),
                    name: "t2".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_3".to_string(),
                    name: "t3".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_4".to_string(),
                    name: "t4".to_string(),
                    arguments: json!({}),
                },
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let t0 = Arc::new(CountingTool::new("t0", PermissionTier::Auto));
        let t1 = Arc::new(CountingTool::new("t1", PermissionTier::Auto));
        let t2 = Arc::new(CountingTool::new("t2", PermissionTier::Auto));
        let t3 = Arc::new(CountingTool::new("t3", PermissionTier::Auto));
        let t4 = Arc::new(CountingTool::new("t4", PermissionTier::Auto));
        let registry =
            ToolRegistry::new(vec![t0.clone(), t1.clone(), t2.clone(), t3.clone(), t4.clone()]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        let names: Vec<&str> = result.trace.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["t0", "t1", "t2", "t3", "t4"]);
        assert!(result.trace.iter().all(|i| i.outcome == ToolOutcome::Executed { ok: true }));
        for tool in [&t0, &t1, &t2, &t3, &t4] {
            assert_eq!(*tool.calls.lock().unwrap(), 1, "{} must run exactly once", tool.name);
        }
    }

    #[tokio::test]
    async fn iteration_stops_at_the_limit_without_hanging() {
        let provider = ScriptedProvider::repeating(vec![ToolCallStreamItem::ToolCall {
            id: "call_0".to_string(),
            name: "alpha".to_string(),
            arguments: json!({}),
        }]);
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Auto));
        let registry = ToolRegistry::new(vec![alpha.clone()]);
        let runtime = AemeathAgentRuntime { max_iterations: 3 };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            runtime.run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &mut |_: &str| {},
            ),
        )
        .await
        .expect("the loop must terminate on its own well within the timeout");

        assert!(matches!(result, Err(AgentError::MaxIterationsReached { .. })));
        assert_eq!(*alpha.calls.lock().unwrap(), 3, "each of the 3 iterations should have run it");
    }

    #[tokio::test]
    async fn a_denied_tier_tool_call_never_executes() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "alpha".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("ok".to_string())],
        ]);
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Deny));
        let registry = ToolRegistry::new(vec![alpha.clone()]);
        let runtime = AemeathAgentRuntime::default();

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "ok");
        assert_eq!(
            result.trace,
            vec![ToolInvocation {
                name: "alpha".to_string(),
                outcome: ToolOutcome::DeniedByPolicy
            }]
        );
        assert_eq!(*alpha.calls.lock().unwrap(), 0, "a Deny-tier call must never reach execute()");
    }

    #[tokio::test]
    async fn a_confirm_tier_call_defers_to_the_permission_decider() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "alpha".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("ok".to_string())],
        ]);
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Confirm));
        let registry = ToolRegistry::new(vec![alpha.clone()]);
        let runtime = AemeathAgentRuntime::default();

        runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysDeny,
                &mut |_: &str| {},
            )
            .await
            .unwrap();
        assert_eq!(*alpha.calls.lock().unwrap(), 0, "AlwaysDeny must block a Confirm-tier call");

        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "alpha".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("ok".to_string())],
        ]);
        runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &mut |_: &str| {},
            )
            .await
            .unwrap();
        assert_eq!(
            *alpha.calls.lock().unwrap(),
            1,
            "AlwaysApprove must let a Confirm-tier call run"
        );
    }

    #[tokio::test]
    async fn an_unknown_tool_call_is_reported_without_crashing() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "does_not_exist".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("ok".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![]);
        let runtime = AemeathAgentRuntime::default();

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &mut |_: &str| {},
            )
            .await
            .unwrap();
        assert_eq!(result.text, "ok");
        assert!(matches!(
            result.trace.as_slice(),
            [ToolInvocation { name, outcome: ToolOutcome::Rejected { .. } }] if name == "does_not_exist"
        ));
    }

    #[tokio::test]
    async fn text_deltas_stream_live_from_every_iteration_not_just_the_final_one() {
        // A model that narrates before calling a tool, then continues
        // after the tool result comes back, should have *both* pieces
        // of text streamed live -- not just the final iteration's.
        let provider = ScriptedProvider::new(vec![
            vec![
                ToolCallStreamItem::TextDelta("checking".to_string()),
                ToolCallStreamItem::ToolCall {
                    id: "call_0".to_string(),
                    name: "alpha".to_string(),
                    arguments: json!({}),
                },
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Auto));
        let registry = ToolRegistry::new(vec![alpha.clone()]);
        let runtime = AemeathAgentRuntime::default();
        let streamed = Arc::new(Mutex::new(String::new()));
        let streamed_in_callback = streamed.clone();

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &mut |delta: &str| streamed_in_callback.lock().unwrap().push_str(delta),
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done");
        assert_eq!(
            result.trace,
            vec![ToolInvocation {
                name: "alpha".to_string(),
                outcome: ToolOutcome::Executed { ok: true }
            }]
        );
        assert_eq!(*streamed.lock().unwrap(), "checkingdone");
    }
}
