//! Folding turns that have fallen outside the history budget into the rolling
//! summary (`context-compaction`).
//!
//! The runner is a pure function of the transcript and the existing summary: it
//! works out which turns the summary should newly cover, asks a
//! [`SummaryWriter`] to merge them in, screens the result, and
//! only then advances the summary. Any failure leaves the stored summary
//! untouched, so the next attempt covers the same turns again; nothing is lost
//! by a crash, a timeout, or an empty reply.

use std::path::Path;

use async_trait::async_trait;
use futures_util::StreamExt;

use crate::{
    log::{self, LogEntry},
    message::Message,
    persona::Language,
    prompt::{self, select_history_start, HistoryLimits},
    provider::AiProvider,
    sensitive::remove_sensitive_sentences,
    summary::{ConversationSummary, SummaryStore},
    token_estimate::{estimate_message_tokens, estimate_tokens},
};

/// The most newly folded text handed to the writer in one go. A long session
/// summarized for the first time would otherwise send its whole backlog; only
/// the newest part within this size is read, and the summary still advances
/// over the rest.
pub const FOLD_INPUT_MAX_TOKENS: usize = 6_000;

/// The longest a stored summary may be. The writer is told to stay well under
/// this; this is the hard limit that keeps the summary from growing without
/// bound over many compactions.
pub const SUMMARY_MAX_TOKENS: usize = 800;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionError {
    /// The model could not be reached or failed while writing.
    Writer(String),
    /// The model returned nothing usable, or everything it returned was
    /// screened out.
    Empty,
    /// The summary could not be saved.
    Store(String),
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Writer(reason) => write!(f, "the summary could not be written: {reason}"),
            Self::Empty => write!(f, "the summary came back empty"),
            Self::Store(reason) => write!(f, "the summary could not be saved: {reason}"),
        }
    }
}

impl std::error::Error for CompactionError {}

/// What a [`SummaryWriter`] is asked to merge.
pub struct SummaryRequest<'a> {
    /// The notes so far, if any.
    pub existing: Option<&'a str>,
    /// The turns to fold in, oldest first.
    pub newer: &'a [Message],
    pub language: Language,
    /// What the assistant asked to be sure to preserve, when it requested the
    /// compaction itself.
    pub focus: Option<&'a str>,
}

/// Produces the merged notes. Implemented over a real model by
/// [`ProviderSummaryWriter`]; tests use a scripted one.
#[async_trait]
pub trait SummaryWriter: Send + Sync {
    async fn write(&self, request: &SummaryRequest<'_>) -> Result<String, CompactionError>;
}

pub fn summary_system_prompt(language: Language) -> String {
    let language_name = prompt::language_name(language);
    format!(
        "You keep neutral background notes about an ongoing conversation between a user and an \
         assistant. You are given the existing notes (possibly empty) and newer messages. Write \
         updated notes that merge both.\nRules:\n- Plain third-person notes. Never write in the \
         assistant's voice or any character's voice.\n- Keep facts, plans, decisions and open \
         questions. Drop greetings, small talk and the weather.\n- Leave out anything about \
         health, medication, money or debt, passwords or credentials, sexuality, religion, or \
         politics.\n- Never invent anything that is not in the messages.\n- Write the notes in \
         {language_name}, whatever language these instructions are in.\n- At most 150 words. \
         Output only the notes."
    )
}

pub fn summary_user_message(
    existing: Option<&str>,
    newer: &[Message],
    focus: Option<&str>,
) -> String {
    let transcript: Vec<String> = newer
        .iter()
        .map(|message| {
            let speaker = match message.role {
                crate::message::Role::User => "user",
                crate::message::Role::Assistant => "assistant",
                crate::message::Role::System => "system",
                crate::message::Role::Tool => "tool",
            };
            format!("{speaker}: {}", message.content)
        })
        .collect();
    let mut text = format!(
        "Existing notes:\n{}\n\nNewer messages:\n{}",
        existing.filter(|notes| !notes.trim().is_empty()).unwrap_or("(none)"),
        transcript.join("\n")
    );
    if let Some(focus) = focus.map(str::trim).filter(|focus| !focus.is_empty()) {
        text.push_str(&format!("\n\nMake sure the notes preserve: {focus}"));
    }
    text
}

/// A [`SummaryWriter`] that asks an ordinary chat provider.
pub struct ProviderSummaryWriter {
    provider: Box<dyn AiProvider>,
}

impl ProviderSummaryWriter {
    pub fn new(provider: Box<dyn AiProvider>) -> Self { Self { provider } }
}

#[async_trait]
impl SummaryWriter for ProviderSummaryWriter {
    async fn write(&self, request: &SummaryRequest<'_>) -> Result<String, CompactionError> {
        let messages = vec![
            Message::system(summary_system_prompt(request.language)),
            Message::user(summary_user_message(request.existing, request.newer, request.focus)),
        ];
        let mut stream = self
            .provider
            .chat(messages)
            .await
            .map_err(|err| CompactionError::Writer(err.to_string()))?;
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk.map_err(|err| CompactionError::Writer(err.to_string()))?.delta);
        }
        Ok(text)
    }
}

/// Lets a tool ask for a compaction of one conversation right now, optionally
/// with something to preserve. Implemented by the app, which knows how to reach
/// the provider and the session.
#[async_trait]
pub trait CompactionRequester: Send + Sync {
    async fn compact(
        &self,
        session_path: &Path,
        focus: Option<&str>,
    ) -> Result<CompactionOutcome, CompactionError>;
}

/// One compaction to perform.
pub struct CompactionJob<'a> {
    pub session_path: &'a Path,
    /// The session's full log, as read from disk.
    pub entries: &'a [LogEntry],
    pub language: Language,
    pub limits: HistoryLimits,
    pub focus: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionOutcome {
    /// Everything outside the budget is already covered, so there is nothing to
    /// fold.
    NothingToFold,
    Updated(ConversationSummary),
}

/// Keeps whole leading sentences of `text` until the estimate would pass
/// `max_tokens`.
fn truncate_to_tokens(text: &str, max_tokens: usize) -> String {
    if estimate_tokens(text) <= max_tokens {
        return text.to_string();
    }
    let mut kept = String::new();
    for sentence in text.split_inclusive(['。', '！', '？', '!', '?', '.', '\n']) {
        if estimate_tokens(&format!("{kept}{sentence}")) > max_tokens {
            break;
        }
        kept.push_str(sentence);
    }
    kept.trim().to_string()
}

/// How many estimated tokens of conversation sit outside the history budget
/// without a summary covering them -- what a compaction would fold right now.
/// The scheduler compares this with its batch threshold.
pub fn pending_fold_tokens(
    entries: &[LogEntry],
    summary: Option<&ConversationSummary>,
    limits: &HistoryLimits,
) -> usize {
    let visible = log::context_visible(entries);
    let history = log::entries_to_context(entries);
    let covered = summary
        .filter(|summary| summary.matches(&visible))
        .map_or(0, |summary| summary.covered_through);
    let budget_start = select_history_start(&history, history.len(), limits);
    if budget_start <= covered {
        return 0;
    }
    history[covered..budget_start].iter().map(estimate_message_tokens).sum()
}

/// Folds the turns that have left the history budget into the stored summary.
///
/// The turns to fold are those between what the summary already covers and
/// where the verbatim history now begins when only the budget (not the
/// pending-turn allowance) is counted. On any failure the stored summary is
/// left exactly as it was.
pub async fn run_compaction(
    job: &CompactionJob<'_>,
    writer: &dyn SummaryWriter,
    store: &dyn SummaryStore,
) -> Result<CompactionOutcome, CompactionError> {
    let visible = log::context_visible(job.entries);
    let history = log::entries_to_context(job.entries);

    let existing = store.read(job.session_path).filter(|summary| summary.matches(&visible));
    let covered = existing.as_ref().map_or(0, |summary| summary.covered_through);

    // Where verbatim history begins if every turn counted as already summarized:
    // only the budget.
    let budget_start = select_history_start(&history, history.len(), &job.limits);
    if budget_start <= covered {
        return Ok(CompactionOutcome::NothingToFold);
    }

    // Newest-first until the fold input cap, so a huge backlog reads its recent
    // part.
    let mut first = budget_start;
    let mut tokens = 0usize;
    for index in (covered..budget_start).rev() {
        let cost = estimate_message_tokens(&history[index]);
        if tokens + cost > FOLD_INPUT_MAX_TOKENS && first < budget_start {
            break;
        }
        tokens += cost;
        first = index;
    }

    let request = SummaryRequest {
        existing: existing.as_ref().map(|summary| summary.text.as_str()),
        newer: &history[first..budget_start],
        language: job.language,
        focus: job.focus,
    };
    let written = writer.write(&request).await?;

    let screened = remove_sensitive_sentences(written.trim());
    let text = truncate_to_tokens(&screened, SUMMARY_MAX_TOKENS);
    if text.trim().is_empty() {
        return Err(CompactionError::Empty);
    }

    let summary = ConversationSummary {
        covered_through: budget_start,
        text,
        updated_at: chrono::Utc::now().to_rfc3339(),
        last_covered_timestamp: Some(visible[budget_start - 1].timestamp.clone()),
    };
    store
        .write(job.session_path, &summary)
        .map_err(|err| CompactionError::Store(err.to_string()))?;
    Ok(CompactionOutcome::Updated(summary))
}

/// The messages a compaction that moved the summary from `before_covered` to
/// `summary.covered_through` read, for work that should look at the same turns
/// (memory extraction). Newest first up to the same input cap the summary
/// writer used, returned oldest first.
pub fn folded_messages(
    entries: &[LogEntry],
    before_covered: usize,
    summary: &ConversationSummary,
) -> Vec<Message> {
    let history = log::entries_to_context(entries);
    let end = summary.covered_through.min(history.len());
    let mut first = end;
    let mut tokens = 0usize;
    for index in (before_covered.min(end)..end).rev() {
        let cost = estimate_message_tokens(&history[index]);
        if tokens + cost > FOLD_INPUT_MAX_TOKENS && first < end {
            break;
        }
        tokens += cost;
        first = index;
    }
    history[first..end].to_vec()
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf, sync::Mutex};

    use super::*;
    use crate::log::LogRole;

    #[derive(Default)]
    struct MemoryStore {
        summaries: Mutex<HashMap<PathBuf, ConversationSummary>>,
    }

    impl SummaryStore for MemoryStore {
        fn read(&self, session_path: &Path) -> Option<ConversationSummary> {
            self.summaries.lock().unwrap().get(session_path).cloned()
        }

        fn write(&self, session_path: &Path, summary: &ConversationSummary) -> std::io::Result<()> {
            self.summaries.lock().unwrap().insert(session_path.to_path_buf(), summary.clone());
            Ok(())
        }
    }

    struct Seen {
        existing: Option<String>,
        newer: usize,
        focus: Option<String>,
        newer_tokens: usize,
    }

    struct ScriptedWriter {
        replies: Mutex<Vec<Result<String, CompactionError>>>,
        seen: Mutex<Vec<Seen>>,
    }

    impl ScriptedWriter {
        fn new(replies: Vec<Result<String, CompactionError>>) -> Self {
            Self { replies: Mutex::new(replies), seen: Mutex::new(Vec::new()) }
        }
    }

    #[async_trait]
    impl SummaryWriter for ScriptedWriter {
        async fn write(&self, request: &SummaryRequest<'_>) -> Result<String, CompactionError> {
            self.seen.lock().unwrap().push(Seen {
                existing: request.existing.map(str::to_string),
                newer: request.newer.len(),
                focus: request.focus.map(str::to_string),
                newer_tokens: crate::token_estimate::estimate_messages_tokens(request.newer),
            });
            self.replies.lock().unwrap().remove(0)
        }
    }

    fn transcript(count: usize, words: usize) -> Vec<LogEntry> {
        (0..count)
            .map(|i| LogEntry {
                role: if i % 2 == 0 { LogRole::User } else { LogRole::Assistant },
                content: format!("entry {i} {}", "word ".repeat(words)),
                timestamp: format!("t{i}"),
            })
            .collect()
    }

    fn job<'a>(entries: &'a [LogEntry], focus: Option<&'a str>) -> CompactionJob<'a> {
        CompactionJob {
            session_path: Path::new("session.jsonl"),
            entries,
            language: Language::En,
            limits: HistoryLimits::default(),
            focus,
        }
    }

    #[tokio::test]
    async fn a_successful_compaction_advances_the_summary_to_the_budget_boundary() {
        let entries = transcript(60, 150);
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![Ok("The user has a cat named Mochi.".into())]);

        let outcome = run_compaction(&job(&entries, None), &writer, &store).await.unwrap();

        let CompactionOutcome::Updated(summary) = outcome else { panic!("expected an update") };
        let history = log::entries_to_context(&entries);
        let budget_start = select_history_start(&history, history.len(), &HistoryLimits::default());
        assert_eq!(summary.covered_through, budget_start);
        assert_eq!(
            summary.last_covered_timestamp.as_deref(),
            Some(format!("t{}", budget_start - 1).as_str())
        );
        assert_eq!(store.read(Path::new("session.jsonl")), Some(summary));
    }

    #[test]
    fn pending_tokens_count_what_a_compaction_would_fold() {
        let entries = transcript(60, 150);
        let limits = HistoryLimits::default();
        let pending = pending_fold_tokens(&entries, None, &limits);
        assert!(pending > 0);

        let history = log::entries_to_context(&entries);
        let budget_start = select_history_start(&history, history.len(), &limits);
        let covered_all = ConversationSummary {
            covered_through: budget_start,
            text: "x".into(),
            updated_at: "now".into(),
            last_covered_timestamp: Some(format!("t{}", budget_start - 1)),
        };
        assert_eq!(pending_fold_tokens(&entries, Some(&covered_all), &limits), 0);
    }

    #[test]
    fn a_short_conversation_has_nothing_pending() {
        assert_eq!(pending_fold_tokens(&transcript(6, 5), None, &HistoryLimits::default()), 0);
    }

    #[tokio::test]
    async fn a_failed_writer_leaves_the_stored_summary_untouched() {
        let entries = transcript(60, 150);
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![Err(CompactionError::Writer("unreachable".into()))]);

        let result = run_compaction(&job(&entries, None), &writer, &store).await;

        assert_eq!(result, Err(CompactionError::Writer("unreachable".into())));
        assert_eq!(store.read(Path::new("session.jsonl")), None);
    }

    #[tokio::test]
    async fn an_empty_reply_is_discarded_and_changes_nothing() {
        let entries = transcript(60, 150);
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![Ok("   \n".into())]);

        let result = run_compaction(&job(&entries, None), &writer, &store).await;

        assert_eq!(result, Err(CompactionError::Empty));
        assert_eq!(store.read(Path::new("session.jsonl")), None);
    }

    #[tokio::test]
    async fn a_later_run_covers_the_same_slice_after_an_earlier_failure() {
        let entries = transcript(60, 150);
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![
            Err(CompactionError::Writer("down".into())),
            Ok("Recovered notes.".into()),
        ]);

        let _ = run_compaction(&job(&entries, None), &writer, &store).await;
        let second = run_compaction(&job(&entries, None), &writer, &store).await.unwrap();

        assert!(matches!(second, CompactionOutcome::Updated(_)));
        let seen = writer.seen.lock().unwrap();
        assert_eq!(seen[0].newer, seen[1].newer, "both attempts folded the same turns");
    }

    #[tokio::test]
    async fn the_existing_notes_are_merged_and_only_newer_turns_are_sent() {
        let entries = transcript(80, 150);
        let store = MemoryStore::default();
        let writer =
            ScriptedWriter::new(vec![Ok("First notes.".into()), Ok("Merged notes.".into())]);

        run_compaction(&job(&entries, None), &writer, &store).await.unwrap();
        let more = transcript(100, 150);
        run_compaction(&job(&more, None), &writer, &store).await.unwrap();

        let seen = writer.seen.lock().unwrap();
        assert_eq!(seen[0].existing, None);
        assert_eq!(seen[1].existing.as_deref(), Some("First notes."));
        assert!(seen[1].newer < seen[0].newer + 20, "the second run folds only what is new");
    }

    #[tokio::test]
    async fn nothing_is_folded_while_the_history_fits_the_budget() {
        let entries = transcript(6, 5);
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![]);

        let outcome = run_compaction(&job(&entries, None), &writer, &store).await.unwrap();

        assert_eq!(outcome, CompactionOutcome::NothingToFold);
        assert!(writer.seen.lock().unwrap().is_empty(), "the writer is not called");
    }

    #[tokio::test]
    async fn a_second_run_with_nothing_new_folds_nothing() {
        let entries = transcript(60, 150);
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![Ok("Notes.".into())]);

        run_compaction(&job(&entries, None), &writer, &store).await.unwrap();
        let again = run_compaction(&job(&entries, None), &writer, &store).await.unwrap();

        assert_eq!(again, CompactionOutcome::NothingToFold);
    }

    #[tokio::test]
    async fn the_fold_input_is_capped_but_the_summary_still_advances_over_the_backlog() {
        let entries = transcript(400, 150);
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![Ok("Notes.".into())]);

        let outcome = run_compaction(&job(&entries, None), &writer, &store).await.unwrap();

        let seen = writer.seen.lock().unwrap();
        assert!(
            seen[0].newer_tokens <= FOLD_INPUT_MAX_TOKENS + 300,
            "got {}",
            seen[0].newer_tokens
        );
        let CompactionOutcome::Updated(summary) = outcome else { panic!() };
        assert!(
            summary.covered_through > seen[0].newer,
            "the older backlog is skipped, not re-read"
        );
    }

    #[tokio::test]
    async fn a_sentence_about_medication_is_not_stored() {
        let entries = transcript(60, 150);
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![Ok("The user has a cat named Mochi. They take \
                                                  medication for anxiety. They plan a trip."
            .into())]);

        run_compaction(&job(&entries, None), &writer, &store).await.unwrap();

        let stored = store.read(Path::new("session.jsonl")).unwrap();
        assert!(!stored.text.contains("medication"));
        assert!(stored.text.contains("Mochi") && stored.text.contains("trip"));
    }

    #[tokio::test]
    async fn a_reply_that_is_entirely_sensitive_counts_as_empty() {
        let entries = transcript(60, 150);
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![Ok("Takes medication daily.".into())]);

        let result = run_compaction(&job(&entries, None), &writer, &store).await;

        assert_eq!(result, Err(CompactionError::Empty));
    }

    #[tokio::test]
    async fn an_overlong_summary_is_cut_back_at_a_sentence_boundary() {
        let entries = transcript(60, 150);
        let store = MemoryStore::default();
        let long = "The user mentioned a plan. ".repeat(400);
        let writer = ScriptedWriter::new(vec![Ok(long)]);

        run_compaction(&job(&entries, None), &writer, &store).await.unwrap();

        let stored = store.read(Path::new("session.jsonl")).unwrap();
        assert!(estimate_tokens(&stored.text) <= SUMMARY_MAX_TOKENS);
        assert!(stored.text.ends_with('.'), "cut at a sentence end, not mid-word");
    }

    #[tokio::test]
    async fn an_error_entry_does_not_shift_what_the_summary_covers() {
        let mut entries = transcript(60, 150);
        entries.insert(
            3,
            LogEntry {
                role: LogRole::Error,
                content: "request failed".into(),
                timestamp: "terr".into(),
            },
        );
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![Ok("Notes.".into())]);

        let outcome = run_compaction(&job(&entries, None), &writer, &store).await.unwrap();

        let CompactionOutcome::Updated(summary) = outcome else { panic!() };
        let visible = log::context_visible(&entries);
        assert!(summary.matches(&visible));
        assert_ne!(summary.last_covered_timestamp.as_deref(), Some("terr"));
    }

    #[tokio::test]
    async fn the_requested_focus_reaches_the_writer() {
        let entries = transcript(60, 150);
        let store = MemoryStore::default();
        let writer = ScriptedWriter::new(vec![Ok("Notes.".into())]);

        run_compaction(&job(&entries, Some("the trip dates")), &writer, &store).await.unwrap();

        assert_eq!(writer.seen.lock().unwrap()[0].focus.as_deref(), Some("the trip dates"));
    }

    #[test]
    fn the_prompt_asks_for_neutral_notes_in_the_conversations_language() {
        let prompt = summary_system_prompt(Language::ZhHant);
        assert!(prompt.contains("Traditional Chinese (繁體中文)"));
        assert!(prompt.contains("Never write in the assistant's voice"));
        assert!(prompt.contains("merge"));
        assert!(prompt.contains("At most 150 words"));
        assert!(prompt.contains("health, medication"));
    }

    #[test]
    fn the_user_message_carries_existing_notes_newer_turns_and_the_focus() {
        let newer = vec![Message::user("I got a cat"), Message::assistant("Congratulations")];
        let text = summary_user_message(Some("Likes tea."), &newer, Some("the cat's name"));
        assert!(text.contains("Existing notes:\nLikes tea."));
        assert!(text.contains("user: I got a cat"));
        assert!(text.contains("assistant: Congratulations"));
        assert!(text.contains("Make sure the notes preserve: the cat's name"));
        let none = summary_user_message(None, &newer, None);
        assert!(none.contains("Existing notes:\n(none)"));
        assert!(!none.contains("Make sure the notes preserve"));
    }

    #[test]
    fn the_folded_messages_are_the_turns_between_the_old_and_new_coverage() {
        let entries: Vec<LogEntry> = (0..6)
            .map(|i| LogEntry {
                role: if i % 2 == 0 { LogRole::User } else { LogRole::Assistant },
                content: format!("turn {i}"),
                timestamp: format!("t{i}"),
            })
            .collect();
        let summary = ConversationSummary {
            covered_through: 5,
            text: "notes".into(),
            updated_at: "now".into(),
            last_covered_timestamp: Some("t4".into()),
        };
        let folded = folded_messages(&entries, 2, &summary);
        let texts: Vec<&str> = folded.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(texts, vec!["turn 2", "turn 3", "turn 4"]);
        assert!(folded_messages(&entries, 5, &summary).is_empty());
    }
}
