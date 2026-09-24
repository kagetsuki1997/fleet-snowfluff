//! What a CLI-backed provider (`ClaudeCodeCli`, `Codex`) needs to know
//! about the conversation it is continuing, bundled so the app crate
//! builds it in one place and both providers take it as one argument
//! (`cli-session-continuity`).

use std::path::PathBuf;

use crate::message::Message;

/// Deliberately not `Default`: an empty `working_dir` would make the
/// CLI's spawn fail, so every construction names one.
#[derive(Debug, Clone)]
pub struct CliContext {
    /// The CLI's own session to resume, if a still-usable one is stored.
    pub resume_session_id: Option<String>,
    /// The conversation's real prior turns, oldest first -- never the
    /// persona few-shot pairs that `chat()`'s message list also carries.
    pub history: Vec<Message>,
    /// How many leading `history` messages the resumed session already
    /// holds. Anything after that index happened without it (for
    /// example, turns another provider answered in `mix` mode) and is
    /// sent along on resume. Meaningless without a resumed session.
    pub seen_turns: usize,
    /// Where the CLI is run -- always explicit, never inherited.
    pub working_dir: PathBuf,
}

impl CliContext {
    /// A context with no session to resume and no history.
    pub fn fresh(working_dir: PathBuf) -> Self {
        Self { resume_session_id: None, history: Vec::new(), seen_turns: 0, working_dir }
    }
}
