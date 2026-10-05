//! [`McpTransport`]: the one seam between the MCP protocol logic
//! ([`super::protocol::McpClient`]) and however a given server is
//! actually reached -- a local subprocess's stdin/stdout
//! ([`super::stdio_transport::StdioTransport`]) or an HTTP/SSE
//! endpoint. `McpClient` is written once against this trait and never
//! needs to know which.
//!
//! Deliberately request/response at this layer, not raw bytes: id
//! bookkeeping (matching a response to the request that produced it,
//! skipping anything addressed to a different id) is a transport
//! concern -- a stdio connection reads lines off one shared stream and
//! must correlate by id itself, while an HTTP transport gets the
//! correlation for free from the request/response pairing HTTP already
//! gives it. Pushing id-matching up into `McpClient` would make every
//! transport redo the same bookkeeping `McpClient` could otherwise stay
//! blind to.

use async_trait::async_trait;
use serde_json::Value;

use super::error::McpError;

/// One live connection to an MCP server, speaking JSON-RPC 2.0's two
/// message shapes: a request (expects a response) and a notification
/// (fire-and-forget, e.g. `notifications/initialized`).
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Sends a request for `method` and returns its `result` field, or
    /// an error built from the response's own `error` field (or a
    /// transport-level failure, e.g. the connection dropped).
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError>;

    /// Sends a notification -- no response is read or expected.
    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError>;
}
