//! `run_command`: the highest-risk native tool. Unlike `read_file`/
//! `list_directory`, there is no popup escalation to a different
//! working directory when `project_root` is unset -- shell execution
//! is a meaningfully higher risk tier than read-only file access, so
//! an unconfigured root is a flat rejection, not a "which directory"
//! decision to offer the user.

use std::{path::Path, process::Stdio, time::Duration};

use async_trait::async_trait;
use fleet_snowfluff_ai::ToolDefinition;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::agent_tool::{PermissionTier, Tool, ToolContext, ToolError, ToolResult};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_BYTES: usize = 20 * 1024;
const TRUNCATION_MARKER: &str = "\n[output truncated]";

/// Caps `output` at [`MAX_OUTPUT_BYTES`], cutting on a UTF-8 character
/// boundary so truncation never produces invalid UTF-8.
fn truncate_output(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output.to_string();
    }
    let mut cut = MAX_OUTPUT_BYTES;
    while !output.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{TRUNCATION_MARKER}", &output[..cut])
}

/// `sh -c`/`cmd /C` per OS, `stdin(Stdio::null())` matching the
/// existing CLI-provider spawn convention, cwd hard-pinned to `cwd`,
/// and `.kill_on_drop(true)` so a timeout (which drops the future that
/// owns the `Child`) actually terminates the process rather than
/// leaking it.
fn build_command(command_str: &str, cwd: &Path) -> Command {
    let mut command = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command_str);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command_str);
        c
    };
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

pub struct RunCommandTool {
    timeout: Duration,
}

impl Default for RunCommandTool {
    fn default() -> Self { Self { timeout: DEFAULT_TIMEOUT } }
}

#[async_trait]
impl Tool for RunCommandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_command".to_string(),
            description: "Runs a shell command in the project directory".to_string(),
            parameters: json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"],
            }),
        }
    }

    fn required_permission(&self, _args: &Value, ctx: &ToolContext) -> PermissionTier {
        if ctx.project_root.is_some() {
            PermissionTier::Confirm
        } else {
            PermissionTier::Deny
        }
    }

    // Default `false` (never offered): one approved command must not
    // silently trust every future, unrelated command for the rest of
    // the session.

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let command_str = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError("missing \"command\" argument".to_string()))?;
        let Some(cwd) = &ctx.project_root else {
            return Ok(ToolResult::error("no project directory is configured"));
        };

        let child = build_command(command_str, cwd)
            .spawn()
            .map_err(|e| ToolError(format!("failed to start command: {e}")))?;

        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let combined = format!(
                    "exit code: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                    output.status.code().unwrap_or(-1)
                );
                Ok(ToolResult {
                    content: truncate_output(&combined),
                    is_error: !output.status.success(),
                })
            }
            Ok(Err(err)) => Ok(ToolResult::error(format!("command failed: {err}"))),
            Err(_) => Ok(ToolResult::error(format!(
                "command timed out after {}s",
                self.timeout.as_secs()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_domain::ConversationId;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-run-command-test-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx_with_root(root: Option<std::path::PathBuf>) -> ToolContext {
        ToolContext {
            project_root: root,
            conversation_id: ConversationId::from_session_path(Path::new("/tmp/x")),
        }
    }

    // "echo hello" runs identically via both `sh -c` and `cmd /C`, so
    // no platform branching is needed here (unlike `sleep_command`).
    fn echo_command() -> &'static str { "echo hello" }

    fn sleep_command() -> &'static str {
        if cfg!(target_os = "windows") {
            "ping -n 5 127.0.0.1"
        } else {
            "sleep 5"
        }
    }

    #[test]
    fn allows_session_remember_is_always_false() {
        assert!(!RunCommandTool::default().allows_session_remember());
    }

    #[test]
    fn required_permission_is_deny_without_a_project_root() {
        let ctx = ctx_with_root(None);
        assert_eq!(
            RunCommandTool::default().required_permission(&json!({}), &ctx),
            PermissionTier::Deny
        );
    }

    #[test]
    fn required_permission_is_confirm_with_a_project_root() {
        let root = temp_dir("permission");
        let ctx = ctx_with_root(Some(root.clone()));
        assert_eq!(
            RunCommandTool::default().required_permission(&json!({}), &ctx),
            PermissionTier::Confirm
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_normal_command_completes_and_reports_its_output() {
        let root = temp_dir("normal");
        let ctx = ctx_with_root(Some(root.clone()));
        let result = RunCommandTool::default()
            .execute(json!({"command": echo_command()}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
        assert!(result.content.contains("exit code: 0"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn without_a_project_root_execute_refuses_without_spawning_anything() {
        let ctx = ctx_with_root(None);
        let result = RunCommandTool::default()
            .execute(json!({"command": echo_command()}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("no project directory"));
    }

    #[tokio::test]
    async fn a_slow_command_is_killed_and_reported_as_timed_out() {
        let root = temp_dir("timeout");
        let ctx = ctx_with_root(Some(root.clone()));
        let tool = RunCommandTool { timeout: Duration::from_millis(200) };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            tool.execute(json!({"command": sleep_command()}), &ctx),
        )
        .await
        .expect("the tool's own timeout must fire well within this outer guard")
        .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn output_under_the_cap_is_returned_unchanged() {
        let output = "a".repeat(100);
        assert_eq!(truncate_output(&output), output);
    }

    #[test]
    fn output_over_the_cap_is_truncated_with_a_marker() {
        let output = "a".repeat(MAX_OUTPUT_BYTES + 500);
        let truncated = truncate_output(&output);
        assert!(truncated.len() < output.len());
        assert!(truncated.ends_with(TRUNCATION_MARKER));
        assert_eq!(truncated.len(), MAX_OUTPUT_BYTES + TRUNCATION_MARKER.len());
    }
}
