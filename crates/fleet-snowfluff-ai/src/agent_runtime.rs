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

use futures_util::StreamExt;
use serde_json::Value;

use crate::{
    agent_tool::{PermissionTier, Tool, ToolContext, ToolResult},
    message::{Message, ProviderError, ToolCallRecord},
    tool_provider::{ToolCallStreamItem, ToolCallingProvider, ToolDefinition},
};

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

/// Runs the tool-calling loop for a `ToolCallingProvider`. Implementors
/// never need to manage `Conversation`/session lifecycle themselves --
/// `run`'s job ends at producing the final answer text, the same
/// boundary `TaskRouter::route()` draws for itself.
#[async_trait::async_trait]
pub trait AgentRuntime: Send + Sync {
    async fn run(
        &self,
        provider: &dyn ToolCallingProvider,
        messages: Vec<Message>,
        registry: &ToolRegistry,
        ctx: &ToolContext,
        permission: &dyn PermissionDecider,
    ) -> Result<String, AgentError>;
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
    ) -> Result<String, AgentError> {
        let mut last_text = String::new();

        for _ in 0..self.max_iterations {
            let mut stream = provider
                .chat_with_tools(messages.clone(), registry.definitions())
                .await
                .map_err(AgentError::Provider)?;

            let mut text = String::new();
            let mut calls = Vec::new();
            while let Some(item) = stream.next().await {
                match item.map_err(AgentError::Provider)? {
                    ToolCallStreamItem::TextDelta(delta) => text.push_str(&delta),
                    ToolCallStreamItem::ToolCall { id, name, arguments } => {
                        calls.push(PendingToolCall { id, name, arguments })
                    }
                }
            }
            last_text = text.clone();

            if calls.is_empty() {
                return Ok(text);
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
            // run -- so they're handled in the same pass rather than a
            // separate up-front validation step.
            let mut pending_confirm: Vec<(PendingToolCall, Arc<dyn Tool>)> = Vec::new();
            for call in calls {
                let Some(tool) = registry.find(&call.name) else {
                    messages.push(Message::tool(format!("Error: unknown tool \"{}\"", call.name)));
                    continue;
                };
                if !call.arguments.is_object() {
                    messages.push(Message::tool(format!(
                        "Error: arguments for \"{}\" must be a JSON object",
                        call.name
                    )));
                    continue;
                }
                match tool.required_permission(&call.arguments, ctx) {
                    PermissionTier::Auto => {
                        let result = execute(&tool, call.arguments, ctx).await;
                        messages.push(Message::tool(tool_result_content(result)));
                    }
                    PermissionTier::Deny => {
                        messages.push(Message::tool(format!(
                            "Error: \"{}\" was not permitted to run",
                            call.name
                        )));
                    }
                    PermissionTier::Confirm => pending_confirm.push((call, tool)),
                }
            }

            if !pending_confirm.is_empty() {
                let batch: Vec<PendingToolCall> =
                    pending_confirm.iter().map(|(call, _)| call.clone()).collect();
                let approved = permission.decide(&batch).await;
                for (call, tool) in pending_confirm {
                    if approved.contains(&call.id) {
                        let result = execute(&tool, call.arguments, ctx).await;
                        messages.push(Message::tool(tool_result_content(result)));
                    } else {
                        messages.push(Message::tool(format!(
                            "Error: \"{}\" was not permitted to run",
                            call.name
                        )));
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
    }

    impl ScriptedProvider {
        fn new(responses: Vec<Vec<ToolCallStreamItem>>) -> Self {
            Self { responses: Mutex::new(responses.into()) }
        }

        /// Never runs out -- every call gets the same tool-call
        /// response, for testing that the loop terminates via the
        /// iteration limit rather than the script running dry.
        fn repeating(response: Vec<ToolCallStreamItem>) -> Self {
            let mut queue = std::collections::VecDeque::new();
            for _ in 0..100 {
                queue.push_back(response.clone());
            }
            Self { responses: Mutex::new(queue) }
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
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<ToolCallStream, ProviderError> {
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
            .run(&provider, vec![Message::user("hi")], &registry, &ctx(), &AlwaysApprove)
            .await
            .unwrap();

        assert_eq!(result, "done");
        assert_eq!(*alpha.calls.lock().unwrap(), 1);
        assert_eq!(*beta.calls.lock().unwrap(), 1);
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
            runtime.run(&provider, vec![Message::user("hi")], &registry, &ctx(), &AlwaysApprove),
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
            .run(&provider, vec![Message::user("hi")], &registry, &ctx(), &AlwaysApprove)
            .await
            .unwrap();

        assert_eq!(result, "ok");
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
            .run(&provider, vec![Message::user("hi")], &registry, &ctx(), &AlwaysDeny)
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
            .run(&provider, vec![Message::user("hi")], &registry, &ctx(), &AlwaysApprove)
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
            .run(&provider, vec![Message::user("hi")], &registry, &ctx(), &AlwaysApprove)
            .await
            .unwrap();
        assert_eq!(result, "ok");
    }
}
