## Why

The CLI-backed subscription providers (`ClaudeCodeCli`, `Codex`) throw away everything in the assembled message list except the system prompt and the latest user message, relying entirely on the CLI's own `--resume` session for conversational memory. That breaks whenever a session is not actually resumed: after `mix`-mode escalation from Ollama (the CLI starts fresh and never sees the earlier turns), after a stale-resume fallback (the retry silently drops the whole conversation), and after an app restart (the chat log is resumed on disk but the in-memory `cli_sessions` map is empty, so the next message starts a cold session). Separately, neither CLI is given a working directory (it inherits wherever the app was launched from, so resume scoping is unpredictable) and neither is killed when a generation is cancelled (Stop drops the child handle without terminating the process).

## What Changes

- Treat Aemeath's own chat transcript as the canonical conversation and the CLI session as a cache of it. A resumed CLI session keeps receiving only the latest message; a session started without a usable resume (first message, stale-resume retry, escalation) additionally receives a compact preamble rendered from the recent transcript already present in the message list.
- Persist each conversation's CLI session references in a sidecar file next to its chat log (`<ts>_<id>.sessions.json`), keyed by profile, recording the session id and the working directory it was created under, and reload it when the conversation is resumed after a restart. A stored entry is used only if its recorded working directory matches the one about to be used.
- Spawn both CLIs with an explicit working directory: `project_root` when set, otherwise a fixed application-owned directory. The launch directory is never inherited.
- Set kill-on-drop on both CLI spawns so cancelling a generation terminates the process. Killing the whole process group (grandchildren such as a shell started by a CLI's own tool) is a known follow-up, not part of this change.
- `Codex` receives only these mechanical changes and stays marked experimental; nothing here was verified against a real Codex subscription.
- Explicitly deferred: an `Execution` record, wiring `ContextManager`, a `SessionManager`, and a runtime-adapter abstraction — none has a consumer in this change.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `ai-provider`: "Session continuity for CLI-backed subscription providers" changes (a fresh or stale-fallback session is seeded from the transcript instead of losing memory; persisted references survive restart), and new requirements cover the CLI working directory and terminating a CLI process on cancellation.
- `ai-chat`: "Session persistence" gains the persisted session-reference sidecar and its lifetime (tied to the conversation's log), and "Background completion, explicit cancellation" gains the requirement that cancelling terminates any CLI process behind the generation.

## Impact

- `crates/fleet-snowfluff-ai/src/providers/claude_code_cli.rs`, `providers/codex.rs`: spawn gains `current_dir` and `kill_on_drop`; `chat()` builds the fresh-session preamble; a new shared helper module for the preamble.
- `crates/fleet-snowfluff/src/chat_commands.rs`: replaces the in-memory-only `cli_sessions` lifetime with load/save against the sidecar; `new_chat_session` no longer needs to clear it (a new log means a new sidecar).
- `crates/fleet-snowfluff/src/chat_log_store.rs` (or a sibling `*_store.rs`): sidecar read/write next to the session log.
- `crates/fleet-snowfluff/src/ai_commands.rs`: passes the resolved working directory into provider construction.
- No new dependencies. No change to `AiProvider::chat()`'s signature.
- `cwd` is not a security boundary: a CLI's read tools can still take absolute paths. Access control remains `ClaudeCodeToolAccess`.
