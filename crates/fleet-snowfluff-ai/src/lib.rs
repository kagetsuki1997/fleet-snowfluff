//! AI provider abstraction, persona loading, prompt assembly, and the
//! Agent Core (`agent-core-and-task-router`): tool-calling, the
//! `AgentRuntime` loop, native tools, and a basic `ContextManager`.
//!
//! This crate depends on nothing project-specific (`fleet-snowfluff-core`
//! or the app crate) so it stays independently testable and so
//! `fleet-snowfluff-core` never has to know AI features exist. The app
//! crate is the only place that integrates this crate with `core` --
//! where a Tauri-specific seam is unavoidable (resolving a session's
//! file path, popup/window orchestration for a `Confirm`-tier tool
//! call), this crate defines a plain trait (`SessionLog`,
//! `PermissionDecider`) for the app crate to implement, rather than
//! depending on it directly.

pub mod agent_runtime;
pub mod agent_tool;
pub mod context_manager;
pub mod conversation;
pub mod credentials;
pub mod limits;
pub mod log;
pub mod message;
pub mod native_tools;
pub mod persona;
pub mod prompt;
pub mod provider;
pub mod providers;
pub mod settings;
pub mod task_router;
pub mod tool_provider;

pub use agent_runtime::{
    AemeathAgentRuntime, AgentError, AgentRuntime, PendingToolCall, PermissionDecider, ToolRegistry,
};
pub use agent_tool::{PermissionTier, Tool, ToolContext, ToolError, ToolResult};
pub use context_manager::{AemeathContextManager, ContextManager, SessionLog};
pub use conversation::ConversationId;
pub use credentials::ProviderCredentials;
pub use log::{LogEntry, LogRole};
pub use message::{
    Message, ModelInfo, ProviderError, ProviderKind, Role, StreamChunk, ToolCallRecord,
};
pub use native_tools::{
    file_tools::{ListDirectoryTool, ReadFileTool},
    run_command::RunCommandTool,
    system_context::GetSystemContextTool,
    web_search::WebSearchTool,
};
pub use persona::{FewShotExample, Language, Persona, PersonaParseError, ResponseLanguage};
pub use provider::{AiProvider, ChatStream};
pub use providers::{Anthropic, ClaudeCodeCli, CliContext, Codex, Mock, Ollama, OpenAiCompatible};
pub use settings::{
    AiSettings, AuthMethod, ClaudeCodeToolAccess, NativeToolAccess, ProfileKey, ProviderProfile,
    TaskRouterMode,
};
pub use task_router::{
    detect_escalation, with_task_router_rules, DefaultTaskRouter, EscalationDecision,
    ExecutionRoute, RoutingContext, SessionStrategy, Task, TaskRequirements, TaskRouter,
    BUNDLED_DEFAULT_TASK_ROUTER_RULES, ESCALATE_MARKER,
};
pub use tool_provider::{ToolCallStream, ToolCallStreamItem, ToolCallingProvider, ToolDefinition};
