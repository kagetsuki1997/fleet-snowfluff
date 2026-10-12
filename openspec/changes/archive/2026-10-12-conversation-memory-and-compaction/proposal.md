## Why

Aemeath forgets. Each request carries only the 20 most recent turns (`ai-provider`'s "Bounded conversation context"), and everything older is dropped silently: the pet cannot recall what the user told it last week, and a long chat quietly loses its own beginning. There is also no size bound on tool output inside a single agent run, so one large MCP result (a Notion page) enters the next request whole. Stage 7 of the roadmap exists to fix this, and the design is settled in `docs/agent-foundation-memory-architecture.md`: relationship memory first (what the pet knows about the user), local-first, reviewable, and usable in all five UI languages.

## What Changes

- **History budget and context compaction.** The fixed 20-message window becomes a fixed token budget, so many short messages are all kept and one huge message cannot blow the cost bound. Turns that fall outside it are folded into a rolling summary, injected as a labeled, untrusted block instead of being dropped. Summarization is batched (a size threshold, a quiet period, never during a reply) plus once at idle or close, not after every reply; a percentage-of-window trigger exists only as a safety net for long agent runs. The session log on disk is never rewritten. A background model call writes the summary: the local Ollama profile if enabled, else the default profile (CLI profiles included, via a fresh provider that never reuses the chat's CLI session).
- **Bounded tool output.** Inside one agent run, a tool result over a size limit is truncated with a visible marker and the remainder spilled to a pruned directory with a short preview; `read_file` may read that directory without a confirmation. This covers MCP results, which currently have no cap.
- **Memory store.** Persistent facts about the user in SQLite with FTS5, behind a `MemoryService` trait. No embedding model in v1. CJK text is indexed as character bigrams (Latin text as words) with Traditional-to-Simplified normalization for matching; the displayed text is kept as written. A correction supersedes the old memory (hidden from retrieval, still viewable); an explicit forget or delete is a hard erase including the index. A memory can be marked sensitive, and that saved entry is then never sent to cloud profiles (text typed into the chat still is, and the disclosure says so).
- **Retrieval.** Every request gets a small always-on digest (top-N facts ranked by pinned, importance, recency, within a token budget) plus a few keyword hits for the current message, injected as an untrusted `<retrieved_memory>` block. CLI-backed runtimes get this through the context builder; they get no tools.
- **Memory tools.** `memory_search` and `memory_get` in every Aemeath context (main loop, the read-only mixed-mode local attempt, delegated sub-agents). `memory_remember`, `memory_forget` (Confirm tier), and `compact_context` only in the top-level loop. All five stay eager, never behind `search_tools`.
- **Extraction.** Only when compaction runs, a background pass proposes memories from the folded turns. They are active immediately, badged `auto` (explicit ones `told`), and sensitive topics (health, money, credentials, sexuality, religion, politics) are never auto-extracted. Explicit "remember" requests and corrections write promptly, with host-side backstops so a weak local model that never calls a memory tool still gets them saved; a correction is resolved by letting the model pick the memory to replace from a short candidate list the host supplies.
- **Settings.** A Memory section in the AI tab lists every memory with its badge and lets the user edit, pin, mark sensitive, delete, and view superseded history. Strings in all five locales.
- The cloud data disclosure now states that retrieved memory and background summaries go to the enabled profile that handles the request. Every cloud profile, API-key and subscription alike, must acknowledge the updated wording before it is used for chat again.

## Capabilities

### New Capabilities

- `context-compaction`: rolling summary of older turns, its triggers, placement and trust boundary, failure behavior, the background model choice, and the model-callable compaction tool.
- `memory`: the memory store, keyword indexing across the five supported languages, supersession and erasure, the sensitive flag, always-on digest and keyword retrieval, memory tools and their context restrictions, and extraction rules.

### Modified Capabilities

- `ai-provider`: "Bounded conversation context" becomes a token budget and no longer means older turns vanish; the disclosure requirement also covers memory and summary content and the limit of the sensitive flag.
- `agent-runtime`: tool results inside a single run are size-bounded, for native and MCP-sourced tools alike, and the read-file tool may read the application's own spill directory without a confirmation.
- `settings-ui`: "AI tab" gains the Memory section.

## Impact

- `crates/fleet-snowfluff-ai/src/`: `ContextManager`/`AemeathContextManager` gain summary and memory injection; new `memory` module (trait, SQLite store, tokenizer, ranking, policies); new native tools; a tool-output bound in the agent runtime; a compaction runner with its background provider selection.
- `crates/fleet-snowfluff/src/`: wiring in `chat_commands.rs` (post-turn work off the hot path, idle/close triggers), Tauri commands for the Memory section, storage path for the database.
- A fixed token budget replaces `CONTEXT_WINDOW_TURNS` as the history limit.
- New dependency: `rusqlite` with the bundled SQLite (the repo's first database dependency); a Traditional-to-Simplified conversion table.
- `ui/src/main.ts`, `ui/src/style.css`, `locales/*.json`: Memory section and strings in en, ja, ko, zh-Hans, zh-Hant.
- Docs: `docs/agent-foundation-memory-architecture.md` (design source), the Stage 7 entry in `docs/fleet-snowfluff-feature-planning.md`.
- Out of scope, deliberately: mood and affection, sub-agent context projection, RAG, embeddings and `semantic-memory`, and a project-facts namespace.
