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
pub mod compaction;
pub mod context_budget;
pub mod context_manager;
pub mod conversation;
pub mod credentials;
pub mod execution;
pub mod fit;
pub mod limits;
pub mod log;
pub mod mcp;
pub mod message;
pub mod native_tools;
pub mod persona;
pub mod prompt;
pub mod provider;
pub mod providers;
pub mod scheduler;
pub mod sensitive;
pub mod settings;
pub mod summary;
pub mod task_router;
pub mod token_estimate;
pub mod tool_output;
pub mod tool_provider;

pub use agent_runtime::{
    AemeathAgentRuntime, AgentError, AgentOutcome, AgentRuntime, ConnectMcpServerTool,
    DelegateTool, McpConnectOutcome, McpConnectRequest, McpConnector, NeverConnectMcp,
    PendingToolCall, PermissionDecider, SearchToolsTool, ToolInvocation, ToolOutcome, ToolRegistry,
    CONNECT_MCP_SERVER_TOOL_NAME, SEARCH_TOOLS_NAME,
};
pub use agent_tool::{PermissionTier, Tool, ToolContext, ToolError, ToolResult};
pub use compaction::{
    pending_fold_tokens, run_compaction, summary_system_prompt, summary_user_message,
    CompactionError, CompactionJob, CompactionOutcome, CompactionRequester, ProviderSummaryWriter,
    SummaryRequest, SummaryWriter,
};
pub use context_budget::{
    api_window, shrink_tool_results, suspected_truncation, UNKNOWN_MODEL_WINDOW,
};
pub use context_manager::{
    AemeathContextManager, ContextManager, ContextRequest, ExecutionLog, SessionLog,
};
pub use conversation::ConversationId;
pub use credentials::{McpServerCredential, ProviderCredentials};
pub use execution::{
    pair_events, parse_events, serialize_event, Execution, ExecutionEnd, ExecutionEvent,
    ExecutionId, ExecutionPath, ExecutionStart, ExecutionStatus,
};
pub use fit::{fit_request, Fit, Shed, WindowTooSmall};
pub use log::{LogEntry, LogRole};
pub use mcp::{
    build_authorization_url, discover, exchange_code_for_token, generate_pkce, generate_state,
    mcp_http_client, probe_authorization, refresh_access_token, register_client_or_explain,
    AuthProbeOutcome, AuthorizationServerMetadata, HttpTransport, McpClient, McpError,
    McpServerConfig, McpServerRecord, McpServerStatus, McpServerTransportConfig, McpServersConfig,
    McpTool, McpToolCallResult, McpToolDescriptor, McpToolSummary, McpTransport, PkceChallenge,
    RedirectListener, StdioTransport, TokenResponse,
};
pub use message::{
    Message, ModelInfo, ProviderError, ProviderKind, Role, StreamChunk, ToolCallRecord,
};
pub use native_tools::{
    compact_context::{CompactContextTool, COMPACT_CONTEXT_TOOL_NAME},
    file_tools::{ListDirectoryTool, ReadFileTool},
    run_command::RunCommandTool,
    system_context::GetSystemContextTool,
    web_search::WebSearchTool,
};
pub use persona::{FewShotExample, Language, Persona, PersonaParseError, ResponseLanguage};
pub use prompt::{HistoryLimits, HistoryPlan};
pub use provider::{AiProvider, ChatStream};
pub use providers::{Anthropic, ClaudeCodeCli, CliContext, Codex, Mock, Ollama, OpenAiCompatible};
pub use scheduler::{
    decide as decide_compaction, Clock, CompactionRunner, CompactionScheduler, SchedulerConfig,
    SessionState, SystemClock, Trigger,
};
pub use sensitive::{is_sensitive, remove_sensitive_sentences};
pub use settings::{
    AiSettings, AuthMethod, ClaudeCodeToolAccess, NativeToolAccess, ProfileKey, ProviderProfile,
    TaskRouterMode, LOCAL_PROFILE_KEY,
};
pub use summary::{summary_path, ConversationSummary, FileSummaryStore, SummaryStore};
pub use task_router::{
    detect_escalation, with_task_router_rules, DefaultTaskRouter, EscalationDecision,
    ExecutionRoute, RoutingContext, SessionStrategy, Task, TaskRequirements, TaskRouter,
    BUNDLED_DEFAULT_TASK_ROUTER_RULES, ESCALATE_MARKER,
};
pub use token_estimate::{
    estimate_message_tokens, estimate_messages_tokens, estimate_tokens, estimate_tools_tokens,
};
pub use tool_output::{bound_tool_output, ToolOutputBound};
pub use tool_provider::{ToolCallStream, ToolCallStreamItem, ToolCallingProvider, ToolDefinition};
