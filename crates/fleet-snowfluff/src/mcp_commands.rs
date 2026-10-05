//! Settings-UI-callable commands for connected MCP servers (Group
//! 4.4/4.5): listing, removal (credential + config entry + live
//! connection, together), and refreshing a connected server's own
//! cached tools. All plain `#[tauri::command]`s -- unlike
//! `connect_mcp_server`'s own chat-triggered path, these already
//! receive `AppHandle` the normal way and never go through
//! `AgentRuntime::run()`'s dispatch at all (design.md's own Decision).

use fleet_snowfluff_ai::{
    McpServerCredential, McpServerRecord, McpServersConfig, McpToolSummary, ProviderCredentials,
};
use tauri::{AppHandle, State};

use crate::{
    mcp_connection::McpConnectionState, mcp_servers_store, secrets_store, tool_confirmation,
};

#[tauri::command]
pub fn list_mcp_servers(app: AppHandle) -> Vec<McpServerRecord> {
    mcp_servers_store::load(&app).servers
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

fn credential_value_for(credentials: &ProviderCredentials, server_id: &str) -> Option<String> {
    match credentials.mcp_server_credentials.get(server_id)? {
        McpServerCredential::EnvVar { value } => Some(value.clone()),
        McpServerCredential::OAuthToken { access_token, .. } => Some(access_token.clone()),
    }
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
