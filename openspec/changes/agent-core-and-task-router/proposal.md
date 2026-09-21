## Why

Fleet Snowfluff can chat (Stage 1) and pick which provider/subscription answers (Stage 2), but every reply is still a single, unconditional `provider.chat()` call: no routing decision, no tool calling, no permission model, and no way for the model to act on the local machine. Stage 3 of `docs/fleet-snowfluff-feature-planning.md` closes that gap by giving Fleet its own Agent Core — a Task Router that decides where a message goes, and an Agent Runtime that lets the chosen provider call a small set of native tools under an explicit permission model. This also absorbs the Task Router domain work originally scoped into Stage 2 but excluded from `subscription-first-chat` when it shipped (see that change's own Non-Goals) — the domain types were never built, so they land here instead of pretending Stage 2 finished them.

## What Changes

- Add `AiSettings.task_router_mode: TaskRouterMode` (`Single` default, or `Mix`). `Single` preserves today's behavior exactly (`default_profile` handles everything). `Mix` sends each message to the enabled Ollama profile first, with a maintained `personas/task-router-rules.md` file appended to its system prompt; the local model judges simple-vs-complex itself and signals "complex" by replying with the literal token `<<ESCALATE>>` and nothing else. The router detects this via incremental prefix-matching against the streamed reply (not full-length buffering), discards that attempt, and re-dispatches the same message to `default_profile` — the same fallback path also fires on infra failure (Ollama disabled, unavailable, or erroring).
- Add the Task Router domain types: `Task`, `TaskRequirements`, `RoutingContext`, `ExecutionRoute` (minimal shape: `runtime`/`provider`/`model`/`session_strategy`), `SessionStrategy`, and a `TaskRouter` trait. `TaskRequirements` is defined but not consumed by the `mode: mix` decision in this change — it's reserved for a future capability-escalation change.
- Add a new `ToolCallingProvider: AiProvider` trait (`chat_with_tools`) implemented **only by `Ollama`** in this change. `build_provider()` decides `PlainChat` vs `ToolCapable` at construction time. `OpenAiCompatible`/`Anthropic` tool-calling, and `Mock`/`ClaudeCodeCli`/`Codex` (which keep their own native tool loops), are explicitly out of scope.
- Add an `AgentRuntime` trait and Agent Loop that runs the tool-calling conversation loop against a `ToolCapable` provider, with schema validation, a permission check, and iteration handling.
- Add a `Tool` trait, `ToolContext`, `ToolResult`, and a Tool Registry with 4 native tools: `web_search` (SearXNG, falling back to DuckDuckGo Instant Answer — both free, keyless; scoped to the Ollama tool-calling path only), `read_file`/`list_directory` (auto inside a new user-configured `project_root` setting, escalate to a confirmation popup outside it), `run_command` (spawned with a 30s timeout, ~20KB output cap, working directory hard-pinned to `project_root`), and `get_system_context` (CPU/memory/uptime via a new `sysinfo` dependency, plus date/time and OS — explicitly not active-window/idle-time/clipboard, which stay a later stage's work).
- Add a permission model (`auto`/`confirm`/`deny`) enforced per tool call, with a new "tool-confirmation" popup window (same `WebviewWindowBuilder` pattern as the existing status bubble) that batches every `confirm`-tier tool call from one model turn into a single prompt, plus a session-scoped (in-memory only) "always allow" shortcut for low-risk tools.
- Replace `claude_code_cli.rs`'s blanket `DISALLOWED_TOOLS` constant with a per-profile, by-case `--allowedTools`/`--disallowedTools` list for Claude Code's own native tools, moving `WebSearch`/`WebFetch` into the auto-allowed default (Anthropic's own first-party, subscription-bundled search tool) alongside `Read`/`Glob`/`Grep`.
- Add a basic `ContextManager` (`build_context`/`record_execution` only — no compaction, memory retrieval, or sub-agent projection) that assembles context for the Agent Loop and records what each execution produced.

## Capabilities

### New Capabilities

- `task-router`: the routing domain (`Task`, `TaskRequirements`, `RoutingContext`, `ExecutionRoute`, `SessionStrategy`, `TaskRouter`) and the `single`/`mix` mode behavior, including the local-model-first classification and fallback rules.
- `agent-runtime`: the `AgentRuntime`/`Tool` traits, the Tool Registry and its 4 native tools, the permission model and confirmation-popup behavior, and the basic `ContextManager`.

### Modified Capabilities

- `ai-provider`: adds the `ToolCallingProvider` capability (Ollama-only) as an extension of "Provider abstraction," and adds a requirement for external-CLI native-tool access control (the Claude Code/Codex by-case allow-list, replacing the current blanket deny).
- `settings-ui`: the AI tab gains the `task_router_mode` control and a `project_root` folder picker.

## Impact

- `crates/fleet-snowfluff-ai/src/provider.rs`: new `ToolCallingProvider` trait, `ToolCallStream`.
- `crates/fleet-snowfluff-ai/src/providers/ollama.rs`: implements `ToolCallingProvider`.
- `crates/fleet-snowfluff-ai/src/providers/claude_code_cli.rs`: replaces `DISALLOWED_TOOLS` with a by-case allow-list builder.
- `crates/fleet-snowfluff-ai/src/settings.rs`: `AiSettings` gains `task_router_mode`, `project_root`.
- `crates/fleet-snowfluff/src/chat_commands.rs`: `run_generation` restructured for the mix-mode buffer-then-decide branch; new tool-confirmation wait path.
- `crates/fleet-snowfluff/src/ai_commands.rs`: `build_provider()` gains the `PlainChat`/`ToolCapable` decision.
- New: a Task Router module, an Agent Runtime/Tool Registry module, the 4 native tools, a `tool-confirmation` Tauri window, `personas/task-router-rules.md`.
- New dependency: `sysinfo`.
- `ui/src/main.ts`: AI tab gains `task_router_mode` and `project_root` controls; new tool-confirmation window UI.
