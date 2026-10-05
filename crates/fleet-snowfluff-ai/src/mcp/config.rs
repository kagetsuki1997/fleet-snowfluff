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

/// Whether a connected server's tools are actually usable yet --
/// `Pending` only ever applies to an HTTP server whose OAuth flow
/// hasn't completed (design.md: "On an OAuth flow that hasn't
/// completed yet, write a `pending` entry"); a stdio server, needing
/// no asynchronous step, goes straight to `Ready`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerStatus {
    Ready,
    Pending,
}

/// A discovered tool's own name/description, cached from `tools/list`
/// at connect time (and refreshed on demand -- Group 4.5) for the
/// Settings UI's own tools browser -- not the full JSON Schema
/// `McpTool`'s own `definition()` carries, since this is display-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolSummary {
    pub name: String,
    pub description: String,
}

/// One connected server's full persisted record: its connection
/// details, current status, and cached discovered tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerRecord {
    pub config: McpServerConfig,
    pub status: McpServerStatus,
    #[serde(default)]
    pub tools: Vec<McpToolSummary>,
}

/// The shape of `mcp-servers.json` -- a sibling of `AiSettings`'s own
/// `ai-config.json`, persisted the same way but kept in its own file
/// rather than as a field on `AiSettings` itself: connected MCP
/// servers are a distinct concern from provider/profile settings, and
/// giving them their own file means neither schema's own migrations
/// ever need to reason about the other.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServersConfig {
    #[serde(default)]
    pub servers: Vec<McpServerRecord>,
}

/// Missing/unreadable/corrupt content yields an empty config rather
/// than an error -- a brand-new install has no connected servers at
/// all, same reasoning `credentials::load_from_str`/`settings::load_from_str`
/// already use for their own files.
pub fn load_from_str(contents: &str) -> McpServersConfig {
    serde_json::from_str(contents).unwrap_or_default()
}

pub fn to_json_string(config: &McpServersConfig) -> String {
    serde_json::to_string_pretty(config).expect("McpServersConfig serialization is infallible")
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
            transport: McpServerTransportConfig::Http {
                url: "https://mcp.github.com/sse".to_string(),
            },
        };
        let json = serde_json::to_string(&config).unwrap();
        let reloaded: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(reloaded, config);
    }

    #[test]
    fn missing_file_content_yields_an_empty_config() {
        assert_eq!(load_from_str(""), McpServersConfig::default());
    }

    #[test]
    fn corrupt_json_yields_an_empty_config() {
        assert_eq!(load_from_str("{not valid json"), McpServersConfig::default());
    }

    #[test]
    fn a_ready_record_with_cached_tools_round_trips() {
        let servers = McpServersConfig {
            servers: vec![McpServerRecord {
                config: McpServerConfig {
                    id: "local-fs".to_string(),
                    display_name: "Local Filesystem".to_string(),
                    transport: McpServerTransportConfig::Stdio {
                        command: "npx".to_string(),
                        args: vec!["-y".to_string(), "server-filesystem".to_string()],
                        credential_env_var: None,
                    },
                },
                status: McpServerStatus::Ready,
                tools: vec![
                    McpToolSummary {
                        name: "read_file".to_string(),
                        description: "Reads a file".to_string(),
                    },
                    McpToolSummary {
                        name: "list_directory".to_string(),
                        description: "Lists a directory".to_string(),
                    },
                ],
            }],
        };
        let reloaded = load_from_str(&to_json_string(&servers));
        assert_eq!(reloaded, servers);
    }

    #[test]
    fn a_pending_record_with_no_tools_yet_round_trips() {
        let servers = McpServersConfig {
            servers: vec![McpServerRecord {
                config: McpServerConfig {
                    id: "github".to_string(),
                    display_name: "GitHub".to_string(),
                    transport: McpServerTransportConfig::Http {
                        url: "https://mcp.github.com/sse".to_string(),
                    },
                },
                status: McpServerStatus::Pending,
                tools: Vec::new(),
            }],
        };
        let reloaded = load_from_str(&to_json_string(&servers));
        assert_eq!(reloaded, servers);
        assert_eq!(reloaded.servers[0].status, McpServerStatus::Pending);
        assert!(reloaded.servers[0].tools.is_empty());
    }
}
