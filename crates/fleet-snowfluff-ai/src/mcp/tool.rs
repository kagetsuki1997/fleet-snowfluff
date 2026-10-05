//! [`McpTool`]: one tool discovered from a connected MCP server,
//! wrapped as a plain [`Tool`] implementation -- the point of this
//! whole module. Once built, nothing in `AemeathAgentRuntime::run()`'s
//! own dispatch (`ToolRegistry::find`/`Tool::execute`) can tell an
//! `McpTool` apart from a native one (`agent-runtime`'s "Native tool
//! registry" requirement, as broadened by this change).

use async_trait::async_trait;
use serde_json::Value;

use super::protocol::{McpClient, McpToolDescriptor};
use crate::{
    agent_tool::{PermissionTier, Tool, ToolContext, ToolError, ToolResult},
    tool_provider::ToolDefinition,
};

/// One discovered tool, plus a handle to the connection it came from.
/// Cheap to clone (`McpClient` is itself a thin `Arc<dyn McpTransport>`
/// wrapper) -- a connected server's whole tool list is built as a
/// `Vec<Arc<McpTool>>` sharing one underlying connection, not one
/// connection per tool.
pub struct McpTool {
    descriptor: McpToolDescriptor,
    client: McpClient,
}

impl McpTool {
    pub fn new(descriptor: McpToolDescriptor, client: McpClient) -> Self {
        Self { descriptor, client }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.descriptor.name.clone(),
            description: self.descriptor.description.clone().unwrap_or_default(),
            parameters: self.descriptor.input_schema.clone(),
        }
    }

    /// Always `Confirm`, regardless of `args` -- MCP carries no
    /// risk-tier metadata of its own to vary this by (design.md's
    /// "Permission default: blanket per-server `Confirm`, not
    /// per-tool").
    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Confirm
    }

    /// `false`: an MCP tool's risk is unknowable per-call (no schema
    /// field says whether it writes, deletes, or is idempotent), so
    /// "always allow this tool for the rest of this session" is never
    /// offered for one -- the same reasoning `run_command` already
    /// gets this `false` default for, just for a different underlying
    /// reason (unknown risk vs. known-variable risk).
    fn allows_session_remember(&self) -> bool { false }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        match self.client.call_tool(&self.descriptor.name, args).await {
            Ok(result) => Ok(result.into_tool_result()),
            Err(err) => Err(ToolError(err.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::{conversation::ConversationId, mcp::protocol::test_support::ScriptedTransport};

    fn ctx() -> ToolContext {
        ToolContext {
            project_root: None,
            conversation_id: ConversationId::from_session_path(std::path::Path::new("/tmp/x")),
        }
    }

    fn tool(
        descriptor: McpToolDescriptor,
        responses: Vec<Result<Value, super::super::error::McpError>>,
    ) -> McpTool {
        let transport = Arc::new(ScriptedTransport::with_responses(responses));
        McpTool::new(descriptor, McpClient::new(transport))
    }

    #[test]
    fn definition_translates_the_discovered_schema() {
        let descriptor = McpToolDescriptor {
            name: "search".to_string(),
            description: Some("Searches things".to_string()),
            input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        };
        let mcp_tool = tool(descriptor.clone(), vec![]);
        let definition = mcp_tool.definition();
        assert_eq!(definition.name, "search");
        assert_eq!(definition.description, "Searches things");
        assert_eq!(definition.parameters, descriptor.input_schema);
    }

    #[test]
    fn definition_falls_back_to_an_empty_description_when_the_server_gave_none() {
        let descriptor = McpToolDescriptor {
            name: "search".to_string(),
            description: None,
            input_schema: json!({"type": "object"}),
        };
        let mcp_tool = tool(descriptor, vec![]);
        assert_eq!(mcp_tool.definition().description, "");
    }

    #[test]
    fn required_permission_is_always_confirm() {
        let descriptor = McpToolDescriptor {
            name: "search".to_string(),
            description: None,
            input_schema: json!({}),
        };
        let mcp_tool = tool(descriptor, vec![]);
        assert_eq!(mcp_tool.required_permission(&json!({}), &ctx()), PermissionTier::Confirm);
        assert_eq!(
            mcp_tool.required_permission(&json!({"anything": "at all"}), &ctx()),
            PermissionTier::Confirm
        );
    }

    #[test]
    fn allows_session_remember_is_always_false() {
        let descriptor = McpToolDescriptor {
            name: "search".to_string(),
            description: None,
            input_schema: json!({}),
        };
        assert!(!tool(descriptor, vec![]).allows_session_remember());
    }

    #[tokio::test]
    async fn execute_calls_tools_call_and_maps_the_result() {
        let descriptor = McpToolDescriptor {
            name: "search".to_string(),
            description: None,
            input_schema: json!({}),
        };
        let mcp_tool = tool(
            descriptor,
            vec![Ok(json!({"content": [{"type": "text", "text": "found it"}], "isError": false}))],
        );

        let result = mcp_tool.execute(json!({"q": "aemeath"}), &ctx()).await.unwrap();
        assert_eq!(result, ToolResult::ok("found it"));
    }

    #[tokio::test]
    async fn execute_maps_a_transport_failure_to_a_tool_error() {
        let descriptor = McpToolDescriptor {
            name: "search".to_string(),
            description: None,
            input_schema: json!({}),
        };
        let mcp_tool = tool(
            descriptor,
            vec![Err(super::super::error::McpError("connection lost".to_string()))],
        );

        let err = mcp_tool.execute(json!({}), &ctx()).await.unwrap_err();
        assert!(err.to_string().contains("connection lost"));
    }
}
