//! Settings-UI-callable commands for connected MCP servers (Groups
//! 4.4/4.5 and 5.2/5.3): listing, adding a server manually, removal
//! (credential + config entry + live connection, together), refreshing
//! a connected server's own cached tools, and submitting a manually
//! pasted OAuth code. All plain `#[tauri::command]`s -- unlike
//! `connect_mcp_server`'s own chat-triggered path, these already
//! receive `AppHandle` the normal way and never go through
//! `AgentRuntime::run()`'s dispatch at all (design.md's own Decision).

use fleet_snowfluff_ai::{
    McpConnectOutcome, McpConnectRequest, McpServerConfig, McpServerCredential, McpServerRecord,
    McpServerTransportConfig, McpServersConfig, McpToolSummary, ProviderCredentials,
};
use tauri::{AppHandle, Manager, State};

use crate::{
    mcp_connection::{McpConnectionState, PendingOAuthState},
    mcp_servers_store, secrets_store, tool_confirmation,
};

/// Whether `request` resolves to the exact same server `config`
/// already does -- same URL for HTTP, same command+args for stdio.
/// Credentials and display name deliberately don't factor in: two
/// requests naming the same address/command are the same server
/// regardless of what the model happened to call it this time.
pub(crate) fn resolves_to_same_server(
    request: &McpConnectRequest,
    config: &McpServerConfig,
) -> bool {
    match (request, &config.transport) {
        (McpConnectRequest::Http { url }, McpServerTransportConfig::Http { url: existing_url }) => {
            url == existing_url
        }
        (
            McpConnectRequest::Stdio { command, args, .. },
            McpServerTransportConfig::Stdio {
                command: existing_command, args: existing_args, ..
            },
        ) => command == existing_command && args == existing_args,
        _ => false,
    }
}

/// The already-connected server (if any) `request` resolves to --
/// checked *before* attempting anything, so a model asking to connect
/// to something already connected gets told so directly
/// (`McpConnectOutcome::AlreadyConnected`) instead of a fresh
/// connection attempt (or, for HTTP, a redundant OAuth dance) running
/// silently against a server that didn't need it.
pub(crate) fn find_matching_server<'a>(
    servers: &'a McpServersConfig,
    request: &McpConnectRequest,
) -> Option<&'a McpServerRecord> {
    servers.servers.iter().find(|record| resolves_to_same_server(request, &record.config))
}

#[tauri::command]
pub fn list_mcp_servers(app: AppHandle) -> Vec<McpServerRecord> {
    mcp_servers_store::load(&app).servers
}

/// The manual "add server" path (Group 5.2): the user filling in the
/// Settings UI's own form and submitting it already *is* their
/// explicit approval -- the same bar `connect_mcp_server`'s own
/// `Confirm`-tier popup exists to clear for the chat-triggered path --
/// so this calls `attempt_connection` directly rather than routing
/// through `PopupMcpConnector`'s own (redundant, here) confirmation
/// step.
#[tauri::command]
pub async fn add_mcp_server(app: AppHandle, request: McpConnectRequest) -> McpConnectOutcome {
    tool_confirmation::attempt_connection(&app, request).await
}

/// The manual "paste the code" fallback (Group 5.3, design.md's own
/// 2.4) for when the loopback redirect can't be used. Competes with
/// the redirect listener to finish the same attempt -- whichever calls
/// `PendingOAuthState::take` first wins; the other finds nothing left
/// to do.
#[tauri::command]
pub async fn submit_mcp_oauth_code(
    app: AppHandle,
    pending: State<'_, PendingOAuthState>,
    server_id: String,
    code: String,
) -> Result<McpConnectOutcome, String> {
    match pending.take(&server_id).await {
        Some(attempt) => Ok(tool_confirmation::finish_oauth(&app, attempt, &code).await),
        None => Err(format!(
            "no pending authorization for server \"{server_id}\" -- it may have already finished, \
             or this attempt is no longer active"
        )),
    }
}

/// The pure part of removal: drops `server_id`'s stored credential (if
/// any) and its persisted config entry. The third part -- tearing down
/// the live connection -- is `McpConnectionState::remove`, already
/// tested in Group 3; this function covers the two parts that don't
/// need `AppHandle` to express or test.
fn remove_server_data(
    credentials: &mut ProviderCredentials,
    servers: &mut McpServersConfig,
    server_id: &str,
) {
    credentials.mcp_server_credentials.remove(server_id);
    servers.servers.retain(|record| record.config.id != server_id);
}

#[tauri::command]
pub async fn remove_mcp_server(
    app: AppHandle,
    connections: State<'_, McpConnectionState>,
    server_id: String,
) -> Result<(), ()> {
    // Torn down first: an in-flight call racing this removal should
    // find no connection to use, not a half-removed one.
    connections.remove(&server_id).await;

    let mut credentials = secrets_store::load(&app);
    let mut servers = mcp_servers_store::load(&app);
    remove_server_data(&mut credentials, &mut servers, &server_id);
    secrets_store::save(&app, &credentials);
    mcp_servers_store::save(&app, &servers);
    Ok(())
}

/// The pure part of a tools refresh: replaces `server_id`'s own cached
/// `tools` in `servers`, if it's there. Returns whether a matching
/// record was actually found -- fetching the live `tools/list` result
/// to pass in here is the one part that does need `AppHandle`-backed
/// state (the live connection) to reach.
fn update_cached_tools(
    servers: &mut McpServersConfig,
    server_id: &str,
    tools: Vec<McpToolSummary>,
) -> bool {
    match servers.servers.iter_mut().find(|record| record.config.id == server_id) {
        Some(record) => {
            record.tools = tools;
            true
        }
        None => false,
    }
}

/// The pure part of toggling one tool's enabled state for a server:
/// inserts into or removes from `disabled_tools` depending on
/// `enabled`. Returns whether a matching server record was found.
fn set_tool_enabled(
    servers: &mut McpServersConfig,
    server_id: &str,
    tool_name: &str,
    enabled: bool,
) -> bool {
    let Some(record) = servers.servers.iter_mut().find(|record| record.config.id == server_id)
    else {
        return false;
    };
    if enabled {
        record.disabled_tools.remove(tool_name);
    } else {
        record.disabled_tools.insert(tool_name.to_string());
    }
    true
}

/// Settings-UI-callable (Group: tool-list-optimization's own "static
/// filtering" option): lets the user turn off individual tools a
/// connected server exposes, without disconnecting the server itself
/// or losing the rest of its tools. A disabled tool is excluded
/// entirely from a chat turn's registry -- `mcp_sourced_tools` never
/// offers it, eagerly or lazily.
#[tauri::command]
pub async fn set_mcp_tool_enabled(
    app: AppHandle,
    server_id: String,
    tool_name: String,
    enabled: bool,
) -> Result<(), String> {
    let mut servers = mcp_servers_store::load(&app);
    if !set_tool_enabled(&mut servers, &server_id, &tool_name, enabled) {
        return Err(format!("no connected server with id \"{server_id}\""));
    }
    mcp_servers_store::save(&app, &servers);
    Ok(())
}

pub(crate) fn credential_value_for(
    credentials: &ProviderCredentials,
    server_id: &str,
) -> Option<String> {
    match credentials.mcp_server_credentials.get(server_id)? {
        McpServerCredential::EnvVar { value } => Some(value.clone()),
        McpServerCredential::OAuthToken { access_token, .. } => Some(access_token.clone()),
    }
}

/// Every tool a connected, `Ready` MCP server currently exposes, built
/// fresh from a live `tools/list` call each time -- this is the piece
/// that was actually missing end to end: `McpServerRecord::tools` is
/// only a *cached* name/description for the Settings UI's own browser
/// (no JSON Schema in it), so a real chat turn re-fetches the full
/// descriptor here rather than trying to call a tool from the cache.
/// A server that's `Pending`, or whose connection fails right now, is
/// skipped (logged, not fatal) rather than failing the whole turn over
/// one unreachable server.
pub(crate) async fn mcp_sourced_tools(
    app: &AppHandle,
) -> Vec<std::sync::Arc<dyn fleet_snowfluff_ai::Tool>> {
    use fleet_snowfluff_ai::{McpServerStatus, McpTool};

    let servers = mcp_servers_store::load(app);
    let credentials = secrets_store::load(app);
    let connections = app.state::<McpConnectionState>();

    let mut tools: Vec<std::sync::Arc<dyn fleet_snowfluff_ai::Tool>> = Vec::new();
    for record in servers.servers {
        if record.status != McpServerStatus::Ready {
            continue;
        }
        let credential = credential_value_for(&credentials, &record.config.id);
        let client = match connections.connection_for(&record.config, credential.as_deref()).await {
            Ok(client) => client,
            Err(err) => {
                log::warn!(
                    "skipping MCP server \"{}\" for this turn -- failed to (re)connect: {err}",
                    record.config.display_name
                );
                continue;
            }
        };
        match client.list_tools().await {
            Ok(descriptors) => {
                for descriptor in descriptors {
                    if record.disabled_tools.contains(&descriptor.name) {
                        continue;
                    }
                    tools.push(std::sync::Arc::new(McpTool::new(descriptor, (*client).clone())));
                }
            }
            Err(err) => {
                log::warn!(
                    "skipping MCP server \"{}\" for this turn -- tools/list failed: {err}",
                    record.config.display_name
                );
            }
        }
    }
    tools
}

#[tauri::command]
pub async fn refresh_mcp_server_tools(
    app: AppHandle,
    connections: State<'_, McpConnectionState>,
    server_id: String,
) -> Result<Vec<McpToolSummary>, String> {
    let mut servers = mcp_servers_store::load(&app);
    let Some(record) = servers.servers.iter().find(|record| record.config.id == server_id) else {
        return Err(format!("no connected server with id \"{server_id}\""));
    };
    let config = record.config.clone();
    let credential = credential_value_for(&secrets_store::load(&app), &server_id);

    let client = connections
        .connection_for(&config, credential.as_deref())
        .await
        .map_err(|err| err.to_string())?;
    let tools = client
        .list_tools()
        .await
        .map(tool_confirmation::summarize_tools)
        .map_err(|err| err.to_string())?;

    update_cached_tools(&mut servers, &server_id, tools.clone());
    mcp_servers_store::save(&app, &servers);
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use fleet_snowfluff_ai::{McpServerConfig, McpServerStatus, McpServerTransportConfig};

    use super::*;

    fn record(id: &str) -> McpServerRecord {
        McpServerRecord {
            config: McpServerConfig {
                id: id.to_string(),
                display_name: id.to_string(),
                transport: McpServerTransportConfig::Http {
                    url: "https://example.com".to_string(),
                },
            },
            status: McpServerStatus::Ready,
            tools: Vec::new(),
            disabled_tools: std::collections::HashSet::new(),
        }
    }

    #[test]
    fn remove_server_data_drops_both_the_credential_and_the_config_entry() {
        let mut credentials = ProviderCredentials::default();
        credentials.mcp_server_credentials.insert(
            "github".to_string(),
            McpServerCredential::OAuthToken { access_token: "at".to_string(), refresh_token: None },
        );
        let mut servers = McpServersConfig { servers: vec![record("github"), record("other")] };

        remove_server_data(&mut credentials, &mut servers, "github");

        assert!(
            !credentials.mcp_server_credentials.contains_key("github"),
            "the credential must be gone, not just the config entry"
        );
        assert_eq!(servers.servers.len(), 1);
        assert_eq!(servers.servers[0].config.id, "other");
    }

    #[test]
    fn remove_server_data_is_a_harmless_no_op_for_an_unknown_id() {
        let mut credentials = ProviderCredentials::default();
        let mut servers = McpServersConfig { servers: vec![record("other")] };

        remove_server_data(&mut credentials, &mut servers, "never-connected");

        assert_eq!(servers.servers.len(), 1);
    }

    #[test]
    fn update_cached_tools_replaces_an_existing_servers_tool_list() {
        let mut servers = McpServersConfig { servers: vec![record("github")] };
        let new_tools = vec![McpToolSummary {
            name: "search_issues".to_string(),
            description: "Searches issues".to_string(),
        }];

        let updated = update_cached_tools(&mut servers, "github", new_tools.clone());

        assert!(updated);
        assert_eq!(servers.servers[0].tools, new_tools);
    }

    #[test]
    fn update_cached_tools_reports_false_for_an_unknown_server() {
        let mut servers = McpServersConfig { servers: vec![record("github")] };
        assert!(!update_cached_tools(&mut servers, "never-connected", Vec::new()));
        assert!(
            servers.servers[0].tools.is_empty(),
            "the known server's own tools must be untouched"
        );
    }

    #[test]
    fn set_tool_enabled_disables_and_reenables_a_tool() {
        let mut servers = McpServersConfig { servers: vec![record("local-fs")] };

        assert!(set_tool_enabled(&mut servers, "local-fs", "delete_file", false));
        assert!(servers.servers[0].disabled_tools.contains("delete_file"));

        assert!(set_tool_enabled(&mut servers, "local-fs", "delete_file", true));
        assert!(!servers.servers[0].disabled_tools.contains("delete_file"));
    }

    #[test]
    fn set_tool_enabled_reports_false_for_an_unknown_server() {
        let mut servers = McpServersConfig { servers: vec![record("local-fs")] };
        assert!(!set_tool_enabled(&mut servers, "never-connected", "any_tool", false));
    }

    #[test]
    fn resolves_to_same_server_matches_http_by_url_only() {
        let config = McpServerConfig {
            id: "notion".to_string(),
            display_name: "Notion".to_string(),
            transport: McpServerTransportConfig::Http {
                url: "https://mcp.notion.com/mcp".to_string(),
            },
        };
        assert!(resolves_to_same_server(
            &McpConnectRequest::Http { url: "https://mcp.notion.com/mcp".to_string() },
            &config
        ));
        assert!(!resolves_to_same_server(
            &McpConnectRequest::Http { url: "https://mcp.github.com/sse".to_string() },
            &config
        ));
    }

    #[test]
    fn resolves_to_same_server_matches_stdio_by_command_and_args_not_credentials() {
        let config = McpServerConfig {
            id: "local-fs".to_string(),
            display_name: "Local Filesystem".to_string(),
            transport: McpServerTransportConfig::Stdio {
                command: "codebase-memory-mcp".to_string(),
                args: vec![],
                credential_env_var: None,
            },
        };
        assert!(
            resolves_to_same_server(
                &McpConnectRequest::Stdio {
                    command: "codebase-memory-mcp".to_string(),
                    args: vec![],
                    credential_env_var: Some("SOME_TOKEN".to_string()),
                    credential_value: Some("different-each-time".to_string()),
                },
                &config
            ),
            "a differing credential must not prevent recognizing the same server"
        );
        assert!(!resolves_to_same_server(
            &McpConnectRequest::Stdio {
                command: "codebase-memory-mcp".to_string(),
                args: vec!["--verbose".to_string()],
                credential_env_var: None,
                credential_value: None,
            },
            &config
        ));
    }

    #[test]
    fn resolves_to_same_server_never_matches_across_transports() {
        let config = McpServerConfig {
            id: "x".to_string(),
            display_name: "x".to_string(),
            transport: McpServerTransportConfig::Http { url: "https://x.com".to_string() },
        };
        assert!(!resolves_to_same_server(
            &McpConnectRequest::Stdio {
                command: "x".to_string(),
                args: vec![],
                credential_env_var: None,
                credential_value: None,
            },
            &config
        ));
    }

    #[test]
    fn find_matching_server_locates_the_already_connected_record() {
        let notion = McpServerRecord {
            config: McpServerConfig {
                id: "notion".to_string(),
                display_name: "Notion".to_string(),
                transport: McpServerTransportConfig::Http {
                    url: "https://mcp.notion.com/mcp".to_string(),
                },
            },
            status: McpServerStatus::Ready,
            tools: vec![McpToolSummary {
                name: "notion-fetch".to_string(),
                description: String::new(),
            }],
            disabled_tools: std::collections::HashSet::new(),
        };
        let servers = McpServersConfig { servers: vec![record("other"), notion] };

        let found = find_matching_server(
            &servers,
            &McpConnectRequest::Http { url: "https://mcp.notion.com/mcp".to_string() },
        );
        assert_eq!(found.map(|r| r.config.id.as_str()), Some("notion"));

        let not_found = find_matching_server(
            &servers,
            &McpConnectRequest::Http { url: "https://never-connected.example.com".to_string() },
        );
        assert!(not_found.is_none());
    }

    #[test]
    fn credential_value_for_reads_either_credential_shape() {
        let mut credentials = ProviderCredentials::default();
        credentials.mcp_server_credentials.insert(
            "local-fs".to_string(),
            McpServerCredential::EnvVar { value: "env-secret".to_string() },
        );
        credentials.mcp_server_credentials.insert(
            "github".to_string(),
            McpServerCredential::OAuthToken {
                access_token: "at-123".to_string(),
                refresh_token: Some("rt-456".to_string()),
            },
        );

        assert_eq!(credential_value_for(&credentials, "local-fs"), Some("env-secret".to_string()));
        assert_eq!(credential_value_for(&credentials, "github"), Some("at-123".to_string()));
        assert_eq!(credential_value_for(&credentials, "never-connected"), None);
    }
}
