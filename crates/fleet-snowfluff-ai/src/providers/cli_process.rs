//! Shared helper for every CLI-subprocess-backed provider (`claude`,
//! and `codex` once it exists) -- not Anthropic- or Claude-specific,
//! unlike `claude_code_cli`, which is why it lives in its own small
//! module rather than inside either provider's file.

use std::{path::Path, process::Stdio};

use tokio::process::Command;

use crate::message::ProviderError;

/// What every *chat* spawn of a CLI-backed provider shares
/// (`cli-session-continuity`): an explicit working directory, so the
/// CLI never inherits wherever the app was launched from, and
/// kill-on-drop, so cancelling a generation -- which aborts the task
/// and drops the `Child` -- actually terminates the process instead of
/// leaving it running in the background. Deliberately not applied to
/// `trigger_login` (a detached, awaited-in-its-own-task login flow) or
/// the availability checks, neither of which is a generation.
pub(crate) fn apply_chat_spawn_settings(command: &mut Command, working_dir: &Path) {
    command
        .current_dir(working_dir)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

/// Maps a failed-to-spawn error to `RuntimeUnavailable` (binary missing
/// or not executable) vs. some other OS-level failure.
pub(crate) fn map_spawn_error(cli: &str, err: std::io::Error) -> ProviderError {
    if err.kind() == std::io::ErrorKind::NotFound {
        ProviderError::RuntimeUnavailable(format!("`{cli}` is not installed or not on PATH"))
    } else {
        ProviderError::RuntimeUnavailable(format!("could not run `{cli}`: {err}"))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Whether `pid` is still a live process. A killed child that has
    /// not been reaped yet shows up as a zombie (`Z`), which is not
    /// running for our purposes.
    fn is_running(pid: u32) -> bool {
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("ps should be available");
        let stat = String::from_utf8_lossy(&out.stdout);
        let stat = stat.trim();
        !stat.is_empty() && !stat.starts_with('Z')
    }

    #[tokio::test]
    async fn chat_spawn_settings_set_the_working_directory() {
        let dir = std::env::temp_dir();
        let mut command = Command::new("sleep");
        command.arg("30");
        apply_chat_spawn_settings(&mut command, &dir);
        assert_eq!(command.as_std().get_current_dir(), Some(dir.as_path()));
    }

    #[tokio::test]
    async fn dropping_the_child_terminates_the_process() {
        let mut command = Command::new("sleep");
        command.arg("30");
        apply_chat_spawn_settings(&mut command, &std::env::temp_dir());
        let child = command.spawn().expect("sleep should spawn");
        let pid = child.id().expect("a running child has a pid");
        assert!(is_running(pid), "the stand-in process should be running before the drop");

        drop(child);

        let mut gone = false;
        for _ in 0..50 {
            if !is_running(pid) {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        assert!(gone, "process {pid} still running 2s after its Child was dropped");
    }
}
