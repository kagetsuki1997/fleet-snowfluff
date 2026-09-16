//! Resolves and loads the user's `persona.yaml`, seeding it from the
//! bundled default the first time it's needed (`ai-persona`'s "Bundled
//! default persona" / "User-editable persona file").
//! `fleet_snowfluff_ai::persona` has the pure load/fallback logic
//! (all-or-nothing parse, graceful fallback to the bundled default on
//! failure); this module is the thin file-I/O layer around it, same
//! split as `config_store.rs`.

use fleet_snowfluff_ai::persona::{self, PersonaLoadResult};
use tauri::{AppHandle, Manager};

pub fn persona_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("persona.yaml"))
}

/// Seeds the user's `persona.yaml` from the bundled default the first
/// time it's needed -- a no-op if the file already exists, so it never
/// overwrites a user's own edit. Call once, e.g. before wiring up an
/// "edit persona" affordance, so there's actually a file to open.
pub fn seed_if_missing(app: &AppHandle) {
    if let Some(path) = persona_path(app) {
        seed_if_missing_at(&path);
    }
}

/// Loads the persona fresh from disk every call (`ai-persona`'s "Fresh
/// reload on every message" -- deliberately no caching), falling back
/// to the bundled default (with a warning) on a missing or malformed
/// file.
pub fn load(app: &AppHandle) -> PersonaLoadResult {
    match persona_path(app) {
        Some(path) => load_at(&path),
        None => persona::load_persona_or_bundled_default(None),
    }
}

fn seed_if_missing_at(path: &std::path::Path) {
    if path.exists() {
        return;
    }
    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            log::error!("failed to create config dir {dir:?}: {err}");
            return;
        }
    }
    if let Err(err) = std::fs::write(path, persona::BUNDLED_DEFAULT_PERSONA_YAML) {
        log::error!("failed to seed persona file at {path:?}: {err}");
    }
}

fn load_at(path: &std::path::Path) -> PersonaLoadResult {
    let contents = std::fs::read_to_string(path).ok();
    persona::load_persona_or_bundled_default(contents.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("fleet-snowfluff-persona-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn seed_if_missing_writes_the_bundled_default_when_absent() {
        let path = temp_path("seed-missing");
        std::fs::remove_file(&path).ok();

        seed_if_missing_at(&path);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, persona::BUNDLED_DEFAULT_PERSONA_YAML);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn seed_if_missing_never_overwrites_an_existing_file() {
        let path = temp_path("seed-existing");
        std::fs::write(&path, "name: \"user's own edit\"").unwrap();

        seed_if_missing_at(&path);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("name: \"user's own edit\""));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_at_falls_back_to_bundled_default_when_file_is_missing() {
        let path = temp_path("load-missing");
        std::fs::remove_file(&path).ok();

        let result = load_at(&path);
        assert!(result.warning.is_none());
    }

    #[test]
    fn load_at_reads_fresh_on_every_call_with_no_caching() {
        let path = temp_path("load-fresh");
        let minimal = |name: &str| {
            format!(
                "name: \"{name}\"\nresponse_language: \"auto\"\npersonality: \"a\"\nspeech_style: \
                 \"a\"\n"
            )
        };

        std::fs::write(&path, minimal("first")).unwrap();
        assert_eq!(load_at(&path).persona.name, "first");

        std::fs::write(&path, minimal("second")).unwrap();
        assert_eq!(load_at(&path).persona.name, "second", "must not have cached the first read");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_at_falls_back_with_a_warning_when_the_file_is_malformed() {
        let path = temp_path("load-malformed");
        std::fs::write(&path, "not: valid: yaml: : :").unwrap();

        let result = load_at(&path);
        assert!(result.warning.is_some());

        std::fs::remove_file(&path).ok();
    }
}
