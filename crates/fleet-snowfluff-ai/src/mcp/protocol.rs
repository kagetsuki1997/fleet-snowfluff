//! The MCP client-side protocol calls every compliant server implements
//! identically, regardless of transport (verified directly against
//! `modelcontextprotocol.io/specification/2025-06-18` during
//! exploration, not assumed): the `initialize` handshake plus the
//! `notifications/initialized` notification that must follow it, and
//! the two tool-use methods (`tools/list`, `tools/call`) this client
//! actually needs. Generic over [`McpTransport`] -- this module knows
//! nothing about subprocesses or HTTP.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{error::McpError, transport::McpTransport};
use crate::agent_tool::ToolResult;

/// The protocol version this client speaks. MCP's own version
/// negotiation is a server concern (it may downgrade), so this client
/// sends one fixed, current value and trusts the server to say if it
/// can't honor it rather than trying to negotiate itself.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// A client's own identity, sent once as part of `initialize`'s
/// `clientInfo` field.
fn client_info() -> Value {
    serde_json::json!({ "name": "fleet-snowfluff", "version": env!("CARGO_PKG_VERSION") })
}

/// One tool as a connected server's own `tools/list` response describes
/// it -- translated into a [`crate::tool_provider::ToolDefinition`] by
/// [`super::tool::McpTool::definition`], not here, since this struct is
/// purely "what the wire said," independent of how Aemeath's own
/// `Tool` trait wants it shaped.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct McpToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// One block of a `tools/call` result's `content` array. Only `Text` is
/// consumed (every tool this client cares about reports its result as
/// text); anything else (`image`, `resource`, a future content type) is
/// kept out of the model-visible result rather than failing the whole
/// call over a block type this client doesn't render.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContentBlock {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

/// A `tools/call` response: `content` (one or more blocks; a server
/// legitimately interleaves text with other block types) plus
/// `is_error`, the protocol's own way of reporting a failure *within* a
/// permitted call (its own `isError: true`, distinct from a JSON-RPC
/// error response, which [`McpTransport::request`] already turns into
/// an [`McpError`] before this struct is ever built).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct McpToolCallResult {
    #[serde(default)]
    pub content: Vec<McpContentBlock>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

impl McpToolCallResult {
    /// Flattens every `Text` block into Aemeath's own [`ToolResult`]
    /// shape -- the one place this protocol's `content`/`isError` split
    /// meets `Tool::execute`'s `ToolResult { content, is_error }` split,
    /// which happen to line up directly.
    pub fn into_tool_result(self) -> ToolResult {
        let text = self
            .content
            .into_iter()
            .filter_map(|block| match block {
                McpContentBlock::Text { text } => Some(text),
                McpContentBlock::Other => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if self.is_error {
            ToolResult::error(text)
        } else {
            ToolResult::ok(text)
        }
    }
}

#[derive(Debug, Serialize)]
struct ToolsCallParams<'a> {
    name: &'a str,
    arguments: Value,
}

/// A connected server's own tool-use surface, generic over transport --
/// the same `McpClient` is built on top of a
/// [`super::stdio_transport::StdioTransport`] or an HTTP transport without this
/// module caring which.
#[derive(Clone)]
pub struct McpClient {
    transport: Arc<dyn McpTransport>,
}

impl McpClient {
    pub fn new(transport: Arc<dyn McpTransport>) -> Self { Self { transport } }

    /// Performs the `initialize` handshake and sends the required
    /// follow-up `notifications/initialized` notification. Must be
    /// called, and succeed, before `list_tools`/`call_tool` -- nothing
    /// here enforces that ordering at the type level (keeping this
    /// client a thin, stateless wrapper over `transport`); the one
    /// caller that matters, connection setup (`connect_mcp_server`/the
    /// Settings-UI add-server path), always calls it first.
    pub async fn initialize(&self) -> Result<(), McpError> {
        let params = serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": client_info(),
        });
        self.transport.request("initialize", Some(params)).await?;
        self.transport.notify("notifications/initialized", None).await
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>, McpError> {
        let result = self.transport.request("tools/list", None).await?;
        let tools = result.get("tools").cloned().unwrap_or(Value::Array(Vec::new()));
        serde_json::from_value(tools)
            .map_err(|err| McpError(format!("invalid tools/list response: {err}")))
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<McpToolCallResult, McpError> {
        let params = serde_json::to_value(ToolsCallParams { name, arguments })
            .expect("ToolsCallParams serialization is infallible");
        let result = self.transport.request("tools/call", Some(params)).await?;
        serde_json::from_value(result)
            .map_err(|err| McpError(format!("invalid tools/call response: {err}")))
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;

    /// One call this double recorded, for assertions on exactly what a
    /// higher-level method sent.
    #[derive(Debug, Clone, PartialEq)]
    pub struct RecordedRequest {
        pub method: String,
        pub params: Option<Value>,
    }

    /// A scripted [`McpTransport`] double -- no real subprocess or
    /// socket, just records every `request`/`notify` call and returns
    /// pre-set responses in order, matching this crate's existing
    /// `ScriptedProvider` pattern (`agent_runtime.rs`'s own tests) for
    /// the same reason: exercising request *shapes* and response
    /// *parsing* without a real I/O dependency.
    #[derive(Default)]
    pub struct ScriptedTransport {
        pub requests: Mutex<Vec<RecordedRequest>>,
        pub responses: Mutex<std::collections::VecDeque<Result<Value, McpError>>>,
    }

    impl ScriptedTransport {
        pub fn with_responses(responses: Vec<Result<Value, McpError>>) -> Self {
            Self { requests: Mutex::new(Vec::new()), responses: Mutex::new(responses.into()) }
        }
    }

    #[async_trait]
    impl McpTransport for ScriptedTransport {
        async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
            self.requests
                .lock()
                .unwrap()
                .push(RecordedRequest { method: method.to_string(), params });
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("ScriptedTransport.request called more times than scripted")
        }

        async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
            self.requests
                .lock()
                .unwrap()
                .push(RecordedRequest { method: method.to_string(), params });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{test_support::ScriptedTransport, *};

    #[tokio::test]
    async fn initialize_sends_the_handshake_then_the_initialized_notification() {
        let transport = Arc::new(ScriptedTransport::with_responses(vec![Ok(
            json!({"protocolVersion": MCP_PROTOCOL_VERSION}),
        )]));
        let client = McpClient::new(transport.clone());

        client.initialize().await.unwrap();

        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "initialize");
        assert_eq!(
            requests[0].params.as_ref().unwrap()["protocolVersion"],
            json!(MCP_PROTOCOL_VERSION)
        );
        assert_eq!(requests[1].method, "notifications/initialized");
    }

    #[tokio::test]
    async fn list_tools_parses_the_tools_array_from_the_result() {
        let transport = Arc::new(ScriptedTransport::with_responses(vec![Ok(json!({
            "tools": [
                {"name": "search", "description": "Searches things", "inputSchema": {"type": "object"}},
                {"name": "no_description", "inputSchema": {"type": "object"}},
            ]
        }))]));
        let client = McpClient::new(transport.clone());

        let tools = client.list_tools().await.unwrap();

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].description.as_deref(), Some("Searches things"));
        assert_eq!(tools[1].description, None);
        assert_eq!(transport.requests.lock().unwrap()[0].method, "tools/list");
    }

    #[tokio::test]
    async fn call_tool_sends_the_name_and_arguments_and_parses_the_result() {
        let transport = Arc::new(ScriptedTransport::with_responses(vec![Ok(json!({
            "content": [{"type": "text", "text": "42"}],
            "isError": false,
        }))]));
        let client = McpClient::new(transport.clone());

        let result = client.call_tool("add", json!({"a": 40, "b": 2})).await.unwrap();

        assert_eq!(result.content, vec![McpContentBlock::Text { text: "42".to_string() }]);
        assert!(!result.is_error);
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests[0].method, "tools/call");
        assert_eq!(requests[0].params.as_ref().unwrap()["name"], json!("add"));
        assert_eq!(requests[0].params.as_ref().unwrap()["arguments"], json!({"a": 40, "b": 2}));
    }

    #[tokio::test]
    async fn a_jsonrpc_error_response_surfaces_as_an_mcp_error() {
        let transport = Arc::new(ScriptedTransport::with_responses(vec![Err(McpError(
            "Method not found (-32601)".to_string(),
        ))]));
        let client = McpClient::new(transport);

        let err = client.list_tools().await.unwrap_err();
        assert!(err.to_string().contains("-32601"));
    }

    #[test]
    fn into_tool_result_joins_text_blocks_and_skips_unknown_block_types() {
        let result = McpToolCallResult {
            content: vec![
                McpContentBlock::Text { text: "line one".to_string() },
                McpContentBlock::Other,
                McpContentBlock::Text { text: "line two".to_string() },
            ],
            is_error: false,
        };
        assert_eq!(result.into_tool_result(), ToolResult::ok("line one\nline two"));
    }

    #[test]
    fn into_tool_result_reports_is_error_as_a_tool_error() {
        let result = McpToolCallResult {
            content: vec![McpContentBlock::Text { text: "boom".to_string() }],
            is_error: true,
        };
        assert_eq!(result.into_tool_result(), ToolResult::error("boom"));
    }
}
