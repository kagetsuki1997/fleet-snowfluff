//! `ContextManager`: Group 4.3's basic version --
//! `build_context`/`record_execution` only, per design.md's Decisions
//! (no compaction, memory retrieval, or sub-agent projection --
//! `docs/fleet-snowfluff-feature-planning.md` §10's fuller Context
//! Engine is Stage 7's job, not this one's). A thin wrapper around
//! machinery that already exists (`prompt::assemble_messages`'s own
//! bounded-window capping, `chat_log_store::append_entry`'s own
//! persistence) rather than new logic -- Stage 3 draws the
//! `ContextManager`/`TaskRouter`/`SessionManager` boundary
//! conceptually, but doesn't yet have enough moving parts (sub-agents,
//! external runtime sessions) to need a heavier implementation than
//! "call the existing assembly function."

use std::path::Path;

use fleet_snowfluff_ai::{Language, LogEntry, LogRole, Message, Persona};

use crate::chat_log_store;

/// Decides "what to bring" for a turn and records what came of it --
/// kept separate from `TaskRouter` ("where to send it") and
/// `SessionManager`'s existing session-resume logic ("whether to
/// create/resume a runtime session"), per the planning doc's own
/// three-way boundary (§4's "這三個責任不要合併").
#[async_trait::async_trait]
pub trait ContextManager: Send + Sync {
    /// Assembles the message list for one turn: persona system prompt,
    /// few-shot examples, a bounded window of prior history read from
    /// `session_path`, and the new user message -- exactly
    /// `prompt::assemble_messages`'s own shape.
    async fn build_context(
        &self,
        session_path: &Path,
        persona: &Persona,
        language: Language,
        user_message: &str,
    ) -> Vec<Message>;

    /// Records one execution's outcome -- the assistant's final reply --
    /// into `session_path`'s persisted history, the same as any other
    /// chat exchange already is.
    async fn record_execution(&self, session_path: &Path, reply: &str);
}

/// The `ContextManager` this change actually ships.
pub struct AemeathContextManager;

#[async_trait::async_trait]
impl ContextManager for AemeathContextManager {
    async fn build_context(
        &self,
        session_path: &Path,
        persona: &Persona,
        language: Language,
        user_message: &str,
    ) -> Vec<Message> {
        let history_entries = chat_log_store::read_session(session_path);
        let history = fleet_snowfluff_ai::log::entries_to_context(&history_entries);
        fleet_snowfluff_ai::prompt::assemble_messages(persona, language, &history, user_message)
    }

    async fn record_execution(&self, session_path: &Path, reply: &str) {
        chat_log_store::append_entry(
            session_path,
            &LogEntry {
                role: LogRole::Assistant,
                content: reply.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use fleet_snowfluff_ai::persona::parse_persona;

    use super::*;

    const PERSONA_YAML: &str = r#"
name: "Test"
response_language: "auto"
personality: "friendly"
speech_style: "short"
"#;

    fn temp_session_path(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "fleet-snowfluff-context-manager-test-{name}-{}.jsonl",
            std::process::id()
        ));
        path
    }

    #[tokio::test]
    async fn build_context_matches_assemble_messages_directly_for_a_short_history() {
        let path = temp_session_path("short");
        let _ = std::fs::remove_file(&path);
        chat_log_store::append_entry(
            &path,
            &LogEntry {
                role: LogRole::User,
                content: "hi".to_string(),
                timestamp: "t".to_string(),
            },
        );
        chat_log_store::append_entry(
            &path,
            &LogEntry {
                role: LogRole::Assistant,
                content: "hello".to_string(),
                timestamp: "t".to_string(),
            },
        );

        let persona = parse_persona(PERSONA_YAML).unwrap();
        let manager = AemeathContextManager;
        let via_manager = manager.build_context(&path, &persona, Language::En, "how are you").await;

        let entries = chat_log_store::read_session(&path);
        let history = fleet_snowfluff_ai::log::entries_to_context(&entries);
        let via_direct_call = fleet_snowfluff_ai::prompt::assemble_messages(
            &persona,
            Language::En,
            &history,
            "how are you",
        );

        assert_eq!(via_manager, via_direct_call);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn build_context_still_applies_the_bounded_window_for_long_history() {
        // `ai-provider`'s "Bounded conversation context" requirement --
        // this proves the wrapper doesn't accidentally bypass capping
        // by, say, reading the file differently than `assemble_messages`
        // itself expects.
        let path = temp_session_path("long");
        let _ = std::fs::remove_file(&path);
        for i in 0..(fleet_snowfluff_ai::limits::CONTEXT_WINDOW_TURNS + 20) {
            chat_log_store::append_entry(
                &path,
                &LogEntry {
                    role: LogRole::User,
                    content: format!("turn {i}"),
                    timestamp: "t".to_string(),
                },
            );
        }

        let persona = parse_persona(PERSONA_YAML).unwrap();
        let manager = AemeathContextManager;
        let via_manager = manager.build_context(&path, &persona, Language::En, "latest").await;

        let entries = chat_log_store::read_session(&path);
        let history = fleet_snowfluff_ai::log::entries_to_context(&entries);
        let via_direct_call = fleet_snowfluff_ai::prompt::assemble_messages(
            &persona,
            Language::En,
            &history,
            "latest",
        );

        assert_eq!(via_manager, via_direct_call);
        assert!(
            via_manager.len() < entries.len(),
            "the assembled context must be smaller than the full unbounded history"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn record_execution_appends_an_assistant_entry() {
        let path = temp_session_path("record");
        let _ = std::fs::remove_file(&path);

        AemeathContextManager.record_execution(&path, "the final answer").await;

        let entries = chat_log_store::read_session(&path);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, LogRole::Assistant);
        assert_eq!(entries[0].content, "the final answer");
        let _ = std::fs::remove_file(&path);
    }
}
