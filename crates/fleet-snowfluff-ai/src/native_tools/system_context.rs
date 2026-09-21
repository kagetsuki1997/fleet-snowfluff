//! `get_system_context`: a Stage-3-scoped subset of "what does this
//! machine look like right now" -- CPU/memory/uptime, current date/
//! time, and OS name. Explicitly excludes the active window title,
//! user idle time, and clipboard content --
//! `docs/fleet-snowfluff-feature-planning.md` §9's fuller Context
//! Awareness (which does read those signals) is Stage 6's job, not
//! this one's.

use async_trait::async_trait;
use serde_json::{json, Value};
use sysinfo::System;

use crate::{
    agent_tool::{PermissionTier, Tool, ToolContext, ToolError, ToolResult},
    tool_provider::ToolDefinition,
};

/// Builds the reported text. A fresh `System` is created per call
/// (this tool is not expected to be invoked at any real frequency) --
/// no long-lived state to keep in sync.
fn build_system_context() -> String {
    let mut sys = System::new_all();
    sys.refresh_all();

    let os = std::env::consts::OS;
    let now = chrono::Utc::now().to_rfc3339();
    let cpu_count = sys.cpus().len();
    let cpu_usage = sys.global_cpu_usage();
    let total_memory_mb = sys.total_memory() / (1024 * 1024);
    let used_memory_mb = sys.used_memory() / (1024 * 1024);
    let uptime_secs = System::uptime();

    format!(
        "OS: {os}\nCurrent time (UTC): {now}\nCPU: {cpu_count} cores, {cpu_usage:.1}% \
         used\nMemory: {used_memory_mb} MB / {total_memory_mb} MB used\nUptime: {uptime_secs} \
         seconds"
    )
}

pub struct GetSystemContextTool;

#[async_trait]
impl Tool for GetSystemContextTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_system_context".to_string(),
            description: "Returns basic system information: current date/time, OS, CPU and memory \
                          usage, and uptime"
                .to_string(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Auto
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::ok(build_system_context()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ConversationId;

    fn ctx() -> ToolContext {
        ToolContext {
            project_root: None,
            conversation_id: ConversationId::from_session_path(std::path::Path::new("/tmp/x")),
        }
    }

    #[test]
    fn build_system_context_reports_all_expected_fields() {
        let context = build_system_context();
        assert!(context.contains("OS:"));
        assert!(context.contains("Current time (UTC):"));
        assert!(context.contains("CPU:"));
        assert!(context.contains("Memory:"));
        assert!(context.contains("Uptime:"));
    }

    #[test]
    fn build_system_context_never_mentions_excluded_signals() {
        // Active window, idle time, and clipboard content are Stage
        // 6's Context Awareness, not this tool's -- a regression here
        // would be a real scope leak, not a cosmetic one.
        let context = build_system_context().to_lowercase();
        assert!(!context.contains("window"));
        assert!(!context.contains("idle"));
        assert!(!context.contains("clipboard"));
    }

    #[test]
    fn required_permission_is_always_auto() {
        assert_eq!(
            GetSystemContextTool.required_permission(&json!({}), &ctx()),
            PermissionTier::Auto
        );
    }

    #[tokio::test]
    async fn execute_returns_a_non_error_result() {
        let result = GetSystemContextTool.execute(json!({}), &ctx()).await.unwrap();
        assert!(!result.is_error);
        assert!(!result.content.is_empty());
    }
}
