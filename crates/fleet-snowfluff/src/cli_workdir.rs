//! The working directory a CLI-backed subscription provider
//! (`claude`, `codex`) is run in (`cli-session-continuity`). Neither
//! CLI used to be given one, so it inherited wherever the app happened
//! to be launched from -- often `/` or `~` when started from Finder or
//! the Dock -- which made both "what do the CLI's relative paths mean"
//! and "which directory are its sessions scoped to" depend on the
//! launch method.
//!
//! This is a determinism fix, not an access boundary: a CLI's read
//! tools can still take absolute paths, and which native tools may run
//! stays governed by `ClaudeCodeToolAccess`.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

/// Folder under the app config dir used when no project directory is
/// configured -- created on demand, so plain persona chat over a CLI
/// works for a user who never picks a project.
const FALLBACK_DIR_NAME: &str = "cli-workspace";

/// `project_root` when it is set and exists; otherwise
/// `<config_dir>/cli-workspace`, created if missing. A `project_root`
/// that is set but no longer exists (deleted or moved since it was
/// chosen) falls back too, logged, instead of letting the spawn fail
/// with an opaque "no such directory".
pub fn resolve(config_dir: &Path, project_root: Option<&Path>) -> PathBuf {
    if let Some(root) = project_root {
        if root.is_dir() {
            return root.to_path_buf();
        }
        log::warn!(
            "project directory {} does not exist; running CLI providers in the application \
             directory instead",
            root.display()
        );
    }
    let dir = config_dir.join(FALLBACK_DIR_NAME);
    if let Err(err) = std::fs::create_dir_all(&dir) {
        log::warn!("could not create {}: {err}", dir.display());
    }
    dir
}

/// [`resolve`] against the real app config dir. If even that cannot be
/// determined, a directory under the OS temp dir keeps the result
/// deterministic rather than inheriting the launch directory.
pub fn for_app(app: &AppHandle, project_root: Option<&Path>) -> PathBuf {
    let config_dir = app
        .path()
        .app_config_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("fleet-snowfluff"));
    resolve(&config_dir, project_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-cli-workdir-test-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_configured_existing_project_root_is_used() {
        let config = temp_dir("root-set-config");
        let project = temp_dir("root-set-project");
        assert_eq!(resolve(&config, Some(&project)), project);
        assert!(
            !config.join(FALLBACK_DIR_NAME).exists(),
            "the fallback directory is not created when it is not needed"
        );
    }

    #[test]
    fn with_no_project_root_the_application_directory_is_created_and_used() {
        let config = temp_dir("root-unset");
        let resolved = resolve(&config, None);
        assert_eq!(resolved, config.join(FALLBACK_DIR_NAME));
        assert!(resolved.is_dir());
    }

    #[test]
    fn a_project_root_that_no_longer_exists_falls_back_to_the_application_directory() {
        let config = temp_dir("root-missing-config");
        let gone = config.join("was-here-once");
        let resolved = resolve(&config, Some(&gone));
        assert_eq!(resolved, config.join(FALLBACK_DIR_NAME));
        assert!(resolved.is_dir());
    }

    #[test]
    fn a_project_root_that_is_a_file_is_not_used() {
        let config = temp_dir("root-file-config");
        let file = config.join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(resolve(&config, Some(&file)), config.join(FALLBACK_DIR_NAME));
    }
}
