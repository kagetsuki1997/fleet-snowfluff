## Why

`mix` mode's local-first attempt (`run_generation_mix_local`) never offers tools to the local model, even when the local profile (Ollama) is itself `ToolCapable` — deliberately scoped out by `agent-core-and-task-router`'s own design.md ("combining the two is real, separate design work, not resolved here"). `task-router-rules.md`'s own `ESCALATE` criteria currently treats *any* tool need (reading a file, searching the web) as a reason to escalate to the cloud `default_profile`, even for a task that's otherwise squarely `LOCAL` in spirit ("read my Cargo.toml and summarize it"). This wastes Ollama's own tool-calling capability — the exact same `AemeathAgentRuntime` it already uses when run as a plain `default_profile` outside mix mode — and sends read-only, low-stakes requests to the cloud purely because they touch a file or the web.

## What Changes

- The local-first attempt gets access to a small, fixed set of read-only, side-effect-free native tools (`read_file`, `list_directory`, `web_search`, `get_system_context` — never `run_command` or `delegate_task`), built dynamically: all four when a project folder is configured, or just `web_search`/`get_system_context` when it isn't (the other two would always need confirmation with no project folder set, so they'd be dead weight in the model's own tool list).
- `<<ESCALATE>>` detection (`detect_escalation`, unchanged mechanism — character-by-character prefix match against the literal marker) now governs a **commitment point**, not just a stream-vs-cloud choice: the first time either (a) any text diverges from the marker, or (b) a tool call is requested while text is still undecided, the turn is irreversibly committed to running locally. Before that point, nothing is shown or logged, exactly as today. After it, the turn runs to completion locally no matter what happens next — there is no mechanism in this codebase to un-show already-streamed text, so a mid-turn hand-off to cloud is not attempted once anything has been shown or executed.
- The one exception: if the model's *very first* action (before anything else has been shown or run) is a tool call that would need confirmation or is outright denied, that's treated as a conclusive escalate signal — the same discipline as today's text-based escalate (discard everything local, retry fresh against `default_profile`).
- Once committed, a tool call needing confirmation is never shown as a popup for this path — it's silently denied (reported to the model as "not permitted," same as any other `Confirm`-tier denial) — this path never opens the tool-confirmation window.
- `task-router-rules.md` is rewritten so the local model knows it may have some of these tools (it reads its own tool list, same as any `ToolCapable` turn) and should use them directly for in-scope tasks rather than reflexively escalating — narrowing the `ESCALATE` criteria for filesystem reads and web lookups specifically, while leaving every other `ESCALATE` category (shell execution, file writes, multi-step/iterative work, browser automation, long-running tasks) unchanged, since none of those map to the newly available tools.
- Implementation reuses `AemeathAgentRuntime::run()` unmodified for iteration 2 onward (seeded from whatever the hand-rolled first iteration produced) — `run()` itself gains no new capability and no signature change, since it has no way to abort mid-stream and this change doesn't need it to.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `task-router`: the "Local-first classification in mixed mode" requirement changes from a purely text-based, no-tools attempt to one where the local profile may use a fixed set of read-only tools, with the escalate/commit decision now covering both text and an early tool-call attempt.

## Impact

- `crates/fleet-snowfluff/src/chat_commands.rs`: `run_generation_mix_local` is restructured — its first iteration is hand-rolled directly against `provider.chat_with_tools()` (not a plain `.chat()` stream), watching for escalation via text exactly as today while also collecting any tool calls from that same iteration; iteration 2 onward hands off to the existing, unmodified `AemeathAgentRuntime::run()` with a simple always-deny-`Confirm` permission decider.
- `crates/fleet-snowfluff-ai/src/agent_runtime.rs`: no changes. `AemeathAgentRuntime::run()`'s loop, `PermissionDecider` trait, and `ToolRegistry` are reused as-is.
- `crates/fleet-snowfluff/src/tool_confirmation.rs`, `ui/src/main.ts`: no changes — this path never opens the confirmation window, by design.
- `personas/task-router-rules.md`: rewritten `ESCALATE` §1 (external information) and §2 (filesystem operations) to reflect the newly available `web_search`/`read_file`/`list_directory` tools; every other section unchanged.
- `openspec/specs/task-router/spec.md`: "Local-first classification in mixed mode" requirement updated with new scenarios covering tool use, the first-action-escalate exception, and the post-commitment visible-error behavior (extending a distinction the code already makes for infra failures today, not introducing a new one).
