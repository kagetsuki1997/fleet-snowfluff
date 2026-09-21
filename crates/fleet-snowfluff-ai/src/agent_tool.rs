//! The `Tool` trait `AemeathAgentRuntime` (`agent_runtime.rs`) calls
//! into for native tools (`agent-core-and-task-router`'s Group 4/5).
//! Lives alongside `ToolCallingProvider`/`ConversationId` in this crate
//! (moved from the app crate after Group 5, once it was clear every
//! native tool's own implementation -- process spawning, file I/O, an
//! HTTP client -- fit this crate's existing "things the Agent/AI layer
//! does to execute a request" boundary at least as well as
//! `ClaudeCodeCli`/`Codex`'s own subprocess spawning already does) --
//! `ToolContext` needs `ConversationId`, which lives here too
//! (`conversation.rs`) for exactly that reason. No `AppHandle`/Tauri
//! type anywhere in this module or its callers.

use std::path::PathBuf;

use serde_json::Value;

use crate::{conversation::ConversationId, tool_provider::ToolDefinition};

/// How a tool call may proceed, decided per-call (not per-tool) by
/// [`Tool::required_permission`] -- see design.md's "`Tool` trait has
/// an args-aware `required_permission`" for why this can't be a static
/// per-tool-name table (`read_file`/`list_directory`'s tier depends on
/// the requested path, not just tool identity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionTier {
    Auto,
    Confirm,
    Deny,
}

/// Everything a [`Tool::execute`] call needs about where it's running.
/// Deliberately no `AppHandle`/Tauri-specific type -- all popup/window/
/// oneshot orchestration for a `Confirm`-tier call lives in the Agent
/// Loop's own calling code (Group 6), not here, so `Tool`
/// implementations stay as portable/testable as `AiProvider` already
/// is.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub project_root: Option<PathBuf>,
    pub conversation_id: ConversationId,
}

/// A tool's own answer, distinct from whether the *call itself* was
/// permitted to run at all (that's the Agent Loop's job, decided
/// before `execute` is ever invoked). `is_error` reports a failure
/// *within* a permitted execution (e.g. a file that doesn't exist) --
/// something the model should see and can react to, not something the
/// Agent Loop needs to treat specially.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true }
    }
}

/// A tool failed to execute at all -- distinct from [`ToolResult::error`],
/// which is a normal, model-visible failure *within* a permitted
/// execution. Kept minimal: Group 5's concrete tools (timeout, I/O
/// failure, etc.) decide their own messages; nothing here needs to
/// distinguish failure kinds yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(pub String);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

impl std::error::Error for ToolError {}

/// One native tool the Agent Loop can offer a tool-calling-capable
/// provider. `Send + Sync` so tools can be held behind `Arc<dyn Tool>`
/// in a `ToolRegistry` shared across an async loop.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    fn required_permission(&self, args: &Value, ctx: &ToolContext) -> PermissionTier;

    /// Whether the Agent Loop's confirmation UI may offer "always allow
    /// this tool for the rest of this session" after this tool needs
    /// confirmation once. `false` by default -- a tool must opt in,
    /// since remembering a decision at the tool-name level (not per-
    /// argument) is only safe for tools whose risk doesn't vary
    /// call-to-call (Group 6 excludes `run_command` and any
    /// write-capable tool from this).
    fn allows_session_remember(&self) -> bool { false }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError>;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A trivial tool used only to exercise `Tool`'s own contract in
    /// isolation -- always `Auto`, echoes its `text` argument back.
    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "echo".to_string(),
                description: "Echoes the given text back".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"],
                }),
            }
        }

        fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
            PermissionTier::Auto
        }

        fn allows_session_remember(&self) -> bool { true }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
            match args.get("text").and_then(Value::as_str) {
                Some(text) => Ok(ToolResult::ok(text.to_string())),
                None => Err(ToolError("missing \"text\" argument".to_string())),
            }
        }
    }

    fn context() -> ToolContext {
        ToolContext {
            project_root: None,
            conversation_id: ConversationId::from_session_path(std::path::Path::new(
                "/tmp/does-not-matter",
            )),
        }
    }

    #[test]
    fn required_permission_and_allows_session_remember_report_as_configured() {
        let tool = EchoTool;
        assert_eq!(tool.required_permission(&json!({}), &context()), PermissionTier::Auto);
        assert!(tool.allows_session_remember());
    }

    #[test]
    fn allows_session_remember_defaults_to_false() {
        struct MinimalTool;

        #[async_trait::async_trait]
        impl Tool for MinimalTool {
            fn definition(&self) -> ToolDefinition {
                ToolDefinition {
                    name: "minimal".to_string(),
                    description: String::new(),
                    parameters: json!({}),
                }
            }

            fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
                PermissionTier::Deny
            }

            async fn execute(
                &self,
                _args: Value,
                _ctx: &ToolContext,
            ) -> Result<ToolResult, ToolError> {
                Ok(ToolResult::ok(""))
            }
        }

        assert!(!MinimalTool.allows_session_remember());
    }

    #[tokio::test]
    async fn execute_echoes_the_text_argument() {
        let tool = EchoTool;
        let result = tool.execute(json!({"text": "hello"}), &context()).await.unwrap();
        assert_eq!(result, ToolResult::ok("hello"));
    }

    #[tokio::test]
    async fn execute_reports_a_model_visible_error_for_missing_arguments() {
        let tool = EchoTool;
        let err = tool.execute(json!({}), &context()).await.unwrap_err();
        assert_eq!(err.to_string(), "missing \"text\" argument");
    }
}
