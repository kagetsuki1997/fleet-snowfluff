//! Resolves and loads `task-router-rules.md`, seeding it from the
//! bundled default the first time it's needed -- same file-I/O-around-
//! pure-logic split as `persona_store.rs`, except there's no parsing
//! step here at all (the file is plain text appended verbatim to a
//! system prompt, not YAML parsed into a struct), so there's no
//! "malformed file" case to report a warning for -- only "missing or
//! unreadable," which falls back to the bundled default silently.

use fleet_snowfluff_ai::BUNDLED_DEFAULT_TASK_ROUTER_RULES;
use tauri::{AppHandle, Manager};

pub fn rules_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("task-router-rules.md"))
}

/// Seeds the user's `task-router-rules.md` from the bundled default the
/// first time it's needed -- a no-op if the file already exists, so it
/// never overwrites a user's own edit.
pub fn seed_if_missing(app: &AppHandle) {
    if let Some(path) = rules_path(app) {
        seed_if_missing_at(&path);
    }
}

/// Loads the rules fresh from disk every call (matching
/// `persona_store::load`'s "no caching" precedent -- a mid-conversation
/// edit takes effect on the very next mix-mode message), falling back
/// to the bundled default on a missing or unreadable file.
pub fn load(app: &AppHandle) -> String {
    match rules_path(app) {
        Some(path) => load_at(&path),
        None => BUNDLED_DEFAULT_TASK_ROUTER_RULES.to_string(),
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
    if let Err(err) = std::fs::write(path, BUNDLED_DEFAULT_TASK_ROUTER_RULES) {
        log::error!("failed to seed task router rules file at {path:?}: {err}");
    }
}

fn load_at(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|_| BUNDLED_DEFAULT_TASK_ROUTER_RULES.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("fleet-snowfluff-task-router-rules-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn seed_if_missing_writes_the_bundled_default_when_absent() {
        let path = temp_path("seed-missing");
        std::fs::remove_file(&path).ok();

        seed_if_missing_at(&path);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, BUNDLED_DEFAULT_TASK_ROUTER_RULES);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn seed_if_missing_never_overwrites_an_existing_file() {
        let path = temp_path("seed-existing");
        std::fs::write(&path, "the user's own rules").unwrap();

        seed_if_missing_at(&path);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "the user's own rules");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_at_falls_back_to_the_bundled_default_when_the_file_is_missing() {
        let path = temp_path("load-missing");
        std::fs::remove_file(&path).ok();

        assert_eq!(load_at(&path), BUNDLED_DEFAULT_TASK_ROUTER_RULES);
    }

    #[test]
    fn load_at_reads_fresh_on_every_call_with_no_caching() {
        let path = temp_path("load-fresh");

        std::fs::write(&path, "first version").unwrap();
        assert_eq!(load_at(&path), "first version");

        std::fs::write(&path, "second version").unwrap();
        assert_eq!(load_at(&path), "second version", "must not have cached the first read");

        std::fs::remove_file(&path).ok();
    }
}
