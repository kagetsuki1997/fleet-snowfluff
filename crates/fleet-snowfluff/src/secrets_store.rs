//! Loads and saves `secrets.json` (`ProviderCredentials`) at the
//! platform config directory, applying restrictive permissions on
//! unix where the OS supports it (`ai-provider`'s "Separate credential
//! storage"). Kept out of `ai-config.json` entirely so a config file a
//! user might reasonably hand-edit or screenshot never contains a key.

use fleet_snowfluff_ai::{credentials, ProviderCredentials};
use tauri::{AppHandle, Manager};

pub fn secrets_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("secrets.json"))
}

pub fn load(app: &AppHandle) -> ProviderCredentials {
    let Some(path) = secrets_path(app) else { return ProviderCredentials::default() };
    match std::fs::read_to_string(&path) {
        Ok(contents) => credentials::load_from_str(&contents),
        Err(_) => ProviderCredentials::default(),
    }
}

pub fn save(app: &AppHandle, creds: &ProviderCredentials) {
    let Some(path) = secrets_path(app) else {
        log::error!("could not resolve app config directory; credentials will not persist");
        return;
    };
    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            log::error!("failed to create config dir {dir:?}: {err}");
            return;
        }
    }
    if let Err(err) = std::fs::write(&path, credentials::to_json_string(creds)) {
        log::error!("failed to write secrets to {path:?}: {err}");
        return;
    }
    restrict_permissions(&path);
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        log::warn!("failed to restrict permissions on {path:?}: {err}");
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {
    // No portable equivalent applied here yet (Windows ACLs, in
    // particular, are a different enough model that this is a known
    // gap per design.md rather than something silently pretended-at).
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("fleet-snowfluff-secrets-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn restrict_permissions_sets_owner_only_read_write() {
        let path = temp_path("restrict");
        std::fs::write(&path, "{}").unwrap();

        restrict_permissions(&path);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        std::fs::remove_file(&path).ok();
        assert_eq!(mode, 0o600);
    }
}
