//! Loads and saves `ai-config.json` (`AiSettings`) at the platform
//! config directory, mirroring `config_store.rs`'s pattern.
//! `fleet_snowfluff_ai::settings` has the pure sanitize/serialize
//! logic and is fully tested against string content; this module is
//! just the thin file-I/O layer that logic was deliberately kept
//! decoupled from.

use fleet_snowfluff_ai::{settings, AiSettings};
use tauri::{AppHandle, Manager};

pub fn ai_config_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("ai-config.json"))
}

/// Loads and sanitizes the AI config file, or `AiSettings::default()`
/// (AI disabled, no provider configured) if it's missing or corrupt.
pub fn load(app: &AppHandle) -> AiSettings {
    let Some(path) = ai_config_path(app) else { return AiSettings::default() };
    match std::fs::read_to_string(&path) {
        Ok(contents) => settings::load_from_str(&contents),
        Err(_) => AiSettings::default(),
    }
}

/// Writes `settings` to disk, creating the config directory if needed.
/// Best-effort: a write failure is logged, not propagated, same as
/// `config_store::save`.
pub fn save(app: &AppHandle, ai_settings: &AiSettings) {
    let Some(path) = ai_config_path(app) else {
        log::error!("could not resolve app config directory; AI settings will not persist");
        return;
    };
    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            log::error!("failed to create config dir {dir:?}: {err}");
            return;
        }
    }
    if let Err(err) = std::fs::write(&path, settings::to_json_string(ai_settings)) {
        log::error!("failed to write ai-config to {path:?}: {err}");
    }
}
