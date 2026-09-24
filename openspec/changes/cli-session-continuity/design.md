## Context

See `proposal.md` for motivation. The relevant current state:

- `send_chat_message` reads the chat log, builds the persona-plus-history message list with `prompt::assemble_messages`, and passes it to a provider. `ClaudeCodeCli::chat()` and `Codex::chat()` use only the system messages and `latest_user_message(&messages)`; everything else is discarded, on the assumption that `--resume` / `codex exec resume` carries memory.
- The resume id lives in `ChatRuntimeState.cli_sessions: HashMap<(ConversationId, ProfileKey), ExternalSessionRef>` — in memory only, cleared by `new_chat_session`. The chat log itself is resumed from disk on restart (`find_latest_session`), so after a restart the transcript returns but the session ids do not.
- Each CLI provider already retries once without `--resume` when the first line looks resume-related; the caller is never told.
- `mix` mode's escalation re-dispatches to `default_profile` through `run_generation_fallback` with the persona-only message list, so a CLI escalation target starts a fresh session.
- Neither `spawn` sets `current_dir` or `kill_on_drop`. `stop_generation` aborts the tokio task, which drops the `Child` without terminating it.
- `ConversationId` is the chat-log path; the log file layout is `chat-logs/<date>/<ts>_<id>.jsonl` under the app config dir.

## Goals / Non-Goals

**Goals:**

- A CLI reply always has the conversation's recent history available: via the CLI's own resumed session when one is valid, otherwise via a preamble rendered from the transcript.
- CLI session references survive an app restart, without ever being applied in a context (working directory) they were not created in.
- CLI processes have a deterministic working directory and die when their generation is cancelled.

**Non-Goals:**

- An `Execution` record, `ContextManager` wiring, a `SessionManager`, or a runtime-adapter abstraction. None has a consumer here; `ContextManager` in particular would only wrap `assemble_messages` with no behavior change.
- Killing a CLI's whole process group. Write/edit/shell tools are denied by default, so a grandchild outliving a killed CLI needs the user to have enabled them; revisit when that becomes a default path.
- Any Codex-specific behavior change, or verifying Codex against a real subscription. It stays experimental.
- Persisting the tool trace, summaries, or anything beyond the chat transcript and session references.
- Treating the working directory as a security boundary.

## Decisions

**1. The transcript is canonical; the CLI session is a cache.** A resumed session is sent only the latest message (today's behavior, no per-turn cost on the warm path). A request with no usable resume additionally carries a history preamble. Alternatives: (a) leave continuity entirely to the CLI — leaves the escalation, stale-resume and post-restart gaps; (b) always resend history — duplicates what a resumed CLI already remembers and pays tokens every turn.

**2. The history is supplied at provider construction, not extracted from `chat()`'s message list.** The message list mixes persona few-shot examples (user/assistant pairs immediately after the system message) with real history, and nothing in it distinguishes them, so a provider deriving history from `messages` would render fake turns into the preamble. Instead `send_chat_message` already has the real history (`context`, capped by `cap_history`); it is passed to the CLI providers' constructors alongside `resume_session_id`, and `run_generation_fallback` receives it via `FallbackAttempt` so an escalation target gets it too. `AiProvider::chat()`'s signature is unchanged. Alternatives: tag few-shot messages so a provider can skip them (widens `Message` for one consumer); route it through `ContextManager` (would require reporting "fell back to fresh" upward, since the retry lives inside the provider).

**3. The preamble helper lives in the providers layer and is shared.** One function renders `(&[Message], budget) -> Option<String>`, used by both CLIs. It is applied inside `chat()` whenever `resume` is `None` at the start, and again when the stale-resume retry falls back — the only place that knows the resume failed. The rendering is: a short header, then turns oldest-to-newest as `User:` / `Assistant:` lines, then the current message; the most recent turns win when a character budget is exceeded (the prompt is a single process argument, so the budget is deliberately modest — a few thousand characters — to stay well inside per-platform argument limits). No history yields no preamble. The preamble goes in the prompt, not the system prompt, so the system prompt stays the persona alone.

**4. Session references are persisted in a sidecar next to the chat log.** File `<ts>_<id>.sessions.json`, containing a version and a list of `{ profile: ProfileKey, session_id, cwd }` entries (a list rather than a map, because `ProfileKey` is a struct and does not serialize as a JSON object key). The in-memory `cli_sessions` map stays the hot path: on a miss the sidecar is read lazily; every `store_cli_session_id` writes through, recording the working directory used. The session reference is read in two places — `send_chat_message`'s direct branch and `run_generation_fallback` (the escalation path) — so lookup goes through one shared helper used by both, and `store_cli_session_id` gains the session log path and the resolved working directory as parameters (today it has neither, and `ConversationId` is opaque and cannot be turned back into a path). Writes are best-effort like the other `*_store.rs` modules, and a missing or corrupt file reads as empty. Because a new chat creates a new log, it gets a new sidecar; nothing needs to be deleted, and removing a log removes its sessions. Alternatives: a special entry type in the JSONL (a closed `LogRole` enum feeds `entries_to_context`, so every reader would have to learn to skip it); one global store keyed by conversation path (needs garbage collection since keys outlive logs); deriving the reference from a persisted execution log (forces an execution record with no other consumer).

**5. A stored session is used only if its recorded working directory equals the one about to be used.** The assumption was that a CLI scopes its sessions by working directory, so an id created under one directory might not resolve under another. **Checked live against Claude Code 2.1.281: it does not** — `--resume <id>` from a different directory resumed the same session id and restored the conversation (each directory just gets its own project entry). The check is therefore stricter than Claude Code needs: a `project_root` change discards a session that would still have resumed, which costs a re-seeded (preamble) session instead of the warm one. It is kept anyway, deliberately: it is cheap, it is the safe direction, and Codex (not installed here) and other CLI versions are unverified. Revisit if discarding on a directory change proves to matter.

**6. Working directory is `project_root` when set, else a fixed application-owned directory.** The fallback is a `cli-workspace` folder under the app config dir, created on demand. If `project_root` is set but no longer exists, the application directory is used instead (logged), rather than letting the spawn fail with an opaque error. Alternatives: keep inheriting the launch directory (unpredictable, and it silently keys persisted sessions to wherever the app started); require `project_root` (breaks plain persona chat over a CLI for users who never set one). Changing `project_root` therefore invalidates stored sessions through Decision 5. This is not an access boundary: a CLI's read tools can still take absolute paths; the control remains `ClaudeCodeToolAccess`.

**7. `kill_on_drop(true)` on the chat spawns only.** Aborting the generation task drops the `Child`, which now terminates it. It is applied to `claude -p` and `codex exec` chat spawns, not to `trigger_login` (which is deliberately detached and awaited in its own task) or to availability checks. Process-group termination is a documented follow-up.

**8. `Codex` gets the same mechanical changes.** The shared preamble helper, the working directory, kill-on-drop and the sidecar (which is keyed by `ProfileKey` and needs no per-provider code) apply to both CLIs. Special-casing Codex out would cost more than including it. Tests cover only what needs no live CLI.

## Risks / Trade-offs

- [The preamble makes a fresh session's first prompt large] → bounded by a modest character budget, most-recent-turns-first; and the history is already capped by `CONTEXT_WINDOW_TURNS`.
- [A hard kill mid-turn may leave the CLI's session file with a partial turn, so a later resume fails or misbehaves] → the existing stale-resume fallback handles a failed resume, and the preamble now restores context on that path.
- [Stale resume becomes routine once ids persist across restarts and sessions get pruned] → the reason the preamble sits in the retry path (Decision 3), not only on the first message.
- [Resume-scoped-by-working-directory turned out not to hold for Claude Code 2.1.281, though it is unverified for Codex and other versions] → the directory is still recorded and compared (Decision 5); the only cost is an unnecessary re-seed after a `project_root` change.
- [`--allowedTools` / `--disallowedTools` are variadic in the CLI, so the prompt could be swallowed as a tool name whenever no `--model`/`--resume` follows them, giving an empty reply] → found during live verification and fixed by ending option parsing with `--` before the prompt (task 4.4); Codex's argument list has no variadic option and was left alone.
- [Resume-failure detection is string matching on CLI error text] → pre-existing; unchanged here, and its failure mode is the same as today's.
- [Codex behavior is unverified against a real install] → only mechanical changes, tests limited to prompt/arg construction and the sidecar, still marked experimental.
- [The sidecar holds opaque session ids and local paths] → stored locally beside the chat log it describes; no new data leaves the machine.
- [Setting the working directory changes what the CLIs' relative paths resolve against: Claude's auto-allowed `Read`/`Glob`/`Grep` and Codex's read-only sandbox now operate on `project_root` by default instead of an arbitrary launch directory] → intended (it is what a project assistant should do) and no wider than before, since those tools could already take absolute paths; called out here so it is a decision, not a side effect.
- [History is rendered as `User:` / `Assistant:` lines inside one prompt, so message text that itself begins `Assistant:` could look like a forged turn] → accepted: it is the user's own conversation and the CLI treats the prompt as a single user message.
- [Grandchild processes of a killed CLI may outlive it] → known follow-up; only reachable when the user has enabled write/shell tools.

## Migration Plan

No migration. Existing installs have no sidecar, which reads as "no stored sessions", so the first message after upgrade starts a fresh session exactly as a restart does today, now with a history preamble. Existing chat logs are unchanged. Rollback is reverting the change; a leftover sidecar file is ignored.
