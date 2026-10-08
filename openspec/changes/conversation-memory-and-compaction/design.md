## Context

See `proposal.md` for motivation and `docs/agent-foundation-memory-architecture.md` for the full exploration, including the comparison with tact, Ogham, and loopctl. This document records how the change fits the code that exists.

Facts about the current code that shape the approach:

- `AemeathContextManager::build_context(session_path, persona, language, user_message)` reads the session log, caps it with `cap_history(.., CONTEXT_WINDOW_TURNS)` (the constant is 20 **messages**, i.e. about 10 exchanges), and assembles `[system, few-shot, history, user]` through `prompt::assemble_messages`. It already prepends a tool-activity note to the user message. `record_execution(session_path, reply)` appends the assistant entry.
- Sessions are JSONL (`chat_log_store`), entries are `{role: User|Assistant|Error, content, timestamp}`, and the TypeScript UI mirrors that `LogRole`. Tool results are never persisted in history.
- In `send_chat_message` the history is read, the context is built, and the user entry is appended afterwards; generation then runs in a spawned task (`run_generation_with_tools` and the mix-mode variant). Mixed mode builds **two** message sets, a persona-only one for the default profile and one with routing guidance for the local attempt.
- A delegated child's tools are the parent registry minus `delegate_task` (verified in the runtime's tests). Anything else the parent has, the child inherits.
- Tools are `Tool` trait objects; native tools that need a service hold it themselves (e.g. `WebSearchTool` holds its transport). `native_tool_list()` builds the top-level set, `mix_local_tool_registry` the read-only local set.
- No database dependency exists yet; all stores are JSON or JSONL.
- Ollama silently drops a model's call to a tool absent from the request, which is why lazily hidden tools had to become stubs. Memory tools must therefore be eager.

## Goals / Non-Goals

**Goals:**

- Nothing the user said is lost to the model without a trace: dropped turns become a summary, and durable facts become memories.
- Every moving part is fail-safe: a broken summary, store, or background model never blocks or loses a chat message.
- Everything stored about the user is visible and erasable by the user.
- Storage is replaceable behind a trait, so embeddings can be added later without touching the runtime.

**Non-Goals:**

- Embeddings, `semantic-memory`, RAG, a project-facts namespace, mood or affection, sub-agent context projection (see proposal).
- Cross-language recall beyond keywords the assistant attaches at write time.
- Rewriting history or the session log format.

## Decisions

**The summary lives in a sidecar file, not in the session log.** A file next to the session records `{covered_through, text, updated_at}`. Adding a `Summary` role to `LogEntry` was rejected: it would change the JSONL schema, `entries_to_context`, and the TypeScript `LogRole`, for no benefit, and it would put model-written text inside the record the user reads. With a sidecar, a missing or corrupt file simply means no summary, which is the fail-safe state, and the transcript stays exactly as written.

**The summary is a pure, restartable function of the transcript.** It always covers the first `covered_through` messages of the context-visible history, where "context-visible" means the log entries `entries_to_context` keeps (error entries are skipped, so the index is over that filtered list, and the sidecar also stores the timestamp of the last covered entry as a guard against drift). A compaction takes the not-yet-covered slice that has fallen outside the history budget, asks the model to merge it into the existing summary, and only then advances `covered_through`. If it fails or the app quits mid-way, nothing is lost: the next trigger covers the same slice. There is no gap while a summary is pending, because uncovered turns that fell outside the budget keep being sent, up to a hard upper limit (see the history decision below).

**Compaction is batched and runs in the background, never before or during a reply.** `record_execution` is the hook, but it only notes that more uncovered content exists. A compaction starts when the uncovered slice reaches a size threshold, the session has been quiet for a short period, and no reply is being generated for it; otherwise the idle and close triggers cover it. This keeps summarization to a few calls per long conversation instead of one per reply, which matters for CLI quota and for a local model that would otherwise be busy exactly when the user sends the next message. One task per session is in flight at a time; extra triggers coalesce, and repeated failures back off per session so a down Ollama is not hammered. The idle and close triggers use a small scheduler that tracks last activity per session and the chat window's close event.

**The history sent with each request is bounded by a token budget, not a message count.** `cap_history(.., CONTEXT_WINDOW_TURNS)` is replaced by a selection that takes the newest messages that fit the budget, plus any uncovered older messages up to a hard upper limit, under a hard cap on message count as a backstop against floods of tiny messages. The budget is the smaller of a fixed constant and a share of the loaded model's window. The constant keeps behavior the same across machines in the normal case (the other Stage 1 guardrails are constants too, not user-configurable). The share keeps history from crowding out everything else on a small window (Ollama's default window is only 4096 tokens below roughly 24 GB of VRAM); the persona itself is protected by the fit check in the next decision, not by this share. A message count alone was rejected: it drops the same number of messages whether they were one word or a pasted page, so it neither bounds cost nor uses the context that is available, and it would trigger a summary on nearly every reply. A budget derived only from the window was rejected: the same chat would be treated differently on different machines. The estimator (ASCII at four characters per token, other characters one token each) is deliberately conservative for CJK.

**The persona is pinned by a fit check before every request, not by luck.** Measured on this project's Ollama (qwen3:14b), when history overflows but the system prompt fits, Ollama's chat layer keeps the system message and drops older history, so the persona survived windows of 512 and 256 tokens. The failure is different: when the system prompt plus the latest message do not fit, the server truncates the raw prompt (it logged `truncating input prompt ... keep=4`) and the persona is destroyed; the reply fell back to a generic greeting. Ollama's template also renders tool definitions into the system turn, so a long tool list can push a small window into that state, and the fixed 4096-token reply allowance counts against the window too. So before each request the runtime computes the fixed cost (persona and routing rules, few-shot examples, tool definitions including stubs, the current message, a reply allowance, and a safety margin) against the window. If it fits, the history budget is the smallest of the constant, 40% of the window, and what is left. If it does not fit, it sheds in a fixed order: history, few-shot examples, stub tool listings, then reduces the reply allowance toward a floor. If the persona, the current message, and the floor still do not fit, the request is not sent and the user is told the context window is too small. Raising `num_ctx` for the request was rejected: changing it makes Ollama reload the model, and it would override the user's own, larger setting. For CLI-backed profiles the persona already travels outside the conversation (Claude Code through `--system-prompt`, Codex through an instructions file), so their own compaction cannot summarize it; a test asserts it never appears in a turn. Because the window is only known for a loaded Ollama model, the first request for a model loads it with an empty generate call and reads `/api/ps`; the result is cached per model. After each response, a tripwire compares the server's reported prompt size with the estimate to catch silent truncation; whether `prompt_eval_count` (which sits beside a cached-token count) is a reliable signal is checked in the spike.

**Starting values, to be confirmed by the spike.**

| Setting | Value |
|---|---|
| History budget | min(3,000 tokens, 40% of loaded window) |
| Hard message cap / uncovered allowance | 60 messages / up to 1,500 further tokens |
| Batch threshold / quiet period / idle timeout | 1,200 uncovered tokens / 20 s / 10 min |
| Digest / keyword block | 400 tokens (about 15-20 facts) / at most 5 hits within 400 tokens |
| Tool-output bound / preview | 6,000 estimated tokens / 1,500 characters |
| Spill retention | 50 files or 7 days |
| Summary cap | 800 tokens |
| Safety net | 80% of the loaded window |

The tool-output bound is in estimated tokens, not characters: for CJK text one character is about one token, so a character limit that is comfortable for English could fill an entire window.

**The background model is picked by a single function.** The local profile if enabled, else the default profile. The provider is built fresh from the existing profile factory with a default `CliContext` (no `resume_session_id`), and its session id is discarded, so a CLI profile's chat session is never read or changed.

**The token safety net is a last resort inside `run()`, and the window size has one source per provider.** Before each provider call the runtime estimates request size (ASCII at 4 characters per token, other characters at one token each, as tact does, because the app's text is often CJK). Past about 80% of the model's window it stubs the oldest tool results, keeping the latest few, with a pointer to the spilled output. tact ships this "micro-compaction" off by default because it breaks prefix caching, so here it fires only under real pressure. The window comes from: Ollama's `/api/ps` `context_length` for the loaded model (verified on this machine: 32768 for qwen3:14b, whereas `/api/show` only reports the trained maximum, 40960, which would overestimate); a small built-in table of known models for OpenAI and Anthropic API profiles, with a conservative default for unknown names so the net triggers early, not late; and nothing for CLI-backed profiles, which run their own context management and never go through this loop. Before the first request of a run the Ollama model may not be loaded yet; the request is small then, and the conservative default applies.

**Tool-output bounding is separate from compaction, and the spilled file is readable.** A single bound applies to every tool result at the point it is added to `messages`, in the runtime, so native and MCP tools are treated alike. The oversized remainder is written under a dedicated directory in the app data directory, and the result ends with a visible marker, a short preview, and the file path. `read_file` outside the project root is Confirm tier today (and always Confirm when no project root is set), so without an exception that path would trigger a confirmation every time or never be read. `required_permission_for_path` therefore gets one narrow exception: a path inside the spill directory is Auto. Nothing else about file permissions changes, and the spill directory is pruned to a bounded number of files.

**Memory lives behind a `MemoryService` trait in `fleet-snowfluff-ai`, backed by SQLite.** `rusqlite` with the bundled SQLite, one database file in the app data directory, WAL mode, schema version tracked with `user_version`. The driver is synchronous, so calls run through `spawn_blocking`. A failure to open or query returns an error the caller treats as "no memory this turn". `semantic-memory` was rejected for v1: its writes require an embedder with no fallback, it is research-grade, and its default vector backend needs a C++ bridge across three desktop platforms. Plain files plus an in-memory index were rejected because the background pass and a chat turn write concurrently, and we want atomic writes and a clean upgrade path.

**Keyword matching is done on text we pre-tokenize, not by SQLite's tokenizers.** Memory text is split by script. Latin runs become lower-cased words; Han, kana, and Hangul runs become overlapping character bigrams; the result is stored as a space-joined token string in an FTS5 column that SQLite indexes with its plain tokenizer, and BM25 ranks it. Queries are tokenized the same way. SQLite's trigram tokenizer was rejected because it cannot match under three characters and many Chinese words have two. A one-character query term falls back to a substring scan of the normalized text, which is cheap at this scale and avoids indexing noisy unigrams. Dictionary segmenters were rejected for their size and per-language handling.

**Traditional-to-Simplified normalization is a checked-in character table, kept only if the spike shows it helps.** The spike measures recall on mixed Traditional/Simplified fixtures with and without it; if normalization does not raise recall, the table and task 3.4 are dropped and the `memory` spec's Traditional/Simplified scenario is removed with them. Matching only needs a one-to-one character mapping, not phrase-level conversion, so a generated constant table (derived from OpenCC's character data, Apache-2.0) is enough and avoids a dependency. Both index and query go through it; the stored text is untouched.

**Ranking for the digest is a simple weighted score.** Pinned first, then a blend of importance and an exponential recency decay, within a token budget. Keyword hits are ranked by BM25 with a floor below which nothing is injected. The weights and budgets are tuned in the spike, not guessed here.

**Sensitivity is enforced at the query, by audience, and covers saved entries only.** `build_context` takes an audience, local or cloud, derived from the profile that will handle the request (Ollama and Mock are local; every OpenAI or Anthropic profile, including CLI-backed ones, is cloud). The store's query excludes sensitive rows for the cloud audience, so there is no post-filter to forget. Mixed mode already builds separate message sets for the local attempt and the default profile; each gets its own audience, so an escalation to a cloud default does not carry sensitive memories. Memory tool results are filtered the same way. The guarantee is deliberately narrow: a fact the user typed into the chat is still part of the conversation text sent to a cloud profile, and the disclosure says so rather than implying otherwise.

**Memory tools are native tools holding an `Arc<dyn MemoryService>`.** `memory_search` and `memory_get` are read-only and Auto tier, and appear in the top-level, local, and child registries. `memory_remember`, `memory_forget` (Confirm tier, through the existing popup), and `compact_context` appear only in the top-level registry. Because a child inherits everything but `delegate_task`, `Tool` gains an `available_to_children` flag defaulting to true, and the write tools and the compaction tool return false; the existing child-registry derivation drops them the way it drops `delegate_task`. The tools take no scope argument. All five are registered as eager.

**`memory_remember` goes through a write policy; host-side paths cover weak models.** The policy dedupes against active memories (normalized equality or high keyword overlap) and applies `supersedes` when a correction names a prior memory. A small local model may not call the tool, and may not know the id of the memory it would be correcting, so the host has two backstops that do not depend on the model. For an explicit "remember" phrase in the user's message (a short phrase list per language), it runs a one-turn extraction after the reply. For a correction cue, it searches the existing memories for the few closest matches and puts them, with their ids, in the one-turn extraction prompt, so the model only has to pick which id the new fact replaces; if none fits it stores a new memory. Dedupe prevents a double write when the model's own tool call also fires. Both run in the background after the reply, so they are prompt but not strictly before the next message.

**Extraction at compaction proposes, the policy disposes.** The prompt asks for a JSON list of candidates (text, kind, importance, tags in the user's language and English) and forbids the sensitive categories. Parsing is lenient; a bad response yields no candidates. Auto-derived memories only dedupe, never supersede: contradiction detection without embeddings is unreliable, so a derived fact never overwrites one. Supersession is reserved for the user's explicit corrections.

**Settings gets its own Tauri commands, mirroring the MCP ones.** List, update, pin, mark sensitive, delete, and history, with a change event so the open list refreshes, reusing the `<details>` row pattern and the five locale files.

**The cloud disclosure gains a memory sentence, and every cloud pair is re-prompted.** The existing rule that changed disclosure text clears an earlier acknowledgement is extended from API-key pairs to all cloud pairs, subscription-backed ones included: memory brings old conversation content back into later requests, which is a materially new data flow even for a subscription account the user already trusts with messages. A cloud profile that has not acknowledged the new wording is not used for chat until it does, and the settings row shows the disclosure with a way to acknowledge it. The sentence states that saved memories and summaries may be sent, that sensitive saved memories are not, and that text typed into the conversation is not covered by that exception.

**A master switch turns memory off.** With it off there is no digest, no keyword retrieval, no memory tools, and no extraction; existing memories stay stored and untouched, and a "delete all memories" action erases them. The switch is on by default once AI is enabled, because the disclosure and the review list are the safeguards; compaction itself is not tied to it.

## Risks / Trade-offs

[Summary-of-summary drift: each compaction rewrites the previous summary and details erode] → bound the summary size, instruct the model to merge not append, keep the transcript on disk, and let the model request a focused compaction.

[The estimator or window is wrong and the persona is truncated anyway] → the safety margin and a conservative estimator make that unlikely, a hard pre-flight refusal replaces silent damage when the fixed cost alone cannot fit, and the post-response tripwire logs and surfaces truncation so the budget can be tightened.

[A small local model summarizes or extracts poorly in some languages] → extraction is advisory and reviewable; failures leave things unchanged; the spike checks all five languages before the prompts are frozen.

[Bigram matching is noisy, and recall without embeddings misses paraphrase] → the digest needs no matching; tags attached at write time bridge some synonyms and English; the model can query deeper through its tool; the store sits behind a trait so an embedder can be added.

[Sensitive-topic exclusion relies on the extraction model following the prompt] → explicit-only storage for those topics is a prompt rule, not a guarantee; everything is listed in Settings with a sensitive flag, and sensitive saved memories are withheld from cloud profiles (text the user typed into the chat is not covered, and the disclosure says so).

[First SQLite dependency adds a C build step and about a megabyte to the binary] → the bundled build is standard in Tauri apps; verify the three-platform release pipeline early, in the spike.

[Background calls to a CLI profile cost startup time and subscription quota] → accepted by decision; batching makes calls few (a size threshold after a quiet period, plus idle and close), coalesced, and backed off.

[A compaction races the next message, or competes with it for a busy local model] → compaction starts only after a quiet period and never while a reply is being generated, uncovered turns keep being sent meanwhile, and a missed compaction is simply redone from the transcript.

[Re-prompting every cloud user, API-key and subscription, with the updated disclosure] → intended; chat on those profiles waits for one acknowledgement, and the release notes say so.

[Host-side "remember" and correction detection misfires or misses] → a false positive only runs a one-turn extraction whose output is deduped and reviewable; a miss falls back to the model's tool and to compaction-time extraction. A wrong supersede is visible in the memory history and can be undone there.

[The token estimator is rough for CJK, so the budget could be exceeded or under-used] → it errs high, the hard limits bound the worst case, and the spike compares it with the server's reported prompt size.

[Memory text derived from web or tool content could carry injected instructions] → everything injected sits in a labeled untrusted block outside the system prompt, and memories derived from tool-heavy turns are lower importance by default.

## Migration Plan

Additive. The database and summary sidecars are created lazily on first use, so existing installs and sessions work unchanged. Existing long sessions are summarized lazily when they are next used, not scanned at startup, and the first summary of a long transcript is built from a bounded recent portion of the dropped region. Rollback is to delete `memory.db` and the summary sidecars; no existing data format changes. The updated disclosure takes effect on first launch after the update.

## Open Questions

- Ranking weights and the keyword score floor, and confirmation of the starting values above (set by the spike, measured on checked-in synthetic fixtures in five languages plus a local, uncommitted run on real chats).
- The per-language phrase and cue lists for the host-side detectors are short hand-kept lists, tuned against real chats in the spike.
- The contents of the built-in model-window table and its conservative default.
