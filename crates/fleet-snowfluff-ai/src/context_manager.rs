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
    agent_runtime::{ToolInvocation, ToolOutcome},
    execution::{Execution, ExecutionEvent},
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

/// Reads and appends a conversation's execution log --
/// `execution-log-and-context`'s design.md Decision 6. Parallel in shape
/// to [`SessionLog`] (same reasoning: path resolution needs `AppHandle`,
/// which this crate must not depend on), implemented in the app crate by
/// delegating to `execution_log_store`. Kept as a second trait rather
/// than merged into `SessionLog` because the two are different formats
/// with different lifetimes, read by the same component for the same
/// `session_path` but not always by the same future reader (a later
/// Stage 7 compaction reader, say, may legitimately want only one).
pub trait ExecutionLog: Send + Sync {
    fn read(&self, session_path: &Path) -> Vec<Execution>;
    fn append(&self, session_path: &Path, event: &ExecutionEvent);
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
/// nothing -- it holds its `SessionLog`/`ExecutionLog` behind trait
/// objects, the same way `WebSearchTool` holds its `SearchTransport`,
/// since callers construct exactly one and never need to know its
/// concrete type.
pub struct AemeathContextManager {
    log: Box<dyn SessionLog>,
    execution_log: Box<dyn ExecutionLog>,
}

impl AemeathContextManager {
    pub fn new(log: Box<dyn SessionLog>, execution_log: Box<dyn ExecutionLog>) -> Self {
        Self { log, execution_log }
    }
}

/// Mirrors `providers::history_preamble`'s own header-then-body style
/// (that module is private to `providers`, so this is a small sibling
/// rendering, not a shared function -- same presentational convention,
/// not the same code) for the one other place this codebase splices
/// non-conversational context ahead of the current message.
const TOOL_ACTIVITY_HEADER: &str =
    "Tools were used earlier in this conversation (context only; reply to the current message):";

fn render_tool_line(invocation: &ToolInvocation) -> String {
    let outcome = match invocation.outcome {
        ToolOutcome::Executed { ok: true } => "executed successfully",
        ToolOutcome::Executed { ok: false } => "executed and failed",
        ToolOutcome::DeniedByPolicy => "denied by standing permission policy",
        ToolOutcome::DeclinedByUser => "declined by the user",
        ToolOutcome::Rejected { .. } => "rejected (malformed call)",
    };
    format!("- {}: {outcome}", invocation.name)
}

/// `None` when the immediately preceding `Execution` made no tool calls
/// (including when there is no preceding `Execution` at all, or it
/// hasn't finished) -- the common case, where `build_context`'s
/// behavior must stay exactly what it was before this note existed.
fn tool_activity_note(prior: Option<&Execution>) -> Option<String> {
    let trace = &prior?.end.as_ref()?.trace;
    if trace.is_empty() {
        return None;
    }
    let lines: Vec<String> = trace.iter().map(render_tool_line).collect();
    Some(format!("{TOOL_ACTIVITY_HEADER}\n{}", lines.join("\n")))
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

        // The immediately preceding `Execution` for this conversation --
        // `execution_log::read` pairs start/end events in file order, so
        // the last entry is the most recent one (`execution-log-and-
        // context`'s "A turn's tool activity informs the next turn's
        // context"). A discarded `mix`-mode local attempt that escalated
        // or failed is never last here, since its own fallback's
        // `Execution` is appended after it -- exactly the one whose
        // trace (if any) should inform this next turn.
        let executions = self.execution_log.read(session_path);
        let user_message = match tool_activity_note(executions.last()) {
            Some(note) => format!("{note}\n\n{user_message}"),
            None => user_message.to_string(),
        };

        prompt::assemble_messages(persona, language, &history, &user_message)
    }

    /// Appends the transcript-facing reply text via `SessionLog`, as
    /// this always has. Does **not** also append an `Execution` end
    /// event via `ExecutionLog` -- design.md's Decision 6 considered
    /// this, but by the time this runs, `ExecutionRecorder` (Decision 5)
    /// has already written every field of that end event (status,
    /// external_ref, seen_turns, trace, ended_at) from the
    /// `chat_commands.rs` level; this method's own signature
    /// (`session_path`, `reply`) has no further information to
    /// contribute, so there is nothing left for it to write. The
    /// `ExecutionLog` side of this type exists for `build_context`'s
    /// read, not for a second, redundant write here.
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
    use std::{collections::HashMap, path::PathBuf, sync::Mutex};

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

    /// An in-memory `ExecutionLog` -- mirrors `InMemorySessionLog` above;
    /// `pair_events` does the real start/end pairing, exercised by
    /// `execution.rs`'s own tests, so this double only needs to store and
    /// replay whatever `Execution`s a test seeds it with directly.
    #[derive(Default)]
    struct InMemoryExecutionLog {
        executions: Mutex<HashMap<std::path::PathBuf, Vec<Execution>>>,
    }

    impl ExecutionLog for InMemoryExecutionLog {
        fn read(&self, session_path: &Path) -> Vec<Execution> {
            self.executions.lock().unwrap().get(session_path).cloned().unwrap_or_default()
        }

        fn append(&self, _session_path: &Path, _event: &ExecutionEvent) {
            unimplemented!("no test under this module calls record_execution's ExecutionLog side")
        }
    }

    fn manager_with(entries: Vec<LogEntry>, path: &Path) -> AemeathContextManager {
        manager_with_executions(entries, Vec::new(), path)
    }

    fn manager_with_executions(
        entries: Vec<LogEntry>,
        executions: Vec<Execution>,
        path: &Path,
    ) -> AemeathContextManager {
        let log = InMemorySessionLog::default();
        for entry in entries {
            log.append(path, &entry);
        }
        let execution_log = InMemoryExecutionLog::default();
        execution_log.executions.lock().unwrap().insert(path.to_path_buf(), executions);
        AemeathContextManager::new(Box::new(log), Box::new(execution_log))
    }

    fn entry(role: LogRole, content: &str) -> LogEntry {
        LogEntry { role, content: content.to_string(), timestamp: "t".to_string() }
    }

    fn profile() -> crate::settings::ProfileKey {
        crate::settings::ProfileKey {
            provider: crate::message::ProviderKind::Anthropic,
            auth_method: crate::settings::AuthMethod::Subscription,
        }
    }

    /// A finished `Execution` with the given tool trace -- `route`/
    /// `profile`/`working_dir`/timestamps are irrelevant to
    /// `tool_activity_note`, which only looks at `end.trace`.
    fn execution_with_trace(path: &Path, trace: Vec<ToolInvocation>) -> Execution {
        Execution {
            id: crate::execution::ExecutionId::new(),
            conversation_id: crate::conversation::ConversationId::from_session_path(path),
            route: crate::execution::ExecutionPath::Direct,
            profile: profile(),
            working_dir: PathBuf::new(),
            started_at: "t0".to_string(),
            end: Some(crate::execution::ExecutionEnd {
                id: crate::execution::ExecutionId::new(),
                status: crate::execution::ExecutionStatus::Completed,
                external_ref: None,
                seen_turns: None,
                trace,
                ended_at: "t1".to_string(),
            }),
        }
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
        let manager =
            AemeathContextManager::new(Box::new(log), Box::new(InMemoryExecutionLog::default()));

        manager.record_execution(path, "the final answer").await;

        let recorded = manager.log.read(path);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].role, LogRole::Assistant);
        assert_eq!(recorded[0].content, "the final answer");
    }

    #[tokio::test]
    async fn a_prior_tool_calling_turn_adds_a_note_to_the_next_turns_context() {
        let path = Path::new("session.jsonl");
        let trace = vec![ToolInvocation {
            name: "read_file".to_string(),
            outcome: ToolOutcome::Executed { ok: true },
        }];
        let manager =
            manager_with_executions(vec![], vec![execution_with_trace(path, trace)], path);

        let persona = parse_persona(PERSONA_YAML).unwrap();
        let messages =
            manager.build_context(path, &persona, Language::En, "what did you find?").await;

        let last = messages.last().unwrap();
        assert!(last.content.contains("read_file: executed successfully"));
        assert!(last.content.ends_with("what did you find?"));
    }

    #[tokio::test]
    async fn a_prior_plain_turn_adds_no_note() {
        let path = Path::new("session.jsonl");
        let manager =
            manager_with_executions(vec![], vec![execution_with_trace(path, Vec::new())], path);

        let persona = parse_persona(PERSONA_YAML).unwrap();
        let messages = manager.build_context(path, &persona, Language::En, "hello").await;

        assert_eq!(messages.last().unwrap().content, "hello");
    }

    #[tokio::test]
    async fn no_preceding_execution_at_all_adds_no_note() {
        let path = Path::new("session.jsonl");
        let manager = manager_with(vec![], path);

        let persona = parse_persona(PERSONA_YAML).unwrap();
        let messages = manager.build_context(path, &persona, Language::En, "hello").await;

        assert_eq!(messages.last().unwrap().content, "hello");
    }

    #[tokio::test]
    async fn the_tool_activity_note_never_leaks_persona_few_shot_examples() {
        // Same guard `history_preamble` already has: the note is built
        // from the execution trace alone, never from the assembled
        // message list, so it cannot echo a few-shot example back as if
        // it were real tool activity.
        const PERSONA_WITH_EXAMPLES: &str = r#"
name: "Test"
response_language: "auto"
personality: "friendly"
speech_style: "short"
few_shot_examples:
  en:
    - user: "hi"
      pet: "hey there"
"#;
        let path = Path::new("session.jsonl");
        let trace = vec![ToolInvocation {
            name: "read_file".to_string(),
            outcome: ToolOutcome::Executed { ok: true },
        }];
        let manager =
            manager_with_executions(vec![], vec![execution_with_trace(path, trace)], path);

        let persona = parse_persona(PERSONA_WITH_EXAMPLES).unwrap();
        let messages = manager.build_context(path, &persona, Language::En, "hello").await;

        let note_message = messages.last().unwrap();
        assert!(note_message.content.contains("read_file"));
        assert!(!note_message.content.contains("hey there"), "must not echo the few-shot example");
    }
}
