//! A client-side MCP failure -- transport-level (process/HTTP) or
//! protocol-level (a JSON-RPC error response, or a response shape that
//! doesn't match what the method's own spec promises). Kept as a
//! single string, same minimalism as [`crate::agent_tool::ToolError`]:
//! nothing downstream needs to distinguish failure kinds yet, only
//! report them.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpError(pub String);

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

impl std::error::Error for McpError {}
