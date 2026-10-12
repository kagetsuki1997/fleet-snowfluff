//! `compact_context`: lets the assistant ask for the older part of the
//! conversation to be summarized, optionally naming what the summary must
//! preserve. Offered to the top-level agent only (it changes the conversation's
//! stored summary), never to a delegated sub-task or to mixed mode's read-only
//! local attempt.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{
    agent_tool::{PermissionTier, Tool, ToolContext, ToolError, ToolResult},
    compaction::{CompactionOutcome, CompactionRequester},
    tool_provider::ToolDefinition,
};

pub const COMPACT_CONTEXT_TOOL_NAME: &str = "compact_context";

pub struct CompactContextTool {
    session_path: PathBuf,
    requester: Arc<dyn CompactionRequester>,
}

impl CompactContextTool {
    pub fn new(session_path: PathBuf, requester: Arc<dyn CompactionRequester>) -> Self {
        Self { session_path, requester }
    }
}

#[async_trait]
impl Tool for CompactContextTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: COMPACT_CONTEXT_TOOL_NAME.to_string(),
            description: "Summarizes the older part of this conversation to make room. Use it \
                          when the conversation has become long. Optionally say what the summary \
                          must preserve."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "focus": {
                        "type": "string",
                        "description": "What the summary must be sure to keep, if anything."
                    }
                }
            }),
        }
    }

    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Auto
    }

    fn available_to_children(&self) -> bool { false }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let focus =
            args.get("focus").and_then(Value::as_str).filter(|focus| !focus.trim().is_empty());
        Ok(match self.requester.compact(&self.session_path, focus).await {
            Ok(CompactionOutcome::Updated(_)) => {
                ToolResult::ok("The older part of the conversation has been summarized.")
            }
            Ok(CompactionOutcome::NothingToFold) => {
                ToolResult::ok("Nothing needed summarizing yet; the conversation still fits.")
            }
            Err(_) => ToolResult::error(
                "The conversation could not be summarized right now. It is unchanged.",
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Mutex};

    use super::*;
    use crate::{compaction::CompactionError, summary::ConversationSummary};

    struct ScriptedRequester {
        reply: Mutex<Option<Result<CompactionOutcome, CompactionError>>>,
        focus_seen: Mutex<Option<Option<String>>>,
    }

    impl ScriptedRequester {
        fn new(reply: Result<CompactionOutcome, CompactionError>) -> Arc<Self> {
            Arc::new(Self { reply: Mutex::new(Some(reply)), focus_seen: Mutex::new(None) })
        }
    }

    #[async_trait]
    impl CompactionRequester for ScriptedRequester {
        async fn compact(
            &self,
            _session_path: &Path,
            focus: Option<&str>,
        ) -> Result<CompactionOutcome, CompactionError> {
            *self.focus_seen.lock().unwrap() = Some(focus.map(str::to_string));
            self.reply.lock().unwrap().take().expect("called once")
        }
    }

    fn ctx() -> ToolContext {
        ToolContext {
            project_root: None,
            conversation_id: crate::conversation::ConversationId::from_session_path(Path::new(
                "/tmp/x",
            )),
        }
    }

    fn tool(requester: Arc<ScriptedRequester>) -> CompactContextTool {
        CompactContextTool::new(PathBuf::from("session.jsonl"), requester)
    }

    fn updated() -> CompactionOutcome {
        CompactionOutcome::Updated(ConversationSummary {
            covered_through: 4,
            text: "notes".into(),
            updated_at: "now".into(),
            last_covered_timestamp: None,
        })
    }

    #[tokio::test]
    async fn the_focus_hint_reaches_the_compaction() {
        let requester = ScriptedRequester::new(Ok(updated()));
        let result = tool(requester.clone())
            .execute(json!({"focus": "the trip dates"}), &ctx())
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(*requester.focus_seen.lock().unwrap(), Some(Some("the trip dates".to_string())));
    }

    #[tokio::test]
    async fn a_blank_focus_is_treated_as_none() {
        let requester = ScriptedRequester::new(Ok(updated()));
        tool(requester.clone()).execute(json!({"focus": "   "}), &ctx()).await.unwrap();
        assert_eq!(*requester.focus_seen.lock().unwrap(), Some(None));
    }

    #[tokio::test]
    async fn a_failed_compaction_is_reported_as_not_done() {
        let requester = ScriptedRequester::new(Err(CompactionError::Writer("down".into())));
        let result = tool(requester).execute(json!({}), &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("unchanged"));
    }

    #[tokio::test]
    async fn nothing_to_summarize_is_not_an_error() {
        let requester = ScriptedRequester::new(Ok(CompactionOutcome::NothingToFold));
        let result = tool(requester).execute(json!({}), &ctx()).await.unwrap();
        assert!(!result.is_error);
    }

    #[test]
    fn the_tool_is_auto_tier_and_withheld_from_children() {
        let t = tool(ScriptedRequester::new(Ok(updated())));
        assert_eq!(t.required_permission(&json!({}), &ctx()), PermissionTier::Auto);
        assert!(!t.available_to_children());
    }

    #[test]
    fn the_schema_takes_only_an_optional_focus() {
        let definition = tool(ScriptedRequester::new(Ok(updated()))).definition();
        assert_eq!(definition.name, "compact_context");
        assert!(definition.parameters["properties"]["focus"].is_object());
        assert!(definition.parameters.get("required").is_none());
    }
}
