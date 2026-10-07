//! Loads and saves `mcp-servers.json` (`McpServersConfig`) at the
//! platform config directory, mirroring `ai_config_store.rs`'s own
//! pattern -- kept as its own file, a sibling of `AiSettings`'s own
//! `ai-config.json` rather than a field on it, since connected MCP
//! servers are a distinct concern (design.md's own "mcp_servers config
//! sibling to AiSettings, persisted the same way").

use fleet_snowfluff_ai::{mcp::config, McpServersConfig};
use tauri::{AppHandle, Manager};

pub fn mcp_servers_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("mcp-servers.json"))
}

pub fn load(app: &AppHandle) -> McpServersConfig {
    let Some(path) = mcp_servers_path(app) else { return McpServersConfig::default() };
    match std::fs::read_to_string(&path) {
        Ok(contents) => config::load_from_str(&contents),
        Err(_) => McpServersConfig::default(),
    }
}

/// Writes `servers` to disk, creating the config directory if needed.
/// Best-effort: a write failure is logged, not propagated, same as
/// `ai_config_store::save`/`config_store::save`.
pub fn save(app: &AppHandle, servers: &McpServersConfig) {
    let Some(path) = mcp_servers_path(app) else {
        log::error!("could not resolve app config directory; MCP server config will not persist");
        return;
    };
    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            log::error!("failed to create config dir {dir:?}: {err}");
            return;
        }
    }
    if let Err(err) = std::fs::write(&path, config::to_json_string(servers)) {
        log::error!("failed to write mcp-servers config to {path:?}: {err}");
    }
}
