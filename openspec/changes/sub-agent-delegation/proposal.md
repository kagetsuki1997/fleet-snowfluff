## Why

Aemeath's agent loop can only ever pursue one task at a time, in one continuous stream of tool calls against one conversation history. A request with genuinely independent sub-parts (research this, separately check that) has no way to be split up — the model either does everything itself, serially, in one unbroken context, or doesn't attempt the split at all. `docs/fleet-snowfluff-feature-planning.md`'s own roadmap names this gap as Stage 5 ("Sub-agent / Delegation", §6.13), dependent on Stage 3 (Agent Loop), Stage 4 (CLI session continuity, API-key tool calling), and Stage 4.5 (`execution-log-and-context`, archived) — all now in place.

## What Changes

Scope here is deliberately narrower than §6.13's full vision (a persisted child-`Execution` tree with `parent_execution_id`/`ExecutionKind`, cross-runtime diversity per role, budget ledgers, a live multi-agent status UI). This change ships the smallest version that is still genuinely a working delegation capability, converged via `/opsx:explore`+`/grill-me`, deferring every piece that doesn't yet have a real consumer — the same "don't build ahead of a consumer" principle `execution-log-and-context` already established for `ContextManager`/`ExecutionResult`.

- A new `delegate_task` capability, offered to every tool-calling-capable profile alongside the existing native tools (`web_search`, `read_file`, `list_directory`, `run_command`, `get_system_context`) — no new settings toggle, following the existing precedent that native tool availability is never settings-gated.
- Delegation is special-cased inside `AemeathAgentRuntime::run()`'s own loop — not a generic `impl Tool` going through the normal dispatch path, since `Tool::execute()`'s signature structurally cannot reach the `provider`/`registry`/`permission` references recursion needs. The tool's definition is still registered normally so the model can discover and call it like any other tool.
- Depth capped at exactly 1, structurally: a delegated child's own tool registry simply omits `delegate_task`, so recursive delegation is impossible by construction, not by a runtime-checked counter.
- Tool-call execution within one turn becomes concurrent (bounded, not unbounded) instead of strictly sequential — a general change to the agent loop, not delegation-specific, since the existing loop already batches multiple tool calls from one model turn but runs them one at a time.
- **Fixes a real, pre-existing bug** surfaced by this change, not introduced by it: `ToolConfirmationState`'s single-slot `pending` field silently drops one batch of confirmation requests if a second one arrives before the first resolves. This is only safe today because exactly one `AemeathAgentRuntime::run()` call is ever in flight at a time; concurrent children are the first thing to actually exercise the collision. Fixed by making `pending` a queue.
- A delegated child gets no projected context from its parent (deferred — see below), a plain task-focused system prompt (not Aemeath's persona framing, since the child's output is read by the parent model, not the end user), and its own narration is never streamed to the UI (no live multi-agent status for this change; invisible until the tool call resolves, same as every other tool call today).
- `docs/fleet-snowfluff-feature-planning.md` updated: §10.1's Sub-agent Context Projection (`SubAgentContextSpec`, `build_sub_agent_context`) moves from Stage 5 to Stage 7, bundled with compaction/memory retrieval — projection only has real value once there's an actual Context Engine with facts/decisions to project from, which doesn't exist until then.

## Capabilities

### New Capabilities

(none — delegation is an extension of the existing agent loop, not a separate subsystem)

### Modified Capabilities

- `agent-runtime`: the agent loop gains a bounded delegation capability (depth-1, no persisted child record, result folds into the parent's own tool-call trace); tool calls within one turn execute concurrently instead of sequentially; batched confirmation is hardened against a second batch arriving while one is still pending.

## Impact

- `crates/fleet-snowfluff-ai/src/agent_runtime.rs`: `AemeathAgentRuntime::run()` gains the special-cased delegation branch and bounded-concurrent tool dispatch (`futures_util`'s `buffer_unordered`, already a dependency).
- `crates/fleet-snowfluff-ai/src/prompt.rs` (or a new sibling module): a plain, non-persona system-prompt builder for a delegated child.
- `crates/fleet-snowfluff/src/tool_confirmation.rs`: `ToolConfirmationState.pending` changes from `Mutex<Option<PendingBatch>>` to a queue.
- No changes to `task_router.rs` (children reuse `DefaultTaskRouter::route()` unmodified), no changes to `execution.rs`/`execution_recorder.rs` (no persisted child record), no frontend changes (`ui/src/main.ts` untouched — nothing about delegation is visible to the UI).
- `docs/fleet-snowfluff-feature-planning.md`: Stage 5/Stage 7/§10 roadmap updated to reflect context projection's move (already applied during exploration, ahead of this proposal).
