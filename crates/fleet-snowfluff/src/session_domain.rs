//! Three concepts a chat turn touches that must not be conflated
//! (added per a `docs/fleet-snowfluff-feature-planning.md` update made
//! mid-implementation of `subscription-first-chat`, and grilled against
//! this change's own already-built code before writing any of this --
//! see that change's design.md for the full resolution):
//!
//! - **`ConversationId`**: the user-visible, long-lived chat container --
//!   already exists in substance as `chat_log_store`'s session file, identified
//!   by its `PathBuf`; this just gives that identity a name and a type instead
//!   of leaving it an implicit path.
//! - **`ExecutionId`**: one turn's execution -- genuinely new. Today a turn is
//!   only the `send_chat_message` -> `run_generation` call chain, with no id
//!   and no per-turn recorded state. This module adds the id; it does not add
//!   execution status, tool trace, timeout, or cancellation-per-execution --
//!   those are Stage 3's job, once its Agent Core actually needs them and has a
//!   real shape for them, not guessed at here.
//! - **`ExternalSessionRef`**: a CLI-backed subscription provider's own
//!   session/thread id (Claude's `session_id`, Codex's `thread_id`) -- already
//!   exists in substance as the bare `String` values in
//!   `ChatRuntimeState.cli_sessions`; this gives that a name too.
//!
//! This is a typing-only pass: nothing here changes observable
//! behavior, and nothing here is persisted to disk. `PendingGeneration`
//! in `chat_commands.rs` is deliberately left untouched (still one
//! global slot, no `execution_id` field) -- seeing this section's own
//! design.md entry for why that's not the same call as re-keying
//! `cli_sessions` (which *is* changed, to include `ConversationId`).

use std::path::Path;

/// Identifies a Fleet Conversation -- derived from the session's log
/// file path, not independently generated and stored. Nothing today
/// needs to reference a conversation before its log file exists or
/// after it's been moved/renamed (Fleet has no such feature), so an
/// independently-persisted id would solve a problem that doesn't exist
/// yet. Kept behind this one function rather than inlined at call
/// sites specifically so that if that changes later, it's a one-place
/// edit, not a search-and-replace across the app crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConversationId(String);

impl ConversationId {
    pub fn from_session_path(path: &Path) -> Self { Self(path.to_string_lossy().into_owned()) }
}

/// Identifies one turn's execution -- the `send_chat_message` ->
/// `run_generation` call chain. Generated fresh per call, never
/// persisted, never sent over IPC (nothing on the frontend needs
/// per-execution visibility yet, so it doesn't appear in `ChatEvent`/
/// `ChatStateSnapshot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExecutionId(u64);

impl ExecutionId {
    /// A fresh, effectively-unique id for one execution. Uses the
    /// thread-local RNG already available via the `rand` crate
    /// (already a workspace dependency, already used the same way by
    /// `chat_log_store::new_session_path`'s own session id) rather than
    /// pulling in a UUID crate for a value that's never persisted or
    /// compared across process restarts.
    pub fn new() -> Self { Self(rand::random()) }
}

/// A CLI-backed subscription provider's own session/thread id (Claude's
/// `session_id`, Codex's `thread_id`) -- a bare wrapper, deliberately
/// with no internal tag for which runtime it belongs to. Wherever this
/// is stored (`ChatRuntimeState.cli_sessions: HashMap<(ConversationId,
/// ProfileKey), ExternalSessionRef>`), the map's own key already
/// disambiguates the conversation and provider; a redundant tag on the
/// value could only ever drift from the key, never add real
/// information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSessionRef(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_id_from_session_path_is_deterministic() {
        let path = Path::new("chat-logs/2026-09-18/20260918T120000Z_abc123.jsonl");
        assert_eq!(
            ConversationId::from_session_path(path),
            ConversationId::from_session_path(path)
        );
    }

    #[test]
    fn conversation_id_differs_for_different_paths() {
        let a = ConversationId::from_session_path(Path::new("chat-logs/a.jsonl"));
        let b = ConversationId::from_session_path(Path::new("chat-logs/b.jsonl"));
        assert_ne!(a, b);
    }

    #[test]
    fn execution_ids_generated_in_succession_are_distinct() {
        let ids: Vec<ExecutionId> = (0..100).map(|_| ExecutionId::new()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "100 freshly generated ids should not collide");
    }
}
