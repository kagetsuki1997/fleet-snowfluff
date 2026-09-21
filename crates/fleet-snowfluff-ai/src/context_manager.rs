//! `ContextManager`: Group 4.3's basic version --
//! `build_context`/`record_execution` only, per design.md's Decisions
//! (no compaction, memory retrieval, or sub-agent projection --
//! `docs/fleet-snowfluff-feature-planning.md` §10's fuller Context
//! Engine is Stage 7's job, not this one's). A thin wrapper around
//! `prompt::assemble_messages`'s own bounded-window capping rather than
//! new logic -- Stage 3 draws the `ContextManager`/`TaskRouter`/
//! `SessionManager` boundary conceptually, but doesn't yet have enough
//! moving parts (sub-agents, external runtime sessions) to need a
//! heavier implementation than "call the existing assembly function."
//!
//! Moved here from the app crate alongside `agent_tool`/`agent_runtime`/
//! `native_tools` -- but unlike those, this module has no file I/O of
//! its own: reading/writing a conversation's persisted session history
//! needs `AppHandle`-resolved paths and date-folder naming
//! (`chat_log_store.rs`), which is squarely app-crate/Tauri territory,
//! not something this crate should ever grow. [`SessionLog`] is the
//! seam: the app crate's `chat_log_store` module already has functions
//! shaped exactly like its two methods, so implementing it there is a
//! few-line adapter, not a rewrite.

use std::path::Path;

use crate::{
    log::{self, LogEntry, LogRole},
    message::Message,
    persona::{Language, Persona},
    prompt,
};

/// Reads and appends a conversation's persisted session history --
/// implemented by the app crate's `chat_log_store` module, whose
/// `read_session`/`append_entry` already have exactly this shape.
/// Kept as its own trait (not a direct file-I/O call) specifically so
/// this crate never needs to depend on the app crate for path
/// resolution (`chat_logs_dir` needs an `AppHandle`).
pub trait SessionLog: Send + Sync {
    fn read(&self, session_path: &Path) -> Vec<LogEntry>;
    fn append(&self, session_path: &Path, entry: &LogEntry);
}

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

/// The `ContextManager` this change actually ships, generic over
/// nothing -- it holds its `SessionLog` behind a trait object, the same
/// way `WebSearchTool` holds its `SearchTransport`, since callers
/// construct exactly one and never need to know its concrete type.
pub struct AemeathContextManager {
    log: Box<dyn SessionLog>,
}

impl AemeathContextManager {
    pub fn new(log: Box<dyn SessionLog>) -> Self { Self { log } }
}

#[async_trait::async_trait]
impl ContextManager for AemeathContextManager {
    async fn build_context(
        &self,
        session_path: &Path,
        persona: &Persona,
        language: Language,
        user_message: &str,
    ) -> Vec<Message> {
        let history_entries = self.log.read(session_path);
        let history = log::entries_to_context(&history_entries);
        prompt::assemble_messages(persona, language, &history, user_message)
    }

    async fn record_execution(&self, session_path: &Path, reply: &str) {
        self.log.append(
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
    use std::{collections::HashMap, sync::Mutex};

    use super::*;
    use crate::persona::parse_persona;

    const PERSONA_YAML: &str = r#"
name: "Test"
response_language: "auto"
personality: "friendly"
speech_style: "short"
"#;

    /// An in-memory `SessionLog` -- these tests exercise the
    /// assembly/capping orchestration `AemeathContextManager` owns, not
    /// real file I/O, which belongs to the app crate's own
    /// `chat_log_store` tests instead.
    #[derive(Default)]
    struct InMemorySessionLog {
        sessions: Mutex<HashMap<std::path::PathBuf, Vec<LogEntry>>>,
    }

    impl SessionLog for InMemorySessionLog {
        fn read(&self, session_path: &Path) -> Vec<LogEntry> {
            self.sessions.lock().unwrap().get(session_path).cloned().unwrap_or_default()
        }

        fn append(&self, session_path: &Path, entry: &LogEntry) {
            self.sessions
                .lock()
                .unwrap()
                .entry(session_path.to_path_buf())
                .or_default()
                .push(entry.clone());
        }
    }

    fn manager_with(entries: Vec<LogEntry>, path: &Path) -> AemeathContextManager {
        let log = InMemorySessionLog::default();
        for entry in entries {
            log.append(path, &entry);
        }
        AemeathContextManager::new(Box::new(log))
    }

    fn entry(role: LogRole, content: &str) -> LogEntry {
        LogEntry { role, content: content.to_string(), timestamp: "t".to_string() }
    }

    #[tokio::test]
    async fn build_context_matches_assemble_messages_directly_for_a_short_history() {
        let path = Path::new("session.jsonl");
        let history_entries = vec![entry(LogRole::User, "hi"), entry(LogRole::Assistant, "hello")];
        let manager = manager_with(history_entries.clone(), path);

        let persona = parse_persona(PERSONA_YAML).unwrap();
        let via_manager = manager.build_context(path, &persona, Language::En, "how are you").await;

        let history = log::entries_to_context(&history_entries);
        let via_direct_call =
            prompt::assemble_messages(&persona, Language::En, &history, "how are you");

        assert_eq!(via_manager, via_direct_call);
    }

    #[tokio::test]
    async fn build_context_still_applies_the_bounded_window_for_long_history() {
        // `ai-provider`'s "Bounded conversation context" requirement --
        // this proves the wrapper doesn't accidentally bypass capping.
        let path = Path::new("session.jsonl");
        let history_entries: Vec<LogEntry> = (0..(crate::limits::CONTEXT_WINDOW_TURNS + 20))
            .map(|i| entry(LogRole::User, &format!("turn {i}")))
            .collect();
        let manager = manager_with(history_entries.clone(), path);

        let persona = parse_persona(PERSONA_YAML).unwrap();
        let via_manager = manager.build_context(path, &persona, Language::En, "latest").await;

        let history = log::entries_to_context(&history_entries);
        let via_direct_call = prompt::assemble_messages(&persona, Language::En, &history, "latest");

        assert_eq!(via_manager, via_direct_call);
        assert!(
            via_manager.len() < history_entries.len(),
            "the assembled context must be smaller than the full unbounded history"
        );
    }

    #[tokio::test]
    async fn record_execution_appends_an_assistant_entry() {
        let path = Path::new("session.jsonl");
        let log = InMemorySessionLog::default();
        let manager = AemeathContextManager::new(Box::new(log));

        manager.record_execution(path, "the final answer").await;

        let recorded = manager.log.read(path);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].role, LogRole::Assistant);
        assert_eq!(recorded[0].content, "the final answer");
    }
}
