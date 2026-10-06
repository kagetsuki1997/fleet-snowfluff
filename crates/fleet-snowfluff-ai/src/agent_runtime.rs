//! `AgentRuntime`: the tool-calling conversation loop for a
//! `ToolCallingProvider` (`agent-core-and-task-router`'s Group 4).
//! Concretely: `AemeathAgentRuntime` sends `messages`, reads back a
//! stream of text and/or tool calls, executes whichever calls are
//! permitted, feeds their results back in, and repeats until the model
//! stops requesting tools or a fixed iteration limit is hit.
//!
//! Deliberately does not take the fuller `Execution`/`AgentResult`
//! shape `docs/fleet-snowfluff-feature-planning.md` §6.7 sketches for
//! the eventual multi-runtime architecture -- this proposal has only
//! one `AgentRuntime` implementation, so there is nothing for a
//! heavier `Execution` wrapper to abstract over today.
//!
//! `sub-agent-delegation` added a bounded sub-task delegation
//! capability (`DELEGATE_TOOL_NAME`/`DelegateTool`), special-cased
//! directly inside `run()`'s own dispatch rather than implemented as a
//! generic `Tool::execute()` call -- see that call site's own comment
//! and design.md's Decision 1 for why.

use std::{collections::HashSet, sync::Arc};

use futures_util::{stream, StreamExt};
use serde_json::{json, Value};

use crate::{
    agent_tool::{PermissionTier, Tool, ToolContext, ToolError, ToolResult},
    message::{Message, ProviderError, ToolCallRecord},
    tool_provider::{ToolCallStreamItem, ToolCallingProvider, ToolDefinition},
};

/// At most this many bytes of a rejected tool call's raw arguments are
/// kept for diagnosis (`execution-log-and-context`) -- matching the
/// magnitude of `run_command`'s and `read_file`'s own output caps, not
/// because a malformed-arguments blob is a realistic attack surface for
/// a desktop pet app, but because every other model-originated value
/// already persisted in this codebase is bounded, and this is the one
/// new place that persists one.
const MAX_REJECTED_ARGS_BYTES: usize = 20 * 1024;
const REJECTED_ARGS_TRUNCATION_MARKER: &str = " ...[truncated]";

/// How many `Auto`-tier tool calls from one model turn may actually be
/// executing at once (`sub-agent-delegation`'s own design.md Decision
/// 7). Unlike every other native tool, a delegated sub-task's own
/// blast radius (a full nested agent loop, possibly a CLI subprocess)
/// is categorically larger than `read_file`'s, so this throttles total
/// concurrency rather than letting an unbounded `join_all` run
/// everything the model asked for at once. Calls beyond this limit
/// still all execute -- they simply queue, starting as earlier ones
/// finish, never rejected or dropped.
const MAX_CONCURRENT_TOOL_CALLS: usize = 3;

/// Serializes `arguments` compactly and caps it at
/// [`MAX_REJECTED_ARGS_BYTES`], cutting on a UTF-8 character boundary
/// (never mid-character) the same way `read_file`'s own cap does. A
/// `Value` that somehow fails to serialize (never happens for anything
/// `serde_json::from_str` itself produced, which is the only source of
/// a `Rejected` call's arguments) falls back to a fixed placeholder
/// rather than panicking.
fn capped_arguments_preview(arguments: &Value) -> String {
    let full = serde_json::to_string(arguments)
        .unwrap_or_else(|_| "<arguments could not be serialized>".to_string());
    if full.len() <= MAX_REJECTED_ARGS_BYTES {
        return full;
    }
    let mut cut = MAX_REJECTED_ARGS_BYTES;
    while !full.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{REJECTED_ARGS_TRUNCATION_MARKER}", &full[..cut])
}

/// What became of one tool call the model requested, retained after the
/// agent loop returns (`execution-log-and-context`'s "Tool call outcomes
/// survive the turn that produced them"). Only `Rejected` keeps any
/// content from the call itself -- the one outcome where the call never
/// reached `execute()`, so there is no tool-side result to protect, and
/// the argument is the only available diagnostic for why it failed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    /// Reached `execute()`; `ok` is `false` for a `ToolError` or a
    /// `ToolResult { is_error: true, .. }`, exactly the same distinction
    /// `tool_result_content` already makes for the model-facing message.
    Executed { ok: bool },
    /// A `Deny`-tier tool -- a standing policy setting, not a one-off
    /// choice for this call.
    DeniedByPolicy,
    /// A `Confirm`-tier tool the user declined for this call -- a
    /// one-off choice that might go differently next time, kept
    /// distinct from `DeniedByPolicy` for exactly that reason.
    DeclinedByUser,
    /// An unknown tool name, or arguments that are not a JSON object --
    /// the two cases the loop already treats identically (an error
    /// tool-result with no execution attempted).
    Rejected { arguments_preview: String },
}

/// One tool call's name and what became of it, in the order the model
/// requested it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolInvocation {
    pub name: String,
    pub outcome: ToolOutcome,
}

/// One tool call the model requested in a single turn, before it's
/// known whether it's permitted to run -- the unit
/// [`PermissionDecider::decide`] batches over (the "Batched
/// confirmation for one turn" requirement).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// What `run()`'s classification pass decided for one call, before any
/// `Auto`-tier call has actually been executed -- kept separate from
/// execution so classification (synchronous, ordered) and `Auto`-tier
/// execution (concurrent, bounded) can be two distinct passes without
/// losing each call's original position.
enum CallPlan {
    UnknownTool,
    MalformedArguments,
    Denied,
    Auto(Arc<dyn Tool>),
    Confirm(Arc<dyn Tool>),
    /// `call.name == DELEGATE_TOOL_NAME` *and* the registry actually
    /// offers it (checked via the same `registry.find()` every other
    /// call already goes through, not by name alone) -- a delegated
    /// sub-task's own registry omits it entirely, so a child that
    /// hallucinates this name still falls through to `UnknownTool`,
    /// preserving the depth-1 cap. Carries the already-parsed task
    /// description.
    Delegate(String),
    /// `call.name == CONNECT_MCP_SERVER_TOOL_NAME` *and* the registry
    /// actually offers it -- same registry-gated reasoning as
    /// `Delegate` above, and for the same purpose: a delegated sub-
    /// task's own registry also omits this name (see its own
    /// `child_registry` construction below), so it falls through to
    /// `UnknownTool` there too. Carries the already-parsed request.
    ConnectMcp(McpConnectRequest),
    /// `call.name == SEARCH_TOOLS_NAME` *and* the registry actually
    /// offers it. Carries the already-parsed query. Unlike `Delegate`/
    /// `ConnectMcp`, resolved in a plain synchronous pass (matching
    /// against already-in-memory tool definitions needs no `.await`
    /// at all), not the concurrent one below.
    SearchTools(String),
    /// `name` resolves to a *lazy* tool in this registry that hasn't
    /// been searched for yet this turn -- rejected the same way an
    /// unknown tool is (model-visible error, `execute()` never
    /// reached), mirroring Claude Code's own deferred tools failing a
    /// direct call before being searched for.
    ToolNotYetUnlocked,
}

/// The reserved name `run()` recognizes to dispatch delegation
/// specially, before ever reaching the generic `registry.find()` +
/// `Tool::execute()` path (design.md's Decision 1: `Tool::execute()`'s
/// signature cannot reach the `provider`/`registry`/`permission`
/// references recursion needs).
pub const DELEGATE_TOOL_NAME: &str = "delegate_task";

/// `delegate_task`'s own schema -- registered normally in a
/// `ToolRegistry` so the model can discover and call it like any other
/// tool. The description tells the model to batch independent
/// sub-tasks into one turn (several calls together) rather than one at
/// a time waiting for each, since that's what actually benefits from
/// the agent loop's own bounded concurrency
/// (`MAX_CONCURRENT_TOOL_CALLS`) -- calling this once, waiting, then
/// calling it again gets no parallelism no matter how concurrent the
/// execution layer is capable of being.
pub fn delegate_task_definition() -> ToolDefinition {
    ToolDefinition {
        name: DELEGATE_TOOL_NAME.to_string(),
        description: "Delegates a focused, self-contained sub-task to a new, independent agent \
                      with no knowledge of this conversation -- include every fact the sub-task \
                      needs directly in its description. Returns the sub-task's final answer. If \
                      you have more than one independent sub-task, call this tool multiple times \
                      in the same turn, not one at a time waiting for each, so they run \
                      concurrently. The delegated sub-task cannot itself delegate further."
            .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "A complete, self-contained description of the sub-task, \
                                     including any facts or context it needs -- the delegated \
                                     agent has no access to this conversation."
                }
            },
            "required": ["task"],
        }),
    }
}

/// `delegate_task`'s own `Tool` impl exists only so its `definition()`
/// and `required_permission()` (never `Confirm`-tier -- see design.md's
/// Decision 3) participate in the normal `ToolRegistry`/model-discovery
/// machinery. `execute()` is unreachable in correct operation: `run()`'s
/// own dispatch intercepts `DELEGATE_TOOL_NAME` before ever reaching
/// the generic `Tool::execute()` path. If this ever runs, something
/// upstream failed to special-case it -- fail loudly in the result
/// rather than panicking, since a tool error is still safely reportable
/// to the model.
pub struct DelegateTool;

#[async_trait::async_trait]
impl Tool for DelegateTool {
    fn definition(&self) -> ToolDefinition { delegate_task_definition() }

    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Auto
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::error(
            "delegate_task was dispatched through the generic tool path, which should be \
             unreachable -- this is an internal bug, not a user-facing failure"
                .to_string(),
        ))
    }
}

/// The reserved name `run()` recognizes to dispatch `mcp-client-support`'s
/// own connect flow specially -- same reason and same mechanism as
/// [`DELEGATE_TOOL_NAME`]: the model decides to call this mid-turn, so
/// dispatch is special-cased *inside* `run()`'s own per-iteration loop,
/// not before `run()` is ever reached.
pub const CONNECT_MCP_SERVER_TOOL_NAME: &str = "connect_mcp_server";

/// What the model supplied when requesting a connection -- resolved as
/// given, with no allowlist matching attempted (design.md's "No
/// curated server list, by explicit choice"). `Serialize`/`Deserialize`
/// so the Settings UI's own manual "add server" form (Group 5.2) can
/// build one directly and send it across IPC, feeding the same
/// `attempt_connection` the chat-triggered path uses.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpConnectRequest {
    Stdio {
        command: String,
        args: Vec<String>,
        /// Both present together, or neither -- the environment
        /// variable to inject a credential's value under, and the
        /// value itself. Optional: most stdio servers (filesystem,
        /// git, local dev tools) need no credential at all.
        credential_env_var: Option<String>,
        credential_value: Option<String>,
    },
    Http {
        url: String,
    },
}

/// What [`McpConnector::connect`] resolved to -- the three-way branch
/// design.md's own Decision requires of any implementation: approved
/// and ready (tools now available, starting the next message), still
/// waiting on OAuth (must never block the rest of the conversation --
/// see the `mcp` capability's own "does not block the rest of the
/// conversation" scenario), or not approved/failed. `Serialize` so the
/// Settings UI's manual "add server" command (Group 5.2) can return
/// one directly to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum McpConnectOutcome {
    Connected {
        display_name: String,
        tool_count: usize,
    },
    /// The resolved command/URL already matches a connected server --
    /// returned instead of silently attempting to reconnect (or
    /// re-running an OAuth dance for something already authorized).
    /// Exists because a model can, and in real usage did, ask to
    /// connect to something already connected (`tool-list-optimization`'s
    /// own real-world trigger: a model reaching for `connect_mcp_server`
    /// out of habit instead of `search_tools`) -- this turns that wrong
    /// guess into a corrective signal pointing at `search_tools`,
    /// rather than a dead end.
    AlreadyConnected {
        display_name: String,
        tool_count: usize,
    },
    PendingAuthorization {
        display_name: String,
    },
    Declined,
    Failed {
        reason: String,
    },
}

/// Mirrors [`PermissionDecider`]'s own shape (design.md's chosen
/// resolution for how `connect_mcp_server`'s dispatch reaches
/// `AppHandle`-level capability without `agent_runtime.rs` taking on a
/// `tauri` dependency): defined abstractly here, implemented
/// concretely with a real `AppHandle` in the app crate, passed into
/// `run()` the same way `permission: &dyn PermissionDecider` already
/// is. `connect()` must itself resolve the confirm/connect/OAuth-pending
/// split -- `run()`'s own dispatch just awaits it in place, the same
/// way it already awaits `permission.decide(...)` for any other
/// `Confirm`-tier call; no new control-flow concept is needed here.
#[async_trait::async_trait]
pub trait McpConnector: Send + Sync {
    async fn connect(&self, request: McpConnectRequest) -> McpConnectOutcome;
}

/// A safe, never-reached default for a call site whose own registry
/// never actually offers `connect_mcp_server` -- the recursive
/// `delegate_task` call below, and `mix-mode-local-tools`'s own
/// continuation call in the app crate -- mirroring `AlwaysDenyConfirm`'s
/// own shape for the same reason. Reached only if that exclusion ever
/// regressed, in which case this fails the call safely rather than
/// doing anything.
pub struct NeverConnectMcp;

#[async_trait::async_trait]
impl McpConnector for NeverConnectMcp {
    async fn connect(&self, _request: McpConnectRequest) -> McpConnectOutcome {
        McpConnectOutcome::Failed {
            reason: "connecting to an MCP server is not available here".to_string(),
        }
    }
}

/// `connect_mcp_server`'s own schema -- registered normally so the
/// model can discover and call it like any other tool.
pub fn connect_mcp_server_definition() -> ToolDefinition {
    ToolDefinition {
        name: CONNECT_MCP_SERVER_TOOL_NAME.to_string(),
        description: "Connects to a NEW third-party MCP (Model Context Protocol) server, making \
                      its own tools available starting your next message. Before calling this, \
                      check whether the capability you actually need already exists among \
                      already-connected servers' own tools -- call search_tools for that, if it's \
                      offered; connecting again to something already connected just reports that \
                      back without doing anything new. Use this tool only when what you need \
                      genuinely isn't connected yet. Resolve the server's exact command (for a \
                      local/stdio server) or URL (for a remote/HTTP server) from the user's \
                      request or your own knowledge -- there is no pre-vetted list to match \
                      against. The user will be shown exactly what you resolved and must approve \
                      it before anything connects."
            .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "transport": {
                    "type": "string",
                    "enum": ["stdio", "http"],
                    "description": "\"stdio\" for a local server run as a subprocess, \"http\" \
                                     for a remote server reached by URL."
                },
                "command": {
                    "type": "string",
                    "description": "Required for \"stdio\": the command to run."
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional for \"stdio\": the command's own arguments."
                },
                "url": {
                    "type": "string",
                    "description": "Required for \"http\": the server's own URL."
                },
                "credential_env_var": {
                    "type": "string",
                    "description": "Optional, \"stdio\" only: the environment variable name a \
                                     credential should be injected under, if this server needs \
                                     one. Must be given together with credential_value."
                },
                "credential_value": {
                    "type": "string",
                    "description": "Optional, \"stdio\" only: the credential's own value. Must \
                                     be given together with credential_env_var."
                }
            },
            "required": ["transport"],
        }),
    }
}

/// `connect_mcp_server`'s own `Tool` impl exists only so its
/// `definition()` participates in the normal `ToolRegistry`/model-
/// discovery machinery, mirroring `DelegateTool`'s own unreachable-
/// `execute()` shape for the identical reason: `run()`'s own dispatch
/// intercepts this reserved name before ever reaching the generic
/// `Tool::execute()` path.
pub struct ConnectMcpServerTool;

#[async_trait::async_trait]
impl Tool for ConnectMcpServerTool {
    fn definition(&self) -> ToolDefinition { connect_mcp_server_definition() }

    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Auto
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::error(
            "connect_mcp_server was dispatched through the generic tool path, which should be \
             unreachable -- this is an internal bug, not a user-facing failure"
                .to_string(),
        ))
    }
}

/// Parses `connect_mcp_server`'s own arguments into a request, or
/// `None` for anything malformed (an unrecognized `transport`, or a
/// transport missing the field it requires) -- the same "malformed
/// arguments" bucket every other tool call's bad input already falls
/// into, not a new error shape.
fn parse_mcp_connect_request(arguments: &Value) -> Option<McpConnectRequest> {
    match arguments.get("transport").and_then(Value::as_str) {
        Some("stdio") => {
            let command = arguments.get("command").and_then(Value::as_str)?.to_string();
            let args = arguments
                .get("args")
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            let credential_env_var =
                arguments.get("credential_env_var").and_then(Value::as_str).map(str::to_string);
            let credential_value =
                arguments.get("credential_value").and_then(Value::as_str).map(str::to_string);
            Some(McpConnectRequest::Stdio { command, args, credential_env_var, credential_value })
        }
        Some("http") => {
            let url = arguments.get("url").and_then(Value::as_str)?.to_string();
            Some(McpConnectRequest::Http { url })
        }
        _ => None,
    }
}

/// The reserved name `run()` recognizes to dispatch a lazy-tool search
/// specially -- same mechanism as [`DELEGATE_TOOL_NAME`]/
/// [`CONNECT_MCP_SERVER_TOOL_NAME`]: special-cased *inside* `run()`'s
/// own per-iteration loop, since matching against `registry`'s own
/// lazy tools and mutating this call's own `unlocked` set both need
/// things a plain `Tool::execute()` can't reach.
pub const SEARCH_TOOLS_NAME: &str = "search_tools";

/// `search_tools`'s own schema -- its `description` is generated fresh
/// each time from `lazy_summaries` (name + description for every lazy
/// tool in the registry this call is built for), which is what
/// actually tells the model such tools exist at all and roughly what
/// they're for, before it has any reason to search for one by name.
/// Mirrors Claude Code's own deferred-tool reminder for the identical
/// purpose (verified via web search during `tool-list-optimization`'s
/// own exploration): names and short descriptions stay visible every
/// turn; full schemas don't, until asked for.
/// How much of each lazy tool's own description gets embedded into
/// `search_tools`'s own description -- without a cap, a real MCP
/// server's own (often verbose, example-laden) tool descriptions can
/// make this single field dominate the whole prompt once more than a
/// couple of tools are connected, confirmed by a real report (persona/
/// language instructions silently lost, presumed truncated out of a
/// context window this field alone was eating into). A name plus a
/// short hint is enough to make a tool findable by `search_tools`;
/// nothing here needs the full, unbounded description.
const LAZY_SUMMARY_MAX_CHARS: usize = 80;

fn truncate_for_summary(text: &str) -> String {
    if text.chars().count() <= LAZY_SUMMARY_MAX_CHARS {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(LAZY_SUMMARY_MAX_CHARS).collect();
    truncated.push('\u{2026}'); // "…"
    truncated
}

pub fn search_tools_definition(lazy_summaries: &[(String, String)]) -> ToolDefinition {
    let mut description = String::from(
        "Check here BEFORE connecting to any new MCP server: the following tools already exist \
         from servers that are already connected, and may already cover what you need. Call this \
         with a name or keyword to load one's full schema, making it directly callable starting \
         your very next tool call -- calling one of them directly before searching for it will \
         fail. The following tools exist but need this call first:\n",
    );
    for (name, summary) in lazy_summaries {
        description.push_str(&format!("- {name}: {}\n", truncate_for_summary(summary)));
    }
    ToolDefinition {
        name: SEARCH_TOOLS_NAME.to_string(),
        description,
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A tool's name, or a keyword from its purpose."
                }
            },
            "required": ["query"],
        }),
    }
}

/// `search_tools`'s own `Tool` impl exists only so its `definition()`
/// participates in the normal `ToolRegistry`/model-discovery machinery
/// -- mirroring `DelegateTool`'s own unreachable-`execute()` shape,
/// for the identical reason: `run()`'s own dispatch intercepts this
/// reserved name before ever reaching the generic `Tool::execute()`
/// path. Holds the same `lazy_summaries` the registry it's built
/// alongside was given, purely so `definition()` can regenerate its
/// own description text on demand without needing registry access
/// (which a plain `Tool` method has no way to reach anyway).
pub struct SearchToolsTool {
    lazy_summaries: Vec<(String, String)>,
}

impl SearchToolsTool {
    pub fn new(lazy_summaries: Vec<(String, String)>) -> Self { Self { lazy_summaries } }
}

#[async_trait::async_trait]
impl Tool for SearchToolsTool {
    fn definition(&self) -> ToolDefinition { search_tools_definition(&self.lazy_summaries) }

    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Auto
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::error(
            "search_tools was dispatched through the generic tool path, which should be \
             unreachable -- this is an internal bug, not a user-facing failure"
                .to_string(),
        ))
    }
}

/// Decides how every `PermissionTier::Confirm` call from one model
/// turn actually resolves -- `Auto`/`Deny` need no decision (the tier
/// itself is the answer); only `Confirm` calls ever reach this. Kept as
/// its own trait so Group 6's real popup-backed implementation is
/// swappable for a trivial always-approve/always-deny one in tests,
/// without either depending on Tauri.
#[async_trait::async_trait]
pub trait PermissionDecider: Send + Sync {
    /// Returns the `id`s of every call in `calls` that is approved;
    /// any `id` not present is treated as denied.
    async fn decide(&self, calls: &[PendingToolCall]) -> HashSet<String>;
}

/// The tools available to one `AgentRuntime::run` call. A thin,
/// immutable lookup -- ownership/lifecycle of the underlying `Tool`
/// implementations (Group 5's 4 native tools) belongs to whatever
/// constructs the registry per turn, not to this type.
///
/// `eager` tools' full definitions are sent to the provider every
/// iteration, same as this type has always worked. `lazy` tools
/// (`tool-list-optimization`'s own "lazy loading" option, mirroring
/// how Claude Code itself avoids sending every MCP tool's full schema
/// every turn) are, by default, named only -- via [`SearchToolsTool`]'s
/// own dynamically-generated description -- not sent in full until the
/// model actually searches for one (`SEARCH_TOOLS_NAME`'s own
/// dispatch, inside `run()`). Once unlocked, a lazy tool behaves
/// exactly like an eager one for the rest of that `run()` call;
/// nothing persists it past that one call, so the next message's own
/// `run()` starts the search over -- a deliberate v1 simplification
/// (per-turn, not per-conversation), not an oversight.
pub struct ToolRegistry {
    eager: Vec<Arc<dyn Tool>>,
    lazy: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Every tool here is eager -- the registry behaves exactly as it
    /// always has for a caller that never uses `with_lazy`.
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self { Self { eager: tools, lazy: Vec::new() } }

    /// `lazy` tools are only named (not fully defined) until the model
    /// searches for one -- see this type's own doc comment.
    pub fn with_lazy(eager: Vec<Arc<dyn Tool>>, lazy: Vec<Arc<dyn Tool>>) -> Self {
        Self { eager, lazy }
    }

    /// Every eager tool's definition, unconditionally -- what
    /// `run()`'s very first iteration (`unlocked` always starts empty)
    /// and every caller that never uses `with_lazy` both see.
    pub fn definitions(&self) -> Vec<ToolDefinition> { self.definitions_for(&HashSet::new()) }

    /// Every eager tool's definition, plus any lazy tool's definition
    /// whose name is already in `unlocked` -- what `run()` actually
    /// sends the provider for one iteration once some lazy tools have
    /// been searched for.
    fn definitions_for(&self, unlocked: &HashSet<String>) -> Vec<ToolDefinition> {
        self.eager
            .iter()
            .chain(self.lazy.iter().filter(|tool| unlocked.contains(&tool.definition().name)))
            .map(|tool| tool.definition())
            .collect()
    }

    /// Every lazy tool's own name and description -- what
    /// `SearchToolsTool`'s own dynamically-generated description lists
    /// (the "these exist, here's roughly what they do" signal the
    /// model needs before it has any reason to search for one by
    /// name), and what `lazy_tools_matching` searches over.
    pub fn lazy_summaries(&self) -> Vec<(String, String)> {
        self.lazy
            .iter()
            .map(|tool| (tool.definition().name, tool.definition().description))
            .collect()
    }

    /// Every lazy tool whose name or description contains `query`
    /// (case-insensitive substring match -- deliberately simple; v1
    /// has no need for anything fancier, and a query matching nothing
    /// is reported back to the model as such, not treated as an error).
    fn lazy_tools_matching(&self, query: &str) -> Vec<Arc<dyn Tool>> {
        let query = query.to_lowercase();
        self.lazy
            .iter()
            .filter(|tool| {
                let def = tool.definition();
                def.name.to_lowercase().contains(&query)
                    || def.description.to_lowercase().contains(&query)
            })
            .cloned()
            .collect()
    }

    /// Whether `name` is a *lazy* tool in this registry specifically
    /// (as opposed to an eager one, or simply absent) -- `run()`'s own
    /// classification uses this to reject a call to a lazy tool that
    /// hasn't been unlocked yet, the same way Claude Code's own
    /// deferred tools fail a direct call before being searched for.
    fn is_lazy(&self, name: &str) -> bool {
        self.lazy.iter().any(|tool| tool.definition().name == name)
    }

    /// How many eager and lazy tools this registry holds, respectively
    /// -- diagnostic-only (e.g. logging how large a turn's own tool
    /// list is), not used by `run()`'s own dispatch.
    pub fn tool_counts(&self) -> (usize, usize) { (self.eager.len(), self.lazy.len()) }

    pub fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.eager
            .iter()
            .chain(self.lazy.iter())
            .find(|tool| tool.definition().name == name)
            .cloned()
    }

    /// A copy of this registry excluding the named tool -- used to
    /// build a delegated sub-task's own registry
    /// (`sub-agent-delegation`'s depth-1 cap: a delegated sub-task's
    /// registry simply omits `DELEGATE_TOOL_NAME`, so recursive
    /// delegation is impossible by construction, not by a
    /// runtime-checked counter). Preserves the eager/lazy split --
    /// excluding `DELEGATE_TOOL_NAME`/`CONNECT_MCP_SERVER_TOOL_NAME`
    /// (both always eager) never touches `lazy` at all.
    pub fn without(&self, name: &str) -> Self {
        Self {
            eager: self
                .eager
                .iter()
                .filter(|tool| tool.definition().name != name)
                .cloned()
                .collect(),
            lazy: self.lazy.iter().filter(|tool| tool.definition().name != name).cloned().collect(),
        }
    }
}

/// Why `AgentRuntime::run` failed to produce a final answer at all --
/// distinct from a tool call being denied or erroring, both of which
/// are reported *back to the model* as a normal tool-result message
/// (the "A denied call is reported back to the model" requirement) and
/// never surface here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    Provider(ProviderError),
    /// The model kept requesting tool calls without ever producing a
    /// final answer, for the fixed maximum number of iterations in a
    /// row -- carries whatever text accompanied that final iteration
    /// (often empty), so a caller can still show *something* rather
    /// than nothing.
    MaxIterationsReached {
        partial_text: String,
    },
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(err) => write!(f, "{err}"),
            Self::MaxIterationsReached { .. } => {
                write!(f, "the model did not produce a final answer within the iteration limit")
            }
        }
    }
}

impl std::error::Error for AgentError {}

/// The final answer text plus a trace of every tool call attempted
/// during the turn, in the order the model requested them
/// (`execution-log-and-context`'s "Tool call outcomes survive the turn
/// that produced them"). The trace is empty for a turn that made no
/// tool calls at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOutcome {
    pub text: String,
    pub trace: Vec<ToolInvocation>,
}

/// Runs the tool-calling loop for a `ToolCallingProvider`. Implementers
/// never need to manage `Conversation`/session lifecycle themselves --
/// `run`'s job ends at producing the final answer and its tool trace,
/// the same boundary `TaskRouter::route()` draws for itself.
///
/// `on_text_delta` is invoked for every [`ToolCallStreamItem::TextDelta`]
/// as it arrives, on *every* iteration -- not just the final one that
/// ends the loop -- so a caller can forward it live to the UI the same
/// way every existing (non-tool-calling) chat path already streams
/// `ChatEvent::Chunk`s. Tool-call JSON and tool execution stay
/// invisible either way; only real model-generated text ever reaches
/// this callback. A model that narrates before calling a tool
/// therefore still streams that narration live, exactly as if the tool
/// call never happened. The trace returned in `AgentOutcome` is a
/// separate, after-the-fact summary -- not something `on_text_delta`
/// or any other callback surfaces live.
#[async_trait::async_trait]
pub trait AgentRuntime: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    async fn run(
        &self,
        provider: &dyn ToolCallingProvider,
        messages: Vec<Message>,
        registry: &ToolRegistry,
        ctx: &ToolContext,
        permission: &dyn PermissionDecider,
        mcp_connector: &dyn McpConnector,
        on_text_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<AgentOutcome, AgentError>;
}

/// The runtime this change actually ships. `max_iterations` guards
/// against the "Agent loop runaway" risk the planning doc calls out --
/// 8 is a generous-but-bounded default: enough for a call, a follow-up
/// call informed by the first result, and headroom beyond that, while
/// still guaranteeing the loop cannot run forever.
pub struct AemeathAgentRuntime {
    pub max_iterations: usize,
}

impl Default for AemeathAgentRuntime {
    fn default() -> Self { Self { max_iterations: 8 } }
}

/// Turns a permitted tool's own outcome into the tool-result message
/// text the model sees next -- `is_error` gets a plain, model-legible
/// prefix rather than a separate wire field (Ollama's own tool-result
/// message shape has no error flag to carry one in).
fn tool_result_content(result: ToolResult) -> String {
    if result.is_error {
        format!("Error: {}", result.content)
    } else {
        result.content
    }
}

#[async_trait::async_trait]
impl AgentRuntime for AemeathAgentRuntime {
    #[allow(clippy::too_many_arguments)]
    async fn run(
        &self,
        provider: &dyn ToolCallingProvider,
        mut messages: Vec<Message>,
        registry: &ToolRegistry,
        ctx: &ToolContext,
        permission: &dyn PermissionDecider,
        mcp_connector: &dyn McpConnector,
        on_text_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<AgentOutcome, AgentError> {
        let mut last_text = String::new();
        let mut trace: Vec<ToolInvocation> = Vec::new();
        // Which of `registry`'s own lazy tools have been searched for
        // (`SEARCH_TOOLS_NAME`'s own dispatch, below) during this
        // `run()` call so far -- starts empty every call, persists
        // across this call's own iterations, never carried into a
        // later call (including a delegated child's own recursive
        // one, which gets its own fresh empty set).
        let mut unlocked: HashSet<String> = HashSet::new();

        for _ in 0..self.max_iterations {
            let mut stream = provider
                .chat_with_tools(messages.clone(), registry.definitions_for(&unlocked))
                .await
                .map_err(AgentError::Provider)?;

            let mut text = String::new();
            let mut calls = Vec::new();
            while let Some(item) = stream.next().await {
                match item.map_err(AgentError::Provider)? {
                    ToolCallStreamItem::TextDelta(delta) => {
                        on_text_delta(&delta);
                        text.push_str(&delta);
                    }
                    ToolCallStreamItem::ToolCall { id, name, arguments } => {
                        calls.push(PendingToolCall { id, name, arguments })
                    }
                }
            }
            last_text = text.clone();

            if calls.is_empty() {
                return Ok(AgentOutcome { text, trace });
            }

            messages.push(Message::assistant_with_tool_calls(
                text,
                calls
                    .iter()
                    .map(|call| ToolCallRecord {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .collect(),
            ));

            // Schema validation (tool exists, arguments is an object)
            // and a `Deny` decision both resolve the same way -- a
            // tool-result message telling the model the call didn't
            // run -- so they're classified in the same pass as `Auto`/
            // `Confirm`, below. Classification itself is synchronous
            // (no `.await`), so it stays a single ordered pass; only
            // `Auto`-tier *execution* is deferred and run concurrently,
            // in the next pass.
            let plans: Vec<CallPlan> = calls
                .iter()
                .map(|call| {
                    // `registry.find()` first, for every call, before
                    // any name-based special-casing -- this is what
                    // makes `delegate_task` fall through to the normal
                    // `UnknownTool` rejection for a delegated sub-task
                    // (whose own registry was built via `.without(
                    // DELEGATE_TOOL_NAME)`), rather than being treated
                    // as delegation by name alone regardless of
                    // whether this registry actually offers it.
                    let Some(tool) = registry.find(&call.name) else {
                        return CallPlan::UnknownTool;
                    };
                    if call.name == DELEGATE_TOOL_NAME {
                        return match call.arguments.get("task").and_then(Value::as_str) {
                            Some(task) => CallPlan::Delegate(task.to_string()),
                            None => CallPlan::MalformedArguments,
                        };
                    }
                    if call.name == CONNECT_MCP_SERVER_TOOL_NAME {
                        return match parse_mcp_connect_request(&call.arguments) {
                            Some(request) => CallPlan::ConnectMcp(request),
                            None => CallPlan::MalformedArguments,
                        };
                    }
                    if call.name == SEARCH_TOOLS_NAME {
                        return match call.arguments.get("query").and_then(Value::as_str) {
                            Some(query) => CallPlan::SearchTools(query.to_string()),
                            None => CallPlan::MalformedArguments,
                        };
                    }
                    // A lazy tool that hasn't been searched for yet
                    // this turn -- `registry.find()` above already
                    // succeeded (the tool genuinely exists), but it
                    // isn't *offered* yet (`definitions_for` above
                    // never sent its schema this iteration), so a
                    // direct call to it is rejected the same way
                    // Claude Code's own deferred tools fail before
                    // being searched for.
                    if registry.is_lazy(&call.name) && !unlocked.contains(&call.name) {
                        return CallPlan::ToolNotYetUnlocked;
                    }
                    if !call.arguments.is_object() {
                        return CallPlan::MalformedArguments;
                    }
                    match tool.required_permission(&call.arguments, ctx) {
                        PermissionTier::Auto => CallPlan::Auto(tool),
                        PermissionTier::Deny => CallPlan::Denied,
                        PermissionTier::Confirm => CallPlan::Confirm(tool),
                    }
                })
                .collect();

            // `Auto`-tier calls run concurrently, bounded by
            // `MAX_CONCURRENT_TOOL_CALLS` -- but `buffer_unordered`
            // yields results in *completion* order, not the order the
            // calls were originally requested in. Ollama pairs tool
            // results with calls by position, not by id (unlike
            // OpenAI/Anthropic, which use `tool_call_id`/`tool_use_id`
            // explicitly) -- see `message.rs`'s own doc comment on
            // `Message::tool_call_id` -- so results are written into a
            // slot indexed by each call's *original* position here,
            // and only read back out in that same order in the
            // writeback pass below, regardless of which finished first.
            let mut auto_results: Vec<Option<(bool, String)>> =
                calls.iter().map(|_| None).collect();

            // `SearchTools` is resolved synchronously, before the
            // concurrent pass below -- matching against `registry`'s
            // own already-in-memory lazy tool definitions needs no
            // `.await`, and mutating `unlocked` (a plain local
            // variable, not behind any lock) from inside a future
            // `buffer_unordered` runs concurrently would need
            // synchronization this doesn't otherwise need at all.
            // Writing straight into `auto_results` here, ahead of the
            // concurrent pass filling in its own entries, is what lets
            // the writeback pass below treat every one of
            // `Auto`/`Delegate`/`ConnectMcp`/`SearchTools` uniformly.
            for (index, plan) in plans.iter().enumerate() {
                let CallPlan::SearchTools(query) = plan else { continue };
                let matches = registry.lazy_tools_matching(query);
                let content = if matches.is_empty() {
                    format!(
                        "No not-yet-loaded tools matched \"{query}\". Try a different word, or \
                         call {SEARCH_TOOLS_NAME} again with a broader query."
                    )
                } else {
                    let mut text = String::new();
                    for tool in &matches {
                        let definition = tool.definition();
                        unlocked.insert(definition.name.clone());
                        text.push_str(&format!(
                            "\"{}\" is now available to call directly: {}\nParameters schema: \
                             {}\n\n",
                            definition.name, definition.description, definition.parameters
                        ));
                    }
                    text
                };
                auto_results[index] = Some((true, content));
            }

            // Built via a plain loop, not `.filter_map(closure)` -- a
            // closure returning `impl Future` here hits rustc's HRTB
            // inference limit (it cannot unify the borrowed-`call`
            // lifetime across every closure invocation); boxing each
            // future explicitly sidesteps that by giving `stream::iter`
            // one concrete, uniform item type instead.
            let mut auto_futures: Vec<
                std::pin::Pin<
                    Box<dyn std::future::Future<Output = (usize, bool, String)> + Send + '_>,
                >,
            > = Vec::new();
            for (index, (call, plan)) in calls.iter().zip(plans.iter()).enumerate() {
                match plan {
                    CallPlan::Auto(tool) => {
                        let tool = tool.clone();
                        let arguments = call.arguments.clone();
                        auto_futures.push(Box::pin(async move {
                            let result = execute(&tool, arguments, ctx).await;
                            (index, !result.is_error, tool_result_content(result))
                        }));
                    }
                    CallPlan::Delegate(task) => {
                        // A delegated sub-task's own registry omits
                        // `DELEGATE_TOOL_NAME` (depth-1, enforced
                        // structurally, not by a counter) and starts
                        // from a plain, non-persona system prompt plus
                        // the task alone -- no parent history, no
                        // projected context (design.md's Decisions 2
                        // and 5). Its own narration never reaches this
                        // turn's `on_text_delta`: passing a discarding
                        // closure instead of the caller's real one is
                        // what actually keeps it invisible while it
                        // runs, not merely a policy left unenforced.
                        // Runs through this same concurrent pass, so
                        // several delegations in one turn share the
                        // same `MAX_CONCURRENT_TOOL_CALLS` bound as
                        // every other tool call, not a separate limit.
                        let task = task.clone();
                        // Also excludes `CONNECT_MCP_SERVER_TOOL_NAME`,
                        // for the same reason it excludes itself:
                        // opening a browser/showing a real confirmation
                        // popup from inside an invisible, no-history
                        // delegated sub-task is the same category of
                        // blast-radius concern `run_command`/`delegate_task`
                        // are already kept out of a child's own registry
                        // for.
                        let child_registry = registry
                            .without(DELEGATE_TOOL_NAME)
                            .without(CONNECT_MCP_SERVER_TOOL_NAME);
                        auto_futures.push(Box::pin(async move {
                            let child_messages = vec![
                                Message::system(crate::prompt::delegated_task_system_prompt()),
                                Message::user(task),
                            ];
                            let outcome = self
                                .run(
                                    provider,
                                    child_messages,
                                    &child_registry,
                                    ctx,
                                    permission,
                                    &NeverConnectMcp,
                                    &mut |_: &str| {},
                                )
                                .await;
                            match outcome {
                                Ok(AgentOutcome { text, .. }) => (index, true, text),
                                Err(err) => {
                                    (index, false, format!("delegated sub-task failed: {err}"))
                                }
                            }
                        }));
                    }
                    CallPlan::ConnectMcp(request) => {
                        // `mcp_connector.connect()` is awaited in place,
                        // the same way `permission.decide(...)` already
                        // is for any other `Confirm`-tier call -- its
                        // own implementation resolves the confirm/
                        // connect/OAuth-pending split and must never
                        // itself block on OAuth completion (design.md's
                        // own Decision); there is no new control-flow
                        // concept for `run()` to learn here.
                        let request = request.clone();
                        auto_futures.push(Box::pin(async move {
                            let (ok, content) = match mcp_connector.connect(request).await {
                                McpConnectOutcome::Connected { display_name, tool_count } => (
                                    true,
                                    format!(
                                        "Connected to \"{display_name}\" -- {tool_count} tool(s) \
                                         will be available starting your next message."
                                    ),
                                ),
                                McpConnectOutcome::AlreadyConnected {
                                    display_name,
                                    tool_count,
                                } => (
                                    true,
                                    format!(
                                        "\"{display_name}\" is already connected, with \
                                         {tool_count} tool(s) available right now -- call \
                                         {SEARCH_TOOLS_NAME} to find and use one of them \
                                         directly; connecting again was not needed and nothing \
                                         new happened."
                                    ),
                                ),
                                McpConnectOutcome::PendingAuthorization { display_name } => (
                                    true,
                                    format!(
                                        "Started authorizing \"{display_name}\" -- the user needs \
                                         to finish in their browser; its tools will be available \
                                         once that's done."
                                    ),
                                ),
                                McpConnectOutcome::Declined => {
                                    (false, "the user did not approve this connection".to_string())
                                }
                                McpConnectOutcome::Failed { reason } => (false, reason),
                            };
                            (index, ok, content)
                        }));
                    }
                    _ => {}
                }
            }
            let completed: Vec<(usize, bool, String)> = stream::iter(auto_futures)
                .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS)
                .collect()
                .await;
            for (index, ok, content) in completed {
                auto_results[index] = Some((ok, content));
            }

            // Writeback, in original request order -- the ordering
            // guarantee the pass above exists to preserve.
            let mut pending_confirm: Vec<(PendingToolCall, Arc<dyn Tool>)> = Vec::new();
            for (index, call) in calls.into_iter().enumerate() {
                match &plans[index] {
                    CallPlan::UnknownTool => {
                        messages.push(Message::tool_result(
                            call.id.clone(),
                            format!("Error: unknown tool \"{}\"", call.name),
                        ));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::Rejected {
                                arguments_preview: capped_arguments_preview(&call.arguments),
                            },
                        });
                    }
                    CallPlan::MalformedArguments => {
                        messages.push(Message::tool_result(
                            call.id.clone(),
                            format!("Error: arguments for \"{}\" must be a JSON object", call.name),
                        ));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::Rejected {
                                arguments_preview: capped_arguments_preview(&call.arguments),
                            },
                        });
                    }
                    CallPlan::Denied => {
                        messages.push(Message::tool_result(
                            call.id.clone(),
                            format!("Error: \"{}\" was not permitted to run", call.name),
                        ));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::DeniedByPolicy,
                        });
                    }
                    CallPlan::ToolNotYetUnlocked => {
                        messages.push(Message::tool_result(
                            call.id.clone(),
                            format!(
                                "Error: \"{}\" has not been loaded yet -- call \
                                 {SEARCH_TOOLS_NAME} with its name first",
                                call.name
                            ),
                        ));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::Rejected {
                                arguments_preview: capped_arguments_preview(&call.arguments),
                            },
                        });
                    }
                    // A delegated sub-task's result, a
                    // `connect_mcp_server` outcome, and a `search_tools`
                    // result all fold into this turn's own trace as an
                    // ordinary `Executed` `ToolInvocation` -- no
                    // persisted child `Execution` record (design.md's
                    // Decision 4); the tool-activity note
                    // `execution-log-and-context` already built picks
                    // this up for free on the next turn, same as any
                    // other tool.
                    CallPlan::Auto(_)
                    | CallPlan::Delegate(_)
                    | CallPlan::ConnectMcp(_)
                    | CallPlan::SearchTools(_) => {
                        let (ok, content) = auto_results[index].take().expect(
                            "every Auto-tier/Delegate/ConnectMcp/SearchTools call has a result by \
                             the writeback pass",
                        );
                        messages.push(Message::tool_result(call.id.clone(), content));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::Executed { ok },
                        });
                    }
                    CallPlan::Confirm(tool) => pending_confirm.push((call, tool.clone())),
                }
            }

            if !pending_confirm.is_empty() {
                let batch: Vec<PendingToolCall> =
                    pending_confirm.iter().map(|(call, _)| call.clone()).collect();
                let approved = permission.decide(&batch).await;
                for (call, tool) in pending_confirm {
                    if approved.contains(&call.id) {
                        let id = call.id.clone();
                        let name = call.name.clone();
                        let result = execute(&tool, call.arguments, ctx).await;
                        let ok = !result.is_error;
                        messages.push(Message::tool_result(id, tool_result_content(result)));
                        trace.push(ToolInvocation { name, outcome: ToolOutcome::Executed { ok } });
                    } else {
                        messages.push(Message::tool_result(
                            call.id.clone(),
                            format!("Error: \"{}\" was not permitted to run", call.name),
                        ));
                        trace.push(ToolInvocation {
                            name: call.name.clone(),
                            outcome: ToolOutcome::DeclinedByUser,
                        });
                    }
                }
            }
        }

        Err(AgentError::MaxIterationsReached { partial_text: last_text })
    }
}

async fn execute(tool: &Arc<dyn Tool>, args: Value, ctx: &ToolContext) -> ToolResult {
    match tool.execute(args, ctx).await {
        Ok(result) => result,
        Err(err) => ToolResult::error(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures_util::stream::{iter, BoxStream};
    use serde_json::json;

    use super::*;
    use crate::{
        conversation::ConversationId,
        message::{ModelInfo, ProviderKind},
        provider::{AiProvider, ChatStream},
        tool_provider::ToolCallStream,
    };

    fn ctx() -> ToolContext {
        ToolContext {
            project_root: None,
            conversation_id: ConversationId::from_session_path(std::path::Path::new("/tmp/x")),
        }
    }

    // -- sub-agent-delegation: DELEGATE_TOOL_NAME / delegate_task_definition /
    // ToolRegistry::without --

    #[test]
    fn delegate_task_definition_has_the_reserved_name_and_a_valid_schema() {
        let definition = delegate_task_definition();
        assert_eq!(definition.name, DELEGATE_TOOL_NAME);
        assert_eq!(definition.parameters["type"], "object");
        assert_eq!(definition.parameters["required"], json!(["task"]));
        assert_eq!(definition.parameters["properties"]["task"]["type"], "string");
    }

    #[test]
    fn delegate_tool_is_auto_tier_and_matches_the_reserved_definition() {
        let tool = DelegateTool;
        assert_eq!(tool.required_permission(&json!({"task": "x"}), &ctx()), PermissionTier::Auto);
        assert_eq!(tool.definition().name, DELEGATE_TOOL_NAME);
    }

    #[test]
    fn connect_mcp_server_definition_has_the_reserved_name_and_a_valid_schema() {
        let definition = connect_mcp_server_definition();
        assert_eq!(definition.name, CONNECT_MCP_SERVER_TOOL_NAME);
        assert_eq!(definition.parameters["type"], "object");
        assert_eq!(definition.parameters["required"], json!(["transport"]));
        assert_eq!(
            definition.parameters["properties"]["transport"]["enum"],
            json!(["stdio", "http"])
        );
    }

    #[test]
    fn connect_mcp_server_tool_matches_the_reserved_definition() {
        let tool = ConnectMcpServerTool;
        assert_eq!(tool.definition().name, CONNECT_MCP_SERVER_TOOL_NAME);
    }

    #[test]
    fn parse_mcp_connect_request_handles_both_transports() {
        assert_eq!(
            parse_mcp_connect_request(
                &json!({"transport": "stdio", "command": "npx", "args": ["-y", "pkg"]})
            ),
            Some(McpConnectRequest::Stdio {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "pkg".to_string()],
                credential_env_var: None,
                credential_value: None,
            })
        );
        assert_eq!(
            parse_mcp_connect_request(&json!({"transport": "stdio", "command": "cat"})),
            Some(McpConnectRequest::Stdio {
                command: "cat".to_string(),
                args: vec![],
                credential_env_var: None,
                credential_value: None,
            }),
            "args is optional, defaulting to empty"
        );
        assert_eq!(
            parse_mcp_connect_request(
                &json!({"transport": "http", "url": "https://example.com/sse"})
            ),
            Some(McpConnectRequest::Http { url: "https://example.com/sse".to_string() })
        );
    }

    #[test]
    fn parse_mcp_connect_request_picks_up_a_stdio_credential() {
        let request = parse_mcp_connect_request(&json!({
            "transport": "stdio",
            "command": "github-mcp-server",
            "credential_env_var": "GITHUB_TOKEN",
            "credential_value": "ghp_secret",
        }));
        assert_eq!(
            request,
            Some(McpConnectRequest::Stdio {
                command: "github-mcp-server".to_string(),
                args: vec![],
                credential_env_var: Some("GITHUB_TOKEN".to_string()),
                credential_value: Some("ghp_secret".to_string()),
            })
        );
    }

    #[test]
    fn parse_mcp_connect_request_rejects_malformed_or_unknown_transports() {
        assert_eq!(
            parse_mcp_connect_request(&json!({"transport": "stdio"})),
            None,
            "missing command"
        );
        assert_eq!(parse_mcp_connect_request(&json!({"transport": "http"})), None, "missing url");
        assert_eq!(parse_mcp_connect_request(&json!({"transport": "carrier_pigeon"})), None);
        assert_eq!(parse_mcp_connect_request(&json!({})), None);
    }

    #[tokio::test]
    async fn connect_mcp_server_dispatch_is_reached_with_the_parsed_request() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: CONNECT_MCP_SERVER_TOOL_NAME.to_string(),
                arguments: json!({"transport": "stdio", "command": "npx", "args": ["-y", "server-fs"]}),
            }],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![Arc::new(ConnectMcpServerTool)]);
        let connector = RecordingMcpConnector {
            requests: Mutex::new(Vec::new()),
            outcome: McpConnectOutcome::Connected {
                display_name: "Local FS".to_string(),
                tool_count: 3,
            },
        };

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &connector,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done");
        assert_eq!(
            result.trace,
            vec![ToolInvocation {
                name: CONNECT_MCP_SERVER_TOOL_NAME.to_string(),
                outcome: ToolOutcome::Executed { ok: true }
            }]
        );
        let requests = connector.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "dispatch must reach the connector exactly once");
        assert_eq!(
            requests[0],
            McpConnectRequest::Stdio {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "server-fs".to_string()],
                credential_env_var: None,
                credential_value: None,
            }
        );
    }

    #[tokio::test]
    async fn an_already_connected_outcome_tells_the_model_to_use_search_tools_instead() {
        // Grounded in the same real report: a model reaching for
        // `connect_mcp_server` for something already connected should
        // get a corrective message pointing at `search_tools`, not a
        // silent reconnect attempt and not a dead end.
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: CONNECT_MCP_SERVER_TOOL_NAME.to_string(),
                arguments: json!({"transport": "http", "url": "https://mcp.notion.com/mcp"}),
            }],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![Arc::new(ConnectMcpServerTool)]);
        let connector = RecordingMcpConnector {
            requests: Mutex::new(Vec::new()),
            outcome: McpConnectOutcome::AlreadyConnected {
                display_name: "Notion".to_string(),
                tool_count: 5,
            },
        };

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &connector,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(
            result.trace,
            vec![ToolInvocation {
                name: CONNECT_MCP_SERVER_TOOL_NAME.to_string(),
                outcome: ToolOutcome::Executed { ok: true }
            }]
        );
        let received = provider.received.lock().unwrap();
        let tool_result = received[1]
            .iter()
            .find(|m| m.role == crate::message::Role::Tool)
            .expect("the connect call's own tool result");
        assert!(tool_result.content.contains("already connected"));
        assert!(
            tool_result.content.contains(SEARCH_TOOLS_NAME),
            "the corrective message must point at search_tools by name: {}",
            tool_result.content
        );
    }

    #[tokio::test]
    async fn a_malformed_connect_request_never_reaches_the_connector() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: CONNECT_MCP_SERVER_TOOL_NAME.to_string(),
                arguments: json!({"transport": "stdio"}),
            }],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![Arc::new(ConnectMcpServerTool)]);
        let connector = RecordingMcpConnector {
            requests: Mutex::new(Vec::new()),
            outcome: McpConnectOutcome::Declined,
        };

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &connector,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert!(matches!(
            result.trace.as_slice(),
            [ToolInvocation { outcome: ToolOutcome::Rejected { .. }, .. }]
        ));
        assert!(
            connector.requests.lock().unwrap().is_empty(),
            "a malformed request must never reach the connector"
        );
    }

    #[test]
    fn registry_without_excludes_only_the_named_tool() {
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Auto));
        let registry =
            ToolRegistry::new(vec![alpha.clone(), Arc::new(DelegateTool) as Arc<dyn Tool>]);
        assert_eq!(registry.definitions().len(), 2);

        let without_delegate = registry.without(DELEGATE_TOOL_NAME);
        let names: Vec<String> =
            without_delegate.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["alpha".to_string()]);
        assert!(without_delegate.find(DELEGATE_TOOL_NAME).is_none());
        assert!(without_delegate.find("alpha").is_some(), "every other tool is kept unchanged");
    }

    // -- tool-list-optimization: ToolRegistry's own eager/lazy split --

    #[test]
    fn with_lazy_offers_only_eager_definitions_until_something_is_unlocked() {
        let eager = Arc::new(CountingTool::new("eager", PermissionTier::Auto));
        let lazy = Arc::new(CountingTool::new("lazy", PermissionTier::Auto));
        let registry = ToolRegistry::with_lazy(vec![eager], vec![lazy]);

        let names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            vec!["eager".to_string()],
            "a fresh registry's definitions() must not leak lazy tools"
        );

        let mut unlocked = HashSet::new();
        unlocked.insert("lazy".to_string());
        let mut names: Vec<String> =
            registry.definitions_for(&unlocked).into_iter().map(|d| d.name).collect();
        names.sort();
        assert_eq!(names, vec!["eager".to_string(), "lazy".to_string()]);
    }

    #[test]
    fn find_and_is_lazy_see_both_eager_and_lazy_tools() {
        let eager = Arc::new(CountingTool::new("eager", PermissionTier::Auto));
        let lazy = Arc::new(CountingTool::new("lazy", PermissionTier::Auto));
        let registry = ToolRegistry::with_lazy(vec![eager], vec![lazy]);

        assert!(registry.find("eager").is_some());
        assert!(
            registry.find("lazy").is_some(),
            "find() must still see a not-yet-unlocked lazy tool"
        );
        assert!(!registry.is_lazy("eager"));
        assert!(registry.is_lazy("lazy"));
        assert!(!registry.is_lazy("never_heard_of_it"));
    }

    #[test]
    fn without_preserves_the_eager_lazy_split() {
        let eager_keep = Arc::new(CountingTool::new("eager_keep", PermissionTier::Auto));
        let eager_drop = Arc::new(CountingTool::new("eager_drop", PermissionTier::Auto));
        let lazy_keep = Arc::new(CountingTool::new("lazy_keep", PermissionTier::Auto));
        let lazy_drop = Arc::new(CountingTool::new("lazy_drop", PermissionTier::Auto));
        let registry =
            ToolRegistry::with_lazy(vec![eager_keep, eager_drop], vec![lazy_keep, lazy_drop]);

        let filtered = registry.without("eager_drop").without("lazy_drop");

        assert!(filtered.find("eager_keep").is_some());
        assert!(filtered.find("lazy_keep").is_some());
        assert!(filtered.is_lazy("lazy_keep"), "lazy_keep must still be classified as lazy");
        assert!(filtered.find("eager_drop").is_none());
        assert!(filtered.find("lazy_drop").is_none());
    }

    #[test]
    fn lazy_tools_matching_is_a_case_insensitive_substring_match_over_name_or_description() {
        let searchable = Arc::new(CountingTool::new("search_graph", PermissionTier::Auto));
        let other = Arc::new(CountingTool::new("get_system_context", PermissionTier::Auto));
        let registry = ToolRegistry::with_lazy(vec![], vec![searchable, other]);

        let names: Vec<String> =
            registry.lazy_tools_matching("GRAPH").iter().map(|t| t.definition().name).collect();
        assert_eq!(names, vec!["search_graph".to_string()]);

        assert!(registry.lazy_tools_matching("no such thing").is_empty());
    }

    #[test]
    fn lazy_summaries_lists_every_lazy_tools_name_and_description() {
        let lazy = Arc::new(CountingTool::new("lazy", PermissionTier::Auto));
        let registry = ToolRegistry::with_lazy(vec![], vec![lazy]);
        assert_eq!(registry.lazy_summaries(), vec![("lazy".to_string(), String::new())]);
    }

    #[test]
    fn search_tools_definition_embeds_every_lazy_summary_by_name() {
        let definition = search_tools_definition(&[
            ("search_graph".to_string(), "Finds symbols".to_string()),
            ("trace_path".to_string(), "Traces callers".to_string()),
        ]);
        assert_eq!(definition.name, SEARCH_TOOLS_NAME);
        assert!(definition.description.contains("search_graph: Finds symbols"));
        assert!(definition.description.contains("trace_path: Traces callers"));
        assert_eq!(definition.parameters["required"], json!(["query"]));
    }

    #[test]
    fn search_tools_definition_bounds_a_verbose_real_servers_own_description() {
        // Grounded in a real report: a connected server's own tool
        // descriptions (often verbose, example-laden -- real MCP
        // servers write these to be thorough, not terse) can make this
        // one field dominate the whole prompt once more than a couple
        // of tools are connected, confirmed to coincide with the
        // persona/system prompt apparently being lost. One long
        // description must not blow past a bounded size.
        let verbose_description = "x".repeat(500);
        let definition =
            search_tools_definition(&[("notion-fetch".to_string(), verbose_description)]);
        let embedded_line = definition
            .description
            .lines()
            .find(|line| line.starts_with("- notion-fetch:"))
            .unwrap();
        assert!(
            embedded_line.len() < 120,
            "one tool's embedded summary must stay well short of the verbose original: \
             {embedded_line}"
        );
        assert!(embedded_line.ends_with('…'), "a truncated summary must say so visibly");
    }

    #[test]
    fn search_tools_definition_leaves_a_short_summary_untouched() {
        let definition = search_tools_definition(&[("lazy".to_string(), "short".to_string())]);
        assert!(definition.description.contains("- lazy: short\n"));
    }

    // -- tool-list-optimization: run()'s own lazy-loading dispatch --

    #[tokio::test]
    async fn a_lazy_tools_full_schema_is_not_sent_until_it_is_searched_for() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: SEARCH_TOOLS_NAME.to_string(),
                arguments: json!({"query": "lazy"}),
            }],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let lazy = Arc::new(CountingTool::new("lazy", PermissionTier::Auto));
        let search_tool = Arc::new(SearchToolsTool::new(vec![("lazy".to_string(), String::new())]));
        let registry = ToolRegistry::with_lazy(vec![search_tool], vec![lazy.clone()]);

        AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        let received_tools = provider.received_tools.lock().unwrap();
        let first_iteration_names: Vec<&str> =
            received_tools[0].iter().map(|d| d.name.as_str()).collect();
        assert!(
            !first_iteration_names.contains(&"lazy"),
            "the lazy tool's full schema must not be sent before it's searched for: \
             {first_iteration_names:?}"
        );
        assert!(first_iteration_names.contains(&SEARCH_TOOLS_NAME));
    }

    #[tokio::test]
    async fn calling_a_lazy_tool_before_searching_for_it_is_rejected() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "lazy".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let lazy = Arc::new(CountingTool::new("lazy", PermissionTier::Auto));
        let registry = ToolRegistry::with_lazy(vec![], vec![lazy.clone()]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert!(matches!(
            result.trace.as_slice(),
            [ToolInvocation { name, outcome: ToolOutcome::Rejected { .. } }] if name == "lazy"
        ));
        assert_eq!(
            *lazy.calls.lock().unwrap(),
            0,
            "a not-yet-unlocked lazy tool must never execute"
        );
    }

    #[tokio::test]
    async fn searching_then_calling_a_lazy_tool_in_the_same_turn_works() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: SEARCH_TOOLS_NAME.to_string(),
                arguments: json!({"query": "lazy"}),
            }],
            vec![ToolCallStreamItem::ToolCall {
                id: "call_1".to_string(),
                name: "lazy".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let lazy = Arc::new(CountingTool::new("lazy", PermissionTier::Auto));
        let search_tool = Arc::new(SearchToolsTool::new(vec![("lazy".to_string(), String::new())]));
        let registry = ToolRegistry::with_lazy(vec![search_tool], vec![lazy.clone()]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done");
        assert_eq!(
            *lazy.calls.lock().unwrap(),
            1,
            "once unlocked, the lazy tool must actually execute"
        );
        assert_eq!(
            result.trace,
            vec![
                ToolInvocation {
                    name: SEARCH_TOOLS_NAME.to_string(),
                    outcome: ToolOutcome::Executed { ok: true }
                },
                ToolInvocation {
                    name: "lazy".to_string(),
                    outcome: ToolOutcome::Executed { ok: true }
                },
            ]
        );

        // The second iteration's own tool list must now include the
        // unlocked tool's full definition.
        let received_tools = provider.received_tools.lock().unwrap();
        let second_iteration_names: Vec<&str> =
            received_tools[1].iter().map(|d| d.name.as_str()).collect();
        assert!(second_iteration_names.contains(&"lazy"));
    }

    #[tokio::test]
    async fn a_notion_shaped_fetch_tool_is_discoverable_by_a_query_matching_either_name_or_description(
    ) {
        // Grounded in a real user report: connecting Notion's own
        // hosted MCP server and asking to fetch a page's content, the
        // model never searched for (and so never called) Notion's own
        // fetch tool. This proves the *mechanism* -- a reasonably-
        // chosen query matches the real tool by name alone, even if
        // its description were empty (`McpTool::definition()`'s own
        // `unwrap_or_default()` for a server that omits one) -- is not
        // where the problem is; it can't prove why the model chose not
        // to search in the first place, which is live model behavior,
        // not something this test controls.
        let notion_fetch = Arc::new(CountingTool::new("notion-fetch", PermissionTier::Auto));
        let notion_search = Arc::new(CountingTool::new("notion-search", PermissionTier::Auto));
        let search_tool = Arc::new(SearchToolsTool::new(vec![
            ("notion-fetch".to_string(), String::new()), // empty description, worst case
            ("notion-search".to_string(), "Searches across pages by query".to_string()),
        ]));
        let registry =
            ToolRegistry::with_lazy(vec![search_tool], vec![notion_fetch.clone(), notion_search]);

        for query in ["notion", "fetch", "NOTION"] {
            let matches = registry.lazy_tools_matching(query);
            assert!(
                matches.iter().any(|tool| tool.definition().name == "notion-fetch"),
                "query {query:?} should have found notion-fetch among {:?}",
                matches.iter().map(|t| t.definition().name).collect::<Vec<_>>()
            );
        }

        // End-to-end: a model that *does* search for "notion" can then
        // call notion-fetch directly, same turn.
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: SEARCH_TOOLS_NAME.to_string(),
                arguments: json!({"query": "notion"}),
            }],
            vec![ToolCallStreamItem::ToolCall {
                id: "call_1".to_string(),
                name: "notion-fetch".to_string(),
                arguments: json!({"url": "https://app.notion.com/..."}),
            }],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);

        AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("print this notion page's content")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(
            *notion_fetch.calls.lock().unwrap(),
            1,
            "notion-fetch must actually execute once unlocked"
        );
    }

    #[tokio::test]
    async fn a_search_with_no_matches_reports_so_without_unlocking_anything() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: SEARCH_TOOLS_NAME.to_string(),
                arguments: json!({"query": "no such thing"}),
            }],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let lazy = Arc::new(CountingTool::new("lazy", PermissionTier::Auto));
        let search_tool = Arc::new(SearchToolsTool::new(vec![("lazy".to_string(), String::new())]));
        let registry = ToolRegistry::with_lazy(vec![search_tool], vec![lazy]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(
            result.trace,
            vec![ToolInvocation {
                name: SEARCH_TOOLS_NAME.to_string(),
                outcome: ToolOutcome::Executed { ok: true }
            }]
        );
        let received = provider.received.lock().unwrap();
        let tool_result = received[1]
            .iter()
            .find(|m| m.role == crate::message::Role::Tool)
            .expect("the search call's own tool result");
        assert!(tool_result.content.contains("No not-yet-loaded tools matched"));
    }

    #[tokio::test]
    async fn a_delegated_sub_task_starts_with_its_own_fresh_unlocked_set() {
        // The child's own run() call gets a brand-new `unlocked`, never
        // inherited from the parent -- proven indirectly: the child
        // hallucinating a direct call to a lazy tool it never searched
        // for (even though the *parent* already unlocked that same
        // tool name earlier in the test) must still be rejected.
        let provider = ScriptedProvider::new(vec![
            // Parent searches and unlocks "lazy" for itself.
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: SEARCH_TOOLS_NAME.to_string(),
                arguments: json!({"query": "lazy"}),
            }],
            // Parent delegates.
            vec![ToolCallStreamItem::ToolCall {
                id: "call_1".to_string(),
                name: DELEGATE_TOOL_NAME.to_string(),
                arguments: json!({"task": "try the lazy tool directly"}),
            }],
            // Child tries the lazy tool directly, without searching.
            vec![ToolCallStreamItem::ToolCall {
                id: "call_2".to_string(),
                name: "lazy".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("child done".to_string())],
            vec![ToolCallStreamItem::TextDelta("parent done".to_string())],
        ]);
        let lazy = Arc::new(CountingTool::new("lazy", PermissionTier::Auto));
        let search_tool = Arc::new(SearchToolsTool::new(vec![("lazy".to_string(), String::new())]));
        let registry =
            ToolRegistry::with_lazy(vec![search_tool, Arc::new(DelegateTool)], vec![lazy.clone()]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "parent done");
        assert_eq!(
            *lazy.calls.lock().unwrap(),
            0,
            "the child must never reach the lazy tool's own execute()"
        );
    }

    // -- execution-log-and-context: ToolOutcome/ToolInvocation/AgentOutcome --

    #[test]
    fn every_tool_outcome_round_trips_through_construction_and_equality() {
        let outcomes = [
            ToolOutcome::Executed { ok: true },
            ToolOutcome::Executed { ok: false },
            ToolOutcome::DeniedByPolicy,
            ToolOutcome::DeclinedByUser,
            ToolOutcome::Rejected { arguments_preview: "{}".to_string() },
        ];
        for outcome in outcomes {
            let invocation = ToolInvocation { name: "alpha".to_string(), outcome: outcome.clone() };
            assert_eq!(invocation.outcome, outcome);
            assert_eq!(invocation.clone(), invocation);
        }
    }

    #[test]
    fn an_agent_outcome_round_trips() {
        let outcome = AgentOutcome {
            text: "done".to_string(),
            trace: vec![ToolInvocation {
                name: "alpha".to_string(),
                outcome: ToolOutcome::Executed { ok: true },
            }],
        };
        assert_eq!(outcome.clone(), outcome);
    }

    #[test]
    fn arguments_within_the_cap_are_kept_verbatim() {
        let args = json!({"path": "a.txt"});
        assert_eq!(capped_arguments_preview(&args), r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn arguments_exactly_at_the_cap_are_not_marked_truncated() {
        // Build a JSON string of exactly MAX_REJECTED_ARGS_BYTES bytes.
        let filler = "x".repeat(MAX_REJECTED_ARGS_BYTES - 2); // minus the quotes
        let args = Value::String(filler);
        let preview = capped_arguments_preview(&args);
        assert_eq!(preview.len(), MAX_REJECTED_ARGS_BYTES);
        assert!(!preview.contains("truncated"));
    }

    #[test]
    fn arguments_over_the_cap_are_truncated_with_a_visible_marker() {
        let filler = "x".repeat(MAX_REJECTED_ARGS_BYTES * 3);
        let args = Value::String(filler);
        let preview = capped_arguments_preview(&args);
        assert!(preview.starts_with("\"xxxx"));
        assert!(preview.ends_with(REJECTED_ARGS_TRUNCATION_MARKER));
        assert_eq!(preview.len(), MAX_REJECTED_ARGS_BYTES + REJECTED_ARGS_TRUNCATION_MARKER.len());
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character_in_the_arguments_preview() {
        // "雪" is 3 bytes in UTF-8; cutting at a raw byte count with no
        // regard for character boundaries would, for most lengths, land
        // mid-character. This only exercises the function's own
        // boundary-walking, not a specific cap-size/char-width
        // coincidence, so it doesn't matter whether `MAX_REJECTED_ARGS_BYTES`
        // happens to be a multiple of 3.
        let filler = "雪".repeat(MAX_REJECTED_ARGS_BYTES);
        let args = Value::String(filler);
        let preview = capped_arguments_preview(&args);
        assert!(preview.ends_with(REJECTED_ARGS_TRUNCATION_MARKER));
        let kept = preview.strip_suffix(REJECTED_ARGS_TRUNCATION_MARKER).unwrap();
        assert!(kept.is_char_boundary(kept.len()), "the kept prefix must end on a char boundary");
        assert!(!kept.contains('\u{FFFD}'), "no replacement characters from a split boundary");
        assert!(kept.len() <= MAX_REJECTED_ARGS_BYTES);
    }

    /// Approves every call it's asked about -- the default for tests
    /// that aren't exercising confirmation behavior itself.
    struct AlwaysApprove;

    #[async_trait::async_trait]
    impl PermissionDecider for AlwaysApprove {
        async fn decide(&self, calls: &[PendingToolCall]) -> HashSet<String> {
            calls.iter().map(|call| call.id.clone()).collect()
        }
    }

    /// Denies every call -- used to prove a denied `Confirm` call never
    /// executes.
    struct AlwaysDeny;

    #[async_trait::async_trait]
    impl PermissionDecider for AlwaysDeny {
        async fn decide(&self, _calls: &[PendingToolCall]) -> HashSet<String> { HashSet::new() }
    }

    /// Records every request it's asked to connect, and returns the
    /// same canned `outcome` each time -- a scripted `McpConnector`
    /// double, same purpose as `ScriptedProvider`.
    struct RecordingMcpConnector {
        requests: Mutex<Vec<McpConnectRequest>>,
        outcome: McpConnectOutcome,
    }

    #[async_trait::async_trait]
    impl McpConnector for RecordingMcpConnector {
        async fn connect(&self, request: McpConnectRequest) -> McpConnectOutcome {
            self.requests.lock().unwrap().push(request);
            self.outcome.clone()
        }
    }

    /// A tool that records how many times it actually ran, so tests can
    /// assert a denied/unpermitted call never reached `execute`.
    struct CountingTool {
        name: &'static str,
        tier: PermissionTier,
        calls: Mutex<u32>,
    }

    impl CountingTool {
        fn new(name: &'static str, tier: PermissionTier) -> Self {
            Self { name, tier, calls: Mutex::new(0) }
        }
    }

    #[async_trait::async_trait]
    impl Tool for CountingTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name.to_string(),
                description: String::new(),
                parameters: json!({"type": "object", "properties": {}}),
            }
        }

        fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
            self.tier
        }

        async fn execute(
            &self,
            _args: Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, crate::agent_tool::ToolError> {
            *self.calls.lock().unwrap() += 1;
            Ok(ToolResult::ok(format!("{} ran", self.name)))
        }
    }

    /// A `ToolCallingProvider` double whose `chat_with_tools` yields the
    /// next scripted response on each call -- one entry per expected
    /// model round-trip.
    struct ScriptedProvider {
        responses: Mutex<std::collections::VecDeque<Vec<ToolCallStreamItem>>>,
        /// The message list handed to each `chat_with_tools` call, in
        /// order -- what the model actually receives on each round trip.
        received: Mutex<Vec<Vec<Message>>>,
        /// The tool *definitions* offered on each call, in order --
        /// what the model could actually see/call that iteration; used
        /// by the lazy-loading tests to confirm a lazy tool's full
        /// schema isn't sent until it's been searched for.
        received_tools: Mutex<Vec<Vec<ToolDefinition>>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<Vec<ToolCallStreamItem>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                received: Mutex::new(Vec::new()),
                received_tools: Mutex::new(Vec::new()),
            }
        }

        /// Never runs out -- every call gets the same tool-call
        /// response, for testing that the loop terminates via the
        /// iteration limit rather than the script running dry.
        fn repeating(response: Vec<ToolCallStreamItem>) -> Self {
            let mut queue = std::collections::VecDeque::new();
            for _ in 0..100 {
                queue.push_back(response.clone());
            }
            Self {
                responses: Mutex::new(queue),
                received: Mutex::new(Vec::new()),
                received_tools: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl AiProvider for ScriptedProvider {
        fn kind(&self) -> ProviderKind { ProviderKind::Ollama }

        async fn chat(&self, _messages: Vec<Message>) -> Result<ChatStream, ProviderError> {
            unimplemented!("ScriptedProvider only exercises chat_with_tools")
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> { Ok(vec![]) }
    }

    #[async_trait::async_trait]
    impl ToolCallingProvider for ScriptedProvider {
        async fn chat_with_tools(
            &self,
            messages: Vec<Message>,
            tools: Vec<ToolDefinition>,
        ) -> Result<ToolCallStream, ProviderError> {
            self.received.lock().unwrap().push(messages);
            self.received_tools.lock().unwrap().push(tools);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("ScriptedProvider called more times than scripted");
            let stream: BoxStream<'static, Result<ToolCallStreamItem, ProviderError>> =
                Box::pin(iter(response.into_iter().map(Ok)));
            Ok(stream)
        }
    }

    #[tokio::test]
    async fn a_multi_tool_call_turn_resolves_correctly() {
        let provider = ScriptedProvider::new(vec![
            vec![
                ToolCallStreamItem::ToolCall {
                    id: "call_0".to_string(),
                    name: "alpha".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_1".to_string(),
                    name: "beta".to_string(),
                    arguments: json!({}),
                },
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Auto));
        let beta = Arc::new(CountingTool::new("beta", PermissionTier::Auto));
        let registry = ToolRegistry::new(vec![alpha.clone(), beta.clone()]);
        let runtime = AemeathAgentRuntime::default();

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done");
        assert_eq!(
            result.trace,
            vec![
                ToolInvocation {
                    name: "alpha".to_string(),
                    outcome: ToolOutcome::Executed { ok: true }
                },
                ToolInvocation {
                    name: "beta".to_string(),
                    outcome: ToolOutcome::Executed { ok: true }
                },
            ],
            "the trace accumulates across iterations, not just the final text-only one"
        );
        assert_eq!(*alpha.calls.lock().unwrap(), 1);
        assert_eq!(*beta.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn every_tool_result_carries_the_id_of_the_call_it_answers() {
        // One turn exercising every result path: executed, unknown tool,
        // malformed arguments, denied by tier, and denied at confirmation.
        let call = |id: &str, name: &str, arguments: Value| ToolCallStreamItem::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
        };
        let provider = ScriptedProvider::new(vec![
            vec![
                call("c_ok", "alpha", json!({})),
                call("c_unknown", "no_such_tool", json!({})),
                call("c_badargs", "alpha", json!("not an object")),
                call("c_denied", "gamma", json!({})),
                call("c_confirm_no", "delta", json!({})),
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![
            Arc::new(CountingTool::new("alpha", PermissionTier::Auto)),
            Arc::new(CountingTool::new("gamma", PermissionTier::Deny)),
            Arc::new(CountingTool::new("delta", PermissionTier::Confirm)),
        ]);

        AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysDeny,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        let received = provider.received.lock().unwrap();
        let second_turn = &received[1];
        let results: Vec<(&str, &str)> = second_turn
            .iter()
            .filter(|m| m.role == crate::message::Role::Tool)
            .map(|m| {
                (
                    m.tool_call_id.as_deref().expect("every tool result has an id"),
                    m.content.as_str(),
                )
            })
            .collect();
        let ids: Vec<&str> = results.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, ["c_ok", "c_unknown", "c_badargs", "c_denied", "c_confirm_no"]);
        assert!(results[0].1.contains("alpha ran"), "the executed call's own output");
        assert!(results[1].1.contains("unknown tool"));
    }

    #[tokio::test]
    async fn all_five_tool_outcomes_in_one_turn_produce_a_trace_in_call_order() {
        let provider = ScriptedProvider::new(vec![
            vec![
                ToolCallStreamItem::ToolCall {
                    id: "call_0".to_string(),
                    name: "does_not_exist".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_1".to_string(),
                    name: "needs_an_object".to_string(),
                    arguments: json!("not an object"),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_2".to_string(),
                    name: "alpha".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_3".to_string(),
                    name: "gamma".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_4".to_string(),
                    name: "delta".to_string(),
                    arguments: json!({}),
                },
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![
            Arc::new(CountingTool::new("needs_an_object", PermissionTier::Auto)),
            Arc::new(CountingTool::new("alpha", PermissionTier::Auto)),
            Arc::new(CountingTool::new("gamma", PermissionTier::Deny)),
            Arc::new(CountingTool::new("delta", PermissionTier::Confirm)),
        ]);
        let runtime = AemeathAgentRuntime::default();

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysDeny,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done");
        let names: Vec<&str> = result.trace.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["does_not_exist", "needs_an_object", "alpha", "gamma", "delta"]);
        assert!(matches!(result.trace[0].outcome, ToolOutcome::Rejected { .. }));
        assert!(matches!(result.trace[1].outcome, ToolOutcome::Rejected { .. }));
        assert_eq!(result.trace[2].outcome, ToolOutcome::Executed { ok: true });
        assert_eq!(result.trace[3].outcome, ToolOutcome::DeniedByPolicy);
        assert_eq!(result.trace[4].outcome, ToolOutcome::DeclinedByUser);
    }

    /// Blocks until `gate` is signaled before returning -- lets a test
    /// force one `Auto`-tier call to finish strictly *after* another,
    /// deterministically, with no reliance on real-time sleeps that
    /// could flake under load.
    struct WaitsForGate {
        name: &'static str,
        gate: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl Tool for WaitsForGate {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name.to_string(),
                description: String::new(),
                parameters: json!({"type": "object", "properties": {}}),
            }
        }

        fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
            PermissionTier::Auto
        }

        async fn execute(
            &self,
            _args: Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, crate::agent_tool::ToolError> {
            self.gate.notified().await;
            Ok(ToolResult::ok(format!("{} ran", self.name)))
        }
    }

    /// The counterpart to `WaitsForGate`: signals `gate` and returns
    /// immediately, with no `.await` point of its own before doing so --
    /// guarantees this call's own future resolves on its very first
    /// poll, strictly before whatever is waiting on the same gate can.
    struct SignalsGate {
        name: &'static str,
        gate: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl Tool for SignalsGate {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name.to_string(),
                description: String::new(),
                parameters: json!({"type": "object", "properties": {}}),
            }
        }

        fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
            PermissionTier::Auto
        }

        async fn execute(
            &self,
            _args: Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, crate::agent_tool::ToolError> {
            self.gate.notify_one();
            Ok(ToolResult::ok(format!("{} ran", self.name)))
        }
    }

    #[tokio::test]
    async fn concurrent_auto_calls_are_written_back_in_request_order_not_completion_order() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let provider = ScriptedProvider::new(vec![
            vec![
                ToolCallStreamItem::ToolCall {
                    id: "call_0".to_string(),
                    name: "slow".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_1".to_string(),
                    name: "fast".to_string(),
                    arguments: json!({}),
                },
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        // "slow" is requested first but can only finish once "fast"
        // signals the gate -- "fast" (requested second) is therefore
        // guaranteed to complete first, regardless of scheduling.
        let registry = ToolRegistry::new(vec![
            Arc::new(WaitsForGate { name: "slow", gate: gate.clone() }),
            Arc::new(SignalsGate { name: "fast", gate: gate.clone() }),
        ]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(
            result.trace,
            vec![
                ToolInvocation {
                    name: "slow".to_string(),
                    outcome: ToolOutcome::Executed { ok: true }
                },
                ToolInvocation {
                    name: "fast".to_string(),
                    outcome: ToolOutcome::Executed { ok: true }
                },
            ],
            "written back in request order even though \"fast\" completed first"
        );
    }

    #[tokio::test]
    async fn more_auto_calls_than_the_concurrency_cap_all_still_execute() {
        // MAX_CONCURRENT_TOOL_CALLS is 3; five calls in one turn must
        // all still run exactly once each, just throttled in how many
        // start at once -- none rejected or silently dropped.
        let provider = ScriptedProvider::new(vec![
            vec![
                ToolCallStreamItem::ToolCall {
                    id: "call_0".to_string(),
                    name: "t0".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_1".to_string(),
                    name: "t1".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_2".to_string(),
                    name: "t2".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_3".to_string(),
                    name: "t3".to_string(),
                    arguments: json!({}),
                },
                ToolCallStreamItem::ToolCall {
                    id: "call_4".to_string(),
                    name: "t4".to_string(),
                    arguments: json!({}),
                },
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let t0 = Arc::new(CountingTool::new("t0", PermissionTier::Auto));
        let t1 = Arc::new(CountingTool::new("t1", PermissionTier::Auto));
        let t2 = Arc::new(CountingTool::new("t2", PermissionTier::Auto));
        let t3 = Arc::new(CountingTool::new("t3", PermissionTier::Auto));
        let t4 = Arc::new(CountingTool::new("t4", PermissionTier::Auto));
        let registry =
            ToolRegistry::new(vec![t0.clone(), t1.clone(), t2.clone(), t3.clone(), t4.clone()]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        let names: Vec<&str> = result.trace.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["t0", "t1", "t2", "t3", "t4"]);
        assert!(result.trace.iter().all(|i| i.outcome == ToolOutcome::Executed { ok: true }));
        for tool in [&t0, &t1, &t2, &t3, &t4] {
            assert_eq!(*tool.calls.lock().unwrap(), 1, "{} must run exactly once", tool.name);
        }
    }

    /// Proves `delegate_task` never requires confirmation structurally,
    /// not merely that `DelegateTool::required_permission` happens to
    /// return `Auto` -- the classification pass never even reaches
    /// that method for a real `delegate_task` call (it returns
    /// `CallPlan::Delegate`/`MalformedArguments` directly, by name,
    /// before any tier is consulted). If that guarantee ever regressed
    /// and delegation started flowing through the `Confirm` tier,
    /// this decider makes the test fail loudly instead of silently
    /// passing with an unexercised assumption.
    struct PanicsIfAskedToDecide;

    #[async_trait::async_trait]
    impl PermissionDecider for PanicsIfAskedToDecide {
        async fn decide(&self, _calls: &[PendingToolCall]) -> HashSet<String> {
            panic!("delegate_task must never require confirmation");
        }
    }

    #[tokio::test]
    async fn delegate_task_never_triggers_a_confirmation_decision() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: DELEGATE_TOOL_NAME.to_string(),
                arguments: json!({"task": "look up X"}),
            }],
            vec![ToolCallStreamItem::TextDelta("child's answer".to_string())],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![Arc::new(DelegateTool)]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &PanicsIfAskedToDecide,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done", "completing without panicking is the test itself");
    }

    #[tokio::test]
    async fn a_delegated_sub_tasks_result_becomes_the_calls_own_tool_result() {
        let provider = ScriptedProvider::new(vec![
            // Parent's 1st turn: delegates.
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: DELEGATE_TOOL_NAME.to_string(),
                arguments: json!({"task": "look up X"}),
            }],
            // Child's own (only) turn: plain text, no tool calls.
            vec![ToolCallStreamItem::TextDelta("child's answer".to_string())],
            // Parent's 2nd turn, after the delegation result comes back.
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![Arc::new(DelegateTool)]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done");
        assert_eq!(
            result.trace,
            vec![ToolInvocation {
                name: DELEGATE_TOOL_NAME.to_string(),
                outcome: ToolOutcome::Executed { ok: true }
            }]
        );

        let received = provider.received.lock().unwrap();
        assert_eq!(received.len(), 3, "parent's 1st turn, the child's turn, parent's 2nd turn");
        let parents_second_turn = &received[2];
        let tool_result = parents_second_turn
            .iter()
            .find(|m| m.role == crate::message::Role::Tool)
            .expect("the delegation call's own tool result");
        assert_eq!(tool_result.content, "child's answer");
    }

    #[tokio::test]
    async fn a_delegated_sub_task_cannot_itself_delegate() {
        let provider = ScriptedProvider::new(vec![
            // Parent delegates.
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: DELEGATE_TOOL_NAME.to_string(),
                arguments: json!({"task": "nested"}),
            }],
            // Child hallucinates delegate_task anyway -- its own
            // registry (built via `.without(DELEGATE_TOOL_NAME)`)
            // does not have it.
            vec![ToolCallStreamItem::ToolCall {
                id: "call_1".to_string(),
                name: DELEGATE_TOOL_NAME.to_string(),
                arguments: json!({"task": "grandchild"}),
            }],
            // Child's 2nd turn, after its own delegation attempt was rejected.
            vec![ToolCallStreamItem::TextDelta(
                "child done despite trying to delegate".to_string(),
            )],
            // Parent's 2nd turn.
            vec![ToolCallStreamItem::TextDelta("parent done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![Arc::new(DelegateTool)]);

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        // From the parent's point of view, delegation still succeeded
        // overall -- the child simply couldn't recurse, the same way
        // any sub-task that hits a dead end can still report back.
        assert_eq!(result.text, "parent done");
        assert_eq!(
            result.trace,
            vec![ToolInvocation {
                name: DELEGATE_TOOL_NAME.to_string(),
                outcome: ToolOutcome::Executed { ok: true }
            }]
        );

        let received = provider.received.lock().unwrap();
        let childs_second_turn = &received[2];
        let rejection = childs_second_turn
            .iter()
            .find(|m| m.role == crate::message::Role::Tool)
            .expect("the child's own delegation attempt got a tool result");
        assert!(
            rejection.content.contains("unknown tool"),
            "rejected the same way a call to any other unknown tool is: {}",
            rejection.content
        );
    }

    #[tokio::test]
    async fn a_delegated_sub_task_cannot_call_connect_mcp_server() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: DELEGATE_TOOL_NAME.to_string(),
                arguments: json!({"task": "try to connect"}),
            }],
            // Child hallucinates connect_mcp_server anyway -- its own
            // registry (built via `.without(CONNECT_MCP_SERVER_TOOL_NAME)`
            // too) does not have it.
            vec![ToolCallStreamItem::ToolCall {
                id: "call_1".to_string(),
                name: CONNECT_MCP_SERVER_TOOL_NAME.to_string(),
                arguments: json!({"transport": "http", "url": "https://example.com"}),
            }],
            vec![ToolCallStreamItem::TextDelta("child done despite trying to connect".to_string())],
            vec![ToolCallStreamItem::TextDelta("parent done".to_string())],
        ]);
        let registry =
            ToolRegistry::new(vec![Arc::new(DelegateTool), Arc::new(ConnectMcpServerTool)]);
        let connector = RecordingMcpConnector {
            requests: Mutex::new(Vec::new()),
            outcome: McpConnectOutcome::Declined,
        };

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &connector,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "parent done");
        assert!(
            connector.requests.lock().unwrap().is_empty(),
            "the outer (real) connector must never be reached by a delegated child"
        );

        let received = provider.received.lock().unwrap();
        let childs_second_turn = &received[2];
        let rejection = childs_second_turn
            .iter()
            .find(|m| m.role == crate::message::Role::Tool)
            .expect("the child's own connect attempt got a tool result");
        assert!(
            rejection.content.contains("unknown tool"),
            "rejected the same way a call to any other unknown tool is: {}",
            rejection.content
        );
    }

    #[tokio::test]
    async fn a_delegated_sub_tasks_narration_never_reaches_the_parents_callback() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: DELEGATE_TOOL_NAME.to_string(),
                arguments: json!({"task": "look up X"}),
            }],
            vec![ToolCallStreamItem::TextDelta(
                "text only the child's own on_text_delta should ever see".to_string(),
            )],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![Arc::new(DelegateTool)]);
        let streamed = Arc::new(Mutex::new(String::new()));
        let streamed_in_callback = streamed.clone();

        let result = AemeathAgentRuntime::default()
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |delta: &str| streamed_in_callback.lock().unwrap().push_str(delta),
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done");
        assert_eq!(
            *streamed.lock().unwrap(),
            "done",
            "only the parent's own narration reaches the parent's callback -- the child's is \
             dropped by the no-op closure the delegation branch passes instead"
        );
    }

    #[tokio::test]
    async fn a_delegated_sub_task_that_exhausts_its_own_iterations_reports_failure_to_the_parent() {
        // The child reuses `self.max_iterations` -- the same runtime
        // instance recursing -- so to let the *parent* reach a second
        // turn while the *child* exhausts its own budget, both need at
        // least 2 iterations: the child's own two turns must each keep
        // requesting a tool call (never a plain final answer), so its
        // own `run()` falls through to `MaxIterationsReached` rather
        // than returning `Ok` early.
        let provider = ScriptedProvider::new(vec![
            // Parent's 1st turn: delegates.
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: DELEGATE_TOOL_NAME.to_string(),
                arguments: json!({"task": "never finishes"}),
            }],
            // Child's 1st (of 2 allowed) turns: requests a tool call.
            vec![ToolCallStreamItem::ToolCall {
                id: "child_call_0".to_string(),
                name: "no_such_tool".to_string(),
                arguments: json!({}),
            }],
            // Child's 2nd (and final) turn: still requesting a tool
            // call, never a plain answer, so it exhausts its own
            // max_iterations without ever reaching the early `Ok` return.
            vec![ToolCallStreamItem::ToolCall {
                id: "child_call_1".to_string(),
                name: "no_such_tool".to_string(),
                arguments: json!({}),
            }],
            // Parent's 2nd turn, after the delegation call reports its
            // own child's failure back as an ordinary tool result.
            vec![ToolCallStreamItem::TextDelta("parent continues anyway".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![Arc::new(DelegateTool)]);
        let runtime = AemeathAgentRuntime { max_iterations: 2 };

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "parent continues anyway");
        assert_eq!(
            result.trace,
            vec![ToolInvocation {
                name: DELEGATE_TOOL_NAME.to_string(),
                outcome: ToolOutcome::Executed { ok: false }
            }],
            "the child's own MaxIterationsReached is reported as this call's own failure, not \
             propagated to the parent's own Result"
        );
    }

    #[tokio::test]
    async fn iteration_stops_at_the_limit_without_hanging() {
        let provider = ScriptedProvider::repeating(vec![ToolCallStreamItem::ToolCall {
            id: "call_0".to_string(),
            name: "alpha".to_string(),
            arguments: json!({}),
        }]);
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Auto));
        let registry = ToolRegistry::new(vec![alpha.clone()]);
        let runtime = AemeathAgentRuntime { max_iterations: 3 };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            runtime.run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            ),
        )
        .await
        .expect("the loop must terminate on its own well within the timeout");

        assert!(matches!(result, Err(AgentError::MaxIterationsReached { .. })));
        assert_eq!(*alpha.calls.lock().unwrap(), 3, "each of the 3 iterations should have run it");
    }

    #[tokio::test]
    async fn a_denied_tier_tool_call_never_executes() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "alpha".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("ok".to_string())],
        ]);
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Deny));
        let registry = ToolRegistry::new(vec![alpha.clone()]);
        let runtime = AemeathAgentRuntime::default();

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();

        assert_eq!(result.text, "ok");
        assert_eq!(
            result.trace,
            vec![ToolInvocation {
                name: "alpha".to_string(),
                outcome: ToolOutcome::DeniedByPolicy
            }]
        );
        assert_eq!(*alpha.calls.lock().unwrap(), 0, "a Deny-tier call must never reach execute()");
    }

    #[tokio::test]
    async fn a_confirm_tier_call_defers_to_the_permission_decider() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "alpha".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("ok".to_string())],
        ]);
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Confirm));
        let registry = ToolRegistry::new(vec![alpha.clone()]);
        let runtime = AemeathAgentRuntime::default();

        runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysDeny,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();
        assert_eq!(*alpha.calls.lock().unwrap(), 0, "AlwaysDeny must block a Confirm-tier call");

        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "alpha".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("ok".to_string())],
        ]);
        runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();
        assert_eq!(
            *alpha.calls.lock().unwrap(),
            1,
            "AlwaysApprove must let a Confirm-tier call run"
        );
    }

    #[tokio::test]
    async fn an_unknown_tool_call_is_reported_without_crashing() {
        let provider = ScriptedProvider::new(vec![
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "does_not_exist".to_string(),
                arguments: json!({}),
            }],
            vec![ToolCallStreamItem::TextDelta("ok".to_string())],
        ]);
        let registry = ToolRegistry::new(vec![]);
        let runtime = AemeathAgentRuntime::default();

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |_: &str| {},
            )
            .await
            .unwrap();
        assert_eq!(result.text, "ok");
        assert!(matches!(
            result.trace.as_slice(),
            [ToolInvocation { name, outcome: ToolOutcome::Rejected { .. } }] if name == "does_not_exist"
        ));
    }

    #[tokio::test]
    async fn text_deltas_stream_live_from_every_iteration_not_just_the_final_one() {
        // A model that narrates before calling a tool, then continues
        // after the tool result comes back, should have *both* pieces
        // of text streamed live -- not just the final iteration's.
        let provider = ScriptedProvider::new(vec![
            vec![
                ToolCallStreamItem::TextDelta("checking".to_string()),
                ToolCallStreamItem::ToolCall {
                    id: "call_0".to_string(),
                    name: "alpha".to_string(),
                    arguments: json!({}),
                },
            ],
            vec![ToolCallStreamItem::TextDelta("done".to_string())],
        ]);
        let alpha = Arc::new(CountingTool::new("alpha", PermissionTier::Auto));
        let registry = ToolRegistry::new(vec![alpha.clone()]);
        let runtime = AemeathAgentRuntime::default();
        let streamed = Arc::new(Mutex::new(String::new()));
        let streamed_in_callback = streamed.clone();

        let result = runtime
            .run(
                &provider,
                vec![Message::user("hi")],
                &registry,
                &ctx(),
                &AlwaysApprove,
                &NeverConnectMcp,
                &mut |delta: &str| streamed_in_callback.lock().unwrap().push_str(delta),
            )
            .await
            .unwrap();

        assert_eq!(result.text, "done");
        assert_eq!(
            result.trace,
            vec![ToolInvocation {
                name: "alpha".to_string(),
                outcome: ToolOutcome::Executed { ok: true }
            }]
        );
        assert_eq!(*streamed.lock().unwrap(), "checkingdone");
    }
}
