//! `ConversationId`: identifies a Fleet Conversation, derived from the
//! session's log file path, not independently generated and stored.
//! Originally introduced in the app crate's `session_domain.rs`
//! alongside `ExecutionId`/`ExternalSessionRef` (both of which stay
//! there -- they're tied to `ChatRuntimeState`, a purely app-crate/
//! Tauri-runtime concept). This one moved here because `ToolContext`
//! (`agent_tool.rs`) needs it and must not pull in the app crate itself
//! -- `session_domain.rs` now just re-exports this definition, so
//! every existing app-crate call site keeps working unchanged.

use std::path::Path;

/// Nothing today needs to reference a conversation before its log file
/// exists or after it's been moved/renamed (Fleet has no such
/// feature), so an independently-persisted id would solve a problem
/// that doesn't exist yet. Kept behind this one function rather than
/// inlined at call sites specifically so that if that changes later,
/// it's a one-place edit, not a search-and-replace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConversationId(String);

impl ConversationId {
    pub fn from_session_path(path: &Path) -> Self { Self(path.to_string_lossy().into_owned()) }
}

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
}
