//! [`McpServerConfig`]: the non-secret, persisted shape of one
//! connected MCP server -- everything needed to (re)establish its
//! connection, *except* a credential's actual value, which lives in
//! `ProviderCredentials::mcp_server_credentials` instead (keyed by
//! this struct's own `id`), never here (design.md: credential handling
//! is kept out of anything "a user might reasonably hand-edit,
//! screenshot, or back up"). Group 4 (`mcp-client-support`'s own
//! `tasks.md`) adds this as a persisted sibling of `AiSettings`; this
//! module only defines its shape, since `McpConnectionState` (Group 3)
//! already needs it to exist to type its own reconnect-from-config
//! lookup.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Stable, user-opaque id -- the key both `McpConnectionState`'s
    /// live-connection map and `ProviderCredentials::mcp_server_credentials`
    /// use, so neither has to care about `display_name` ever changing.
    pub id: String,
    pub display_name: String,
    pub transport: McpServerTransportConfig,
}

/// Which transport a server uses and what it needs to connect --
/// credential *shape* forks here by transport (design.md: "Credential
/// shape forks by transport, following the MCP spec's own directive"),
/// but the credential *value* itself never does; both cases only name
/// where a value from `ProviderCredentials::mcp_server_credentials`
/// would go if one exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpServerTransportConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        /// The environment variable name a stored credential's value
        /// (if this server has one) is injected under at spawn time --
        /// `None` for a server that needs no credential at all.
        #[serde(default)]
        credential_env_var: Option<String>,
    },
    Http {
        url: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stdio_config_round_trips_through_json() {
        let config = McpServerConfig {
            id: "local-fs".to_string(),
            display_name: "Local Filesystem".to_string(),
            transport: McpServerTransportConfig::Stdio {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()],
                credential_env_var: None,
            },
        };
        let json = serde_json::to_string(&config).unwrap();
        let reloaded: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(reloaded, config);
    }

    #[test]
    fn an_http_config_round_trips() {
        let config = McpServerConfig {
            id: "github".to_string(),
            display_name: "GitHub".to_string(),
            transport: McpServerTransportConfig::Http { url: "https://mcp.github.com/sse".to_string() },
        };
        let json = serde_json::to_string(&config).unwrap();
        let reloaded: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(reloaded, config);
    }
}
