## Why

Aemeath's native tool set is fixed and hand-coded (`read_file`, `list_directory`, `web_search`, `run_command`, `get_system_context`, plus `delegate_task`) — there is no way to extend what the agent loop can reach beyond editing this app's own Rust source. MCP (Model Context Protocol) is a standard client/server protocol for exactly this: discovering and calling tools exposed by a separate process or remote endpoint. Originally planned (roadmap §13 Stage 6, under the now-retired name "MCP + Context Awareness" — Context Awareness was moved out to Backlog §19 in a separate, unrelated exploration) as a thin bridge, with the user's own concrete motivating case: being able to say in plain chat "connect to the GitHub MCP server" and have it just work, not only configure servers ahead of time through a settings screen.

This proposal covers only the _consuming_ direction — Aemeath (and, by extension, any future caller of the same `ToolRegistry`) connecting out to third-party MCP servers. The _reverse_ direction (Aemeath exposing its own capability as an MCP server for Claude Code/Codex/OpenClaw to call into — planning doc §8/§11's "Aemeath MCP" server role, floated during exploration as e.g. letting Claude Code hand a quick sub-task to the local Ollama model) is deliberately out of scope: it is a completely separate protocol role with no code anywhere in this codebase today, and — unlike the consuming direction — has no concrete consumer asking for it yet, consistent with this project's own repeated "don't build ahead of a real consumer" principle (`TaskRequirements`, `ContextManager`, `DelegationManager` all deferred the same way).

## What Changes

- A new `connect_mcp_server` capability, special-cased in dispatch _inside_ `AgentRuntime::run()`'s own per-iteration loop, the same way `delegate_task` is — the model decides to call it mid-turn, so there's no point before `run()` starts to intercept it at. Unlike `delegate_task`, it needs `AppHandle`-level access (opening a browser, writing settings, showing the confirmation popup) that neither `Tool::execute()`'s signature nor any of `run()`'s existing parameters carry, and `agent_runtime.rs` has no `tauri` dependency to add one with — resolved by a new `McpConnector` trait, defined abstractly in `fleet-snowfluff-ai` and implemented with a real `AppHandle` in the app crate, passed into `run()` the same way `permission: &dyn PermissionDecider` already is; see design.md's own Decision for the full shape and the rejected alternative.
- No curated/known-server allowlist: the model resolves a candidate server (by exact command/URL if the user gave one, or from its own knowledge if asked more vaguely) from a plain-language chat request, same as a local custom service the user already knows the exact invocation for.
- A `Confirm`-tier popup shows exactly what would be connected (command + args, or URL) before anything happens — the same discipline `run_command` already uses, not a new, weaker bar.
- A parallel Settings-UI entry point (AI tab, new section) to add a server by filling in the same fields directly, for cases where a form is more precise than extracting a command string from a sentence.
- Both transports in v1: stdio (local subprocess) and HTTP/SSE (remote), since OAuth is meaningless for the former and the MCP ecosystem's simplest, most common servers (filesystem, git, local dev tools) are predominantly the former.
- Credential handling splits by transport, per the MCP spec's own stance (HTTP-transport authorization is OAuth-shaped; stdio explicitly "should not" follow that spec and instead takes credentials from the environment): an HTTP server needing OAuth gets a short-lived, on-demand local redirect listener (PKCE, no client secret) plus a manual "paste the code" fallback; a stdio server needing a credential gets a plain text field for an environment-variable value. Both end up in the existing `secrets_store`.
- A connected MCP tool is just another `Tool` trait implementation, discovered once at connect time via the protocol's own fixed `initialize`/`tools/list` methods and built into the registry alongside the native tools — zero changes to `AemeathAgentRuntime::run()`'s own dispatch. Default permission tier for every tool a server exposes: `Confirm`, blanket per-server (no per-tool filtering in v1 — MCP carries no risk metadata of its own to filter on, and this is the only defensible default given that).
- A new app-lifetime `McpConnectionState` singleton holds live connections (spawned subprocess, or an HTTP client + token), looked up when a registry is built. Eager-connect once, right when a server is added (so there's something to validate and show); lazy reconnect after that (app restart, a connection that died mid-session) — never proactively health-checked.
- A newly connected server's tools become available starting the _next_ message, never live mid-turn — the registry is still built once per attempt and handed around as an immutable reference for that whole call, same reasoning `mix-mode-local-tools` already established for why `AgentRuntime::run()` itself is never touched mid-flight.
- Settings UI (AI tab) gains a list of connected servers (name, status `ready`/`pending`, remove), each with a collapsible tools browser (cached from connect time, with a manual refresh action) — reusing the existing `<details>`-per-entry pattern already used for provider profiles.
- Removing a server deletes its token/credential from `secrets_store` immediately and drops the config entry — deliberately _not_ mirroring `disable_profile`'s existing "leave the credential behind" precedent, since an OAuth grant is a live thing a remote service is actively honoring, not an inert string the user typed in themselves. No call to the provider's own revocation endpoint in v1.
- `connect_mcp_server` (and anything it connects) is never offered to `mix-mode-local-tools`' own narrow local attempt — same tier as `run_command`/`delegate_task`, which that registry already excludes.

## Capabilities

### New Capabilities

- `mcp`: connecting to, discovering tools from, and managing third-party MCP servers (both transports, both credential shapes, connection lifecycle, removal).

### Modified Capabilities

- `agent-runtime`: "Native tool registry" broadens to admit MCP-sourced tools riding the same `ToolRegistry`/permission-tier machinery as native tools.
- `settings-ui`: "AI tab" gains the connected-servers list, add-server form, and per-server tools browser.

## Impact

- `crates/fleet-snowfluff-ai/src/`: new `mcp` module — client connection (stdio spawn + HTTP/SSE), the `initialize`/`tools/list`/`tools/call` protocol calls, `McpTool: Tool` wrapping a discovered tool, OAuth discovery/PKCE flow, env-var credential handling.
- `crates/fleet-snowfluff/src/`: new `McpConnectionState` (Tauri-managed singleton), `mcp_servers` config persisted alongside `AiSettings`, `connect_mcp_server`'s special-cased dispatch (new code, sitting beside where `run_generation_with_tools` is called), new Tauri commands for Settings UI (list/add/remove/refresh-tools).
- `crates/fleet-snowfluff/src/secrets_store.rs`: gains a removal path (deleting one entry), not just `load`/`save`.
- `ui/src/main.ts`, `ui/src/style.css`: new connected-servers section in the AI tab, reusing the existing `.ai-profile-row`/`<details>` pattern.
- No changes to `AemeathAgentRuntime::run()`, `agent_runtime.rs`'s `CallPlan` dispatch, or `mix_local_tool_registry`.
