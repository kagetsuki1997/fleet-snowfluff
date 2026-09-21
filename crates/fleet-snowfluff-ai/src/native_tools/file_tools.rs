//! `read_file`/`list_directory`: gated by `AiSettings.project_root`. A
//! path that canonicalizes to somewhere inside `project_root` is
//! `Auto`; a path outside it -- or any path at all if `project_root`
//! isn't configured yet, since an unset root means the auto zone is
//! the empty set -- escalates to `Confirm`. The containment check is a
//! real canonicalization + containment check, not a string-prefix
//! comparison, specifically to close `..`-traversal and symlink-escape
//! holes (design.md's "`Tool` trait has an args-aware
//! `required_permission`").

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{
    agent_tool::{PermissionTier, Tool, ToolContext, ToolError, ToolResult},
    tool_provider::ToolDefinition,
};

/// Resolves the path argument the model provided against
/// `project_root`: an absolute path is used as-is (the model may
/// legitimately ask for a location outside the project directory,
/// which is exactly the case `Confirm` exists for); a relative path is
/// joined onto `project_root` when one is configured, or left as a
/// path with no meaningful base otherwise (harmless, since
/// [`required_permission_for_path`] already returns `Confirm` whenever
/// `project_root` is `None`, so this value is never treated as
/// in-root).
fn resolve_candidate_path(path_arg: &str, project_root: Option<&Path>) -> PathBuf {
    let path = Path::new(path_arg);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match project_root {
        Some(root) => root.join(path),
        None => path.to_path_buf(),
    }
}

/// A real containment check: canonicalizing both sides resolves `.`/
/// `..` components and symlinks, so a symlink inside `project_root`
/// that points outside it is correctly treated as outside, not as a
/// string-level match.
fn is_within_root(root: &Path, candidate: &Path) -> bool {
    let (Ok(root), Ok(candidate)) = (root.canonicalize(), candidate.canonicalize()) else {
        return false;
    };
    candidate.starts_with(root)
}

fn required_permission_for_path(path_arg: &str, ctx: &ToolContext) -> PermissionTier {
    let Some(root) = &ctx.project_root else { return PermissionTier::Confirm };
    let candidate = resolve_candidate_path(path_arg, Some(root));
    if is_within_root(root, &candidate) {
        PermissionTier::Auto
    } else {
        PermissionTier::Confirm
    }
}

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".to_string(),
            description: "Reads the contents of a text file".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to the project directory or absolute",
                    },
                },
                "required": ["path"],
            }),
        }
    }

    fn required_permission(&self, args: &Value, ctx: &ToolContext) -> PermissionTier {
        match args.get("path").and_then(Value::as_str) {
            Some(path) => required_permission_for_path(path, ctx),
            // Malformed args must never resolve to `Auto` -- fail
            // toward the safer tier and let `execute` report the real
            // problem.
            None => PermissionTier::Confirm,
        }
    }

    fn allows_session_remember(&self) -> bool { true }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let path_arg = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError("missing \"path\" argument".to_string()))?;
        let candidate = resolve_candidate_path(path_arg, ctx.project_root.as_deref());
        match std::fs::read_to_string(&candidate) {
            Ok(content) => Ok(ToolResult::ok(content)),
            Err(err) => {
                Ok(ToolResult::error(format!("could not read {}: {err}", candidate.display())))
            }
        }
    }
}

pub struct ListDirectoryTool;

#[async_trait]
impl Tool for ListDirectoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_directory".to_string(),
            description: "Lists the entries of a directory".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the directory, relative to the project directory or absolute",
                    },
                },
                "required": ["path"],
            }),
        }
    }

    fn required_permission(&self, args: &Value, ctx: &ToolContext) -> PermissionTier {
        match args.get("path").and_then(Value::as_str) {
            Some(path) => required_permission_for_path(path, ctx),
            None => PermissionTier::Confirm,
        }
    }

    fn allows_session_remember(&self) -> bool { true }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let path_arg = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError("missing \"path\" argument".to_string()))?;
        let candidate = resolve_candidate_path(path_arg, ctx.project_root.as_deref());
        match std::fs::read_dir(&candidate) {
            Ok(entries) => {
                let mut names: Vec<String> = entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                Ok(ToolResult::ok(names.join("\n")))
            }
            Err(err) => {
                Ok(ToolResult::error(format!("could not list {}: {err}", candidate.display())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ConversationId;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-file-tools-test-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx_with_root(root: Option<PathBuf>) -> ToolContext {
        ToolContext {
            project_root: root,
            conversation_id: ConversationId::from_session_path(std::path::Path::new("/tmp/x")),
        }
    }

    #[test]
    fn a_path_inside_project_root_is_auto() {
        let root = temp_dir("in-root");
        std::fs::write(root.join("a.txt"), "hi").unwrap();
        let ctx = ctx_with_root(Some(root));
        assert_eq!(
            ReadFileTool.required_permission(&json!({"path": "a.txt"}), &ctx),
            PermissionTier::Auto
        );
        std::fs::remove_dir_all(ctx.project_root.unwrap()).ok();
    }

    #[test]
    fn an_absolute_path_outside_project_root_requires_confirmation() {
        let root = temp_dir("out-of-root");
        let outside = std::env::temp_dir()
            .join(format!("fleet-snowfluff-file-tools-test-outside-{}", std::process::id()));
        std::fs::write(&outside, "secret").unwrap();
        let ctx = ctx_with_root(Some(root.clone()));
        assert_eq!(
            ReadFileTool.required_permission(&json!({"path": outside.to_string_lossy()}), &ctx),
            PermissionTier::Confirm
        );
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_file(&outside).ok();
    }

    #[test]
    fn a_dot_dot_traversal_attempt_requires_confirmation() {
        let root = temp_dir("traversal");
        let inner = root.join("project");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(root.join("secret.txt"), "top secret").unwrap();
        let ctx = ctx_with_root(Some(inner));
        // "../secret.txt" resolves outside `project_root` once canonicalized.
        assert_eq!(
            ReadFileTool.required_permission(&json!({"path": "../secret.txt"}), &ctx),
            PermissionTier::Confirm
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_pointing_outside_project_root_requires_confirmation() {
        let root = temp_dir("symlink-root");
        let outside_target = std::env::temp_dir()
            .join(format!("fleet-snowfluff-file-tools-test-symlink-target-{}", std::process::id()));
        std::fs::write(&outside_target, "outside content").unwrap();
        std::os::unix::fs::symlink(&outside_target, root.join("escape")).unwrap();

        let ctx = ctx_with_root(Some(root.clone()));
        assert_eq!(
            ReadFileTool.required_permission(&json!({"path": "escape"}), &ctx),
            PermissionTier::Confirm,
            "a symlink resolving outside project_root must not be Auto"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_file(&outside_target).ok();
    }

    #[test]
    fn any_path_is_confirm_when_project_root_is_unset() {
        let ctx = ctx_with_root(None);
        assert_eq!(
            ReadFileTool.required_permission(&json!({"path": "anything.txt"}), &ctx),
            PermissionTier::Confirm
        );
    }

    #[tokio::test]
    async fn read_file_returns_file_contents() {
        let root = temp_dir("read-contents");
        std::fs::write(root.join("a.txt"), "hello world").unwrap();
        let ctx = ctx_with_root(Some(root.clone()));
        let result = ReadFileTool.execute(json!({"path": "a.txt"}), &ctx).await.unwrap();
        assert_eq!(result, ToolResult::ok("hello world"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn read_file_reports_a_model_visible_error_for_a_missing_file() {
        let root = temp_dir("read-missing");
        let ctx = ctx_with_root(Some(root.clone()));
        let result = ReadFileTool.execute(json!({"path": "nope.txt"}), &ctx).await.unwrap();
        assert!(result.is_error);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn list_directory_returns_sorted_entry_names() {
        let root = temp_dir("list-entries");
        std::fs::write(root.join("b.txt"), "").unwrap();
        std::fs::write(root.join("a.txt"), "").unwrap();
        let ctx = ctx_with_root(Some(root.clone()));
        let result = ListDirectoryTool.execute(json!({"path": "."}), &ctx).await.unwrap();
        assert_eq!(result, ToolResult::ok("a.txt\nb.txt"));
        std::fs::remove_dir_all(&root).ok();
    }
}
