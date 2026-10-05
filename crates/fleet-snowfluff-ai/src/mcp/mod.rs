//! Aemeath as an MCP *client* (`mcp-client-support`) -- connecting out
//! to third-party MCP servers, discovering their tools via the
//! protocol's own fixed `initialize`/`tools/list` methods, and calling
//! them the same way a native tool is called. Deliberately only the
//! consuming direction; see this change's `proposal.md` for why the
//! reverse (Aemeath as a server) is out of scope.

pub mod error;
pub mod protocol;
pub mod stdio_transport;
pub mod tool;
pub mod transport;

pub use error::McpError;
pub use protocol::{McpClient, McpContentBlock, McpToolCallResult, McpToolDescriptor};
pub use stdio_transport::StdioTransport;
pub use tool::McpTool;
pub use transport::McpTransport;
