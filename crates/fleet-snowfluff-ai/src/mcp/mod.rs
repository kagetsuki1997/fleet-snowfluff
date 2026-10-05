//! Aemeath as an MCP *client* (`mcp-client-support`) -- connecting out
//! to third-party MCP servers, discovering their tools via the
//! protocol's own fixed `initialize`/`tools/list` methods, and calling
//! them the same way a native tool is called. Deliberately only the
//! consuming direction; see this change's `proposal.md` for why the
//! reverse (Aemeath as a server) is out of scope.

pub mod config;
pub mod error;
pub mod http_transport;
pub mod oauth;
pub mod protocol;
pub mod stdio_transport;
pub mod tool;
pub mod transport;

pub use config::{McpServerConfig, McpServerTransportConfig};
pub use error::McpError;
pub use http_transport::HttpTransport;
pub use oauth::{
    build_authorization_url, discover, exchange_code_for_token, generate_pkce, generate_state,
    probe_authorization, register_client_or_explain, AuthProbeOutcome, AuthorizationServerMetadata,
    DynamicClientRegistration, PkceChallenge, RedirectListener, TokenResponse,
};
pub use protocol::{McpClient, McpContentBlock, McpToolCallResult, McpToolDescriptor};
pub use stdio_transport::StdioTransport;
pub use tool::McpTool;
pub use transport::McpTransport;
