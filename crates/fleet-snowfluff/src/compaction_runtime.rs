//! The app side of conversation compaction: the runner that performs one, the
//! ticker that asks the scheduler what is due, and the small hooks the chat
//! code calls as replies start and finish.
//!
//! Everything here is best effort and off the hot path. A compaction never
//! delays a reply, and one that fails or cannot run (AI disabled, no usable
//! profile, a disclosure not yet acknowledged) is simply skipped: the
//! transcript on disk is untouched, so a later attempt covers the same turns.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use fleet_snowfluff_ai::{
    pending_fold_tokens, run_compaction, AiSettings, CompactionError, CompactionJob,
    CompactionOutcome, CompactionRequester, CompactionRunner, CompactionScheduler,
    FileSummaryStore, HistoryLimits, ProviderCredentials, ProviderSummaryWriter, SummaryStore,
    Trigger,
};
use tauri::{AppHandle, Manager};

use crate::{
    ai_commands, chat_commands, chat_log_store, memory_commands, memory_runtime, persona_store,
};

/// How often the scheduler is asked whether anything is due.
const TICK_INTERVAL: Duration = Duration::from_secs(5);

/// Performs one compaction of `session_path` with whatever profile does
/// background work.
///
/// `Ok(None)` means it could not run for a reason only the user can change (AI
/// is off, no usable profile, a cloud disclosure not yet acknowledged); that is
/// not an error and is not retried.
async fn compact_session(
    app: &AppHandle,
    session_path: &Path,
    limits: HistoryLimits,
    focus: Option<&str>,
) -> Result<Option<(CompactionOutcome, Vec<fleet_snowfluff_ai::LogEntry>)>, CompactionError> {
    let (settings, credentials) = {
        let settings = app.state::<Mutex<AiSettings>>().lock().unwrap().clone();
        let credentials = app.state::<Mutex<ProviderCredentials>>().lock().unwrap().clone();
        (settings, credentials)
    };
    if !settings.ai_enabled {
        return Ok(None);
    }
    let Some(profile) = settings.background_profile().cloned() else { return Ok(None) };
    if !ai_commands::disclosure_ok(&settings, Some(profile.key())) {
        return Ok(None);
    }

    let entries = chat_log_store::read_session(session_path);
    let covered_before = FileSummaryStore
        .read(session_path)
        .filter(|summary| summary.matches(&fleet_snowfluff_ai::log::context_visible(&entries)))
        .map_or(0, |summary| summary.covered_through);
    // Built fresh with no session to resume: a CLI-backed profile gets its own
    // throwaway session, and its id is never stored, so the chat's own CLI
    // session is untouched.
    let provider = ai_commands::build_provider(
        &credentials,
        &profile,
        None,
        &settings.claude_code_tool_access,
    );
    let writer = ProviderSummaryWriter::new(provider);
    let job = CompactionJob {
        session_path,
        entries: &entries,
        language: chat_commands::conversation_language(app),
        limits,
        focus,
    };
    log::debug!("compacting {session_path:?} using {:?}", profile.key());
    let outcome = run_compaction(&job, &writer, &FileSummaryStore).await?;
    if let CompactionOutcome::Updated(summary) = &outcome {
        extract_memories(app, session_path, &entries, covered_before, summary);
    }
    Ok(Some((outcome, entries)))
}

/// After a compaction, derives memories from the turns it folded. Best effort
/// and in the background: the summary is already stored, so a failure here
/// changes nothing about it.
fn extract_memories(
    app: &AppHandle,
    session_path: &Path,
    entries: &[fleet_snowfluff_ai::LogEntry],
    covered_before: usize,
    summary: &fleet_snowfluff_ai::ConversationSummary,
) {
    let Some(service) = memory_runtime::service_if_enabled(app) else { return };
    let folded = fleet_snowfluff_ai::compaction::folded_messages(entries, covered_before, summary);
    if folded.is_empty() {
        return;
    }
    let source = format!(
        "{}:{covered_before}-{}",
        session_path.file_name().and_then(|name| name.to_str()).unwrap_or("session"),
        summary.covered_through
    );
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(model) = memory_runtime::background_model(&app) else { return };
        let persona = persona_store::load(&app).persona.name;
        match fleet_snowfluff_ai::memory::extraction::extract_after_compaction(
            &model,
            service.as_ref(),
            &folded,
            &source,
            Some(&persona),
        )
        .await
        {
            Ok(0) => {}
            Ok(stored) => {
                log::debug!("extracted {stored} memorie(s) from {source}");
                memory_commands::notify_changed(&app);
            }
            Err(error) => log::debug!("memory extraction skipped for {source}: {error}"),
        }
    });
}

pub struct AppCompactionRunner {
    app: AppHandle,
}

impl AppCompactionRunner {
    pub fn new(app: AppHandle) -> Self { Self { app } }
}

#[async_trait]
impl CompactionRunner for AppCompactionRunner {
    async fn run(
        &self,
        session_path: &Path,
        trigger: Trigger,
        limits: HistoryLimits,
    ) -> Result<usize, CompactionError> {
        log::debug!("compaction triggered ({trigger:?}) for {session_path:?}");
        // Reporting zero remaining when it cannot run keeps the scheduler from retrying
        // every few seconds for a condition that only the user can change.
        match compact_session(&self.app, session_path, limits, None).await? {
            Some((CompactionOutcome::Updated(summary), entries)) => {
                Ok(pending_fold_tokens(&entries, Some(&summary), &limits))
            }
            Some((CompactionOutcome::NothingToFold, _)) | None => Ok(0),
        }
    }
}

/// The `compact_context` tool's way to compact the open conversation right now.
pub struct AppCompactionRequester {
    app: AppHandle,
}

impl AppCompactionRequester {
    pub fn new(app: AppHandle) -> Self { Self { app } }
}

#[async_trait]
impl CompactionRequester for AppCompactionRequester {
    async fn compact(
        &self,
        session_path: &Path,
        focus: Option<&str>,
    ) -> Result<CompactionOutcome, CompactionError> {
        let limits = scheduler(&self.app)
            .map_or_else(HistoryLimits::default, |scheduler| scheduler.limits_for(session_path));
        match compact_session(&self.app, session_path, limits, focus).await? {
            Some((outcome, _)) => Ok(outcome),
            None => Err(CompactionError::Writer(
                "no model is available for background work".to_string(),
            )),
        }
    }
}

/// Creates the scheduler, registers it as managed state, and starts the ticker.
pub fn start(app: &AppHandle) {
    let runner = Arc::new(AppCompactionRunner::new(app.clone()));
    let scheduler = Arc::new(CompactionScheduler::new(runner));
    app.manage(Arc::clone(&scheduler));
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            // The handles are dropped: each compaction runs on its own task.
            drop(scheduler.tick());
        }
    });
}

fn scheduler(app: &AppHandle) -> Option<Arc<CompactionScheduler>> {
    app.try_state::<Arc<CompactionScheduler>>().map(|state| state.inner().clone())
}

/// A reply is about to be generated for `session_path` using `limits`.
pub fn reply_started(app: &AppHandle, session_path: &Path, limits: HistoryLimits) {
    if let Some(scheduler) = scheduler(app) {
        scheduler.note_reply_started(session_path, limits);
    }
}

/// The latest request for `session_path` carried only `kept_history_tokens` of
/// history because the context window was small. Later compactions fold what
/// that left out.
pub fn history_was_trimmed(app: &AppHandle, session_path: &Path, kept_history_tokens: usize) {
    if let Some(scheduler) = scheduler(app) {
        let defaults = HistoryLimits::default();
        scheduler.note_limits(
            session_path,
            HistoryLimits {
                budget_tokens: kept_history_tokens,
                uncovered_extra_tokens: 0,
                max_messages: defaults.max_messages,
            },
        );
    }
}

/// A reply for `session_path` finished, failed, or was stopped. Works out how
/// much content now sits outside the history budget without a summary and tells
/// the scheduler.
pub fn reply_finished_for(app: &AppHandle, session_path: &Path) {
    let Some(scheduler) = scheduler(app) else { return };
    let entries = chat_log_store::read_session(session_path);
    let summary = FileSummaryStore.read(session_path);
    let limits = scheduler.limits_for(session_path);
    let pending = pending_fold_tokens(&entries, summary.as_ref(), &limits);
    scheduler.note_reply_finished(session_path, pending);
}

/// [`reply_finished_for`] the conversation that is currently open.
pub fn reply_finished(app: &AppHandle) {
    if let Some(session_path) =
        app.state::<chat_commands::ChatRuntimeState>().current_session_path()
    {
        reply_finished_for(app, &session_path);
    }
}

/// The chat window was closed.
pub fn window_closed(app: &AppHandle) {
    let Some(scheduler) = scheduler(app) else { return };
    if let Some(session_path) =
        app.state::<chat_commands::ChatRuntimeState>().current_session_path()
    {
        scheduler.note_window_closed(&session_path);
    }
}
