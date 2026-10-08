//! When to run a compaction (`context-compaction`'s "Compaction triggers").
//!
//! Summarizing after every reply would cost a model call per message, which
//! matters for a CLI profile's quota and for a local model that would be busy
//! exactly when the user sends the next message. So compaction is batched: it
//! waits for enough unsummarized content to build up and for the conversation
//! to go quiet, and never starts while a reply is being generated. Two other
//! triggers run it regardless of size: the conversation going idle, and its
//! chat window closing.
//!
//! The decision itself ([`decide`]) is a pure function of a session's state and
//! the time, so every rule here is tested with a synthetic clock rather than by
//! waiting. [`CompactionScheduler`] holds the per-session state, and starts a
//! [`CompactionRunner`] for the sessions that are due.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;

use crate::{compaction::CompactionError, prompt::HistoryLimits};

/// Unsummarized tokens that must build up before a batched compaction.
pub const BATCH_THRESHOLD_TOKENS: usize = 1_200;
/// How long the conversation must have been quiet before a batched compaction
/// starts.
pub const QUIET_PERIOD: Duration = Duration::from_secs(20);
/// How long without activity counts as idle.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The first retry delay after a failed compaction; it doubles with each
/// further failure.
pub const BACKOFF_BASE: Duration = Duration::from_secs(60);
/// The longest retry delay.
pub const BACKOFF_MAX: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerConfig {
    pub batch_threshold_tokens: usize,
    pub quiet_period: Duration,
    pub idle_timeout: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            batch_threshold_tokens: BATCH_THRESHOLD_TOKENS,
            quiet_period: QUIET_PERIOD,
            idle_timeout: IDLE_TIMEOUT,
            backoff_base: BACKOFF_BASE,
            backoff_max: BACKOFF_MAX,
        }
    }
}

/// Why a compaction was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Enough unsummarized content, after a quiet moment.
    Batch,
    /// The conversation has been idle for a long time.
    Idle,
    /// The chat window was closed.
    Close,
}

/// What the scheduler knows about one conversation.
#[derive(Debug, Clone)]
pub struct SessionState {
    /// The history limits the most recent request actually used, so a
    /// compaction folds exactly the turns that request left out.
    pub limits: HistoryLimits,
    /// Tokens of turns outside the budget that no summary covers yet.
    pub uncovered_tokens: usize,
    pub last_activity: Instant,
    pub reply_in_flight: bool,
    pub compaction_in_flight: bool,
    pub failures: u32,
    pub last_failure: Option<Instant>,
    pub close_pending: bool,
    pub idle_handled: bool,
}

impl SessionState {
    fn new(now: Instant) -> Self {
        Self {
            limits: HistoryLimits::default(),
            uncovered_tokens: 0,
            last_activity: now,
            reply_in_flight: false,
            compaction_in_flight: false,
            failures: 0,
            last_failure: None,
            close_pending: false,
            idle_handled: false,
        }
    }
}

fn backoff(config: &SchedulerConfig, failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    config.backoff_base.saturating_mul(1u32 << doublings).min(config.backoff_max)
}

/// Whether a compaction is due for `state` at `now`, and why.
pub fn decide(state: &SessionState, now: Instant, config: &SchedulerConfig) -> Option<Trigger> {
    if state.compaction_in_flight || state.reply_in_flight || state.uncovered_tokens == 0 {
        return None;
    }
    if let Some(failed_at) = state.last_failure {
        if state.failures > 0 && now < failed_at + backoff(config, state.failures) {
            return None;
        }
    }
    if state.close_pending {
        return Some(Trigger::Close);
    }
    let quiet_for = now.saturating_duration_since(state.last_activity);
    if quiet_for >= config.idle_timeout && !state.idle_handled {
        return Some(Trigger::Idle);
    }
    if state.uncovered_tokens >= config.batch_threshold_tokens && quiet_for >= config.quiet_period {
        return Some(Trigger::Batch);
    }
    None
}

pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant { Instant::now() }
}

/// Performs one compaction and reports how many tokens remain unsummarized
/// afterwards.
#[async_trait]
pub trait CompactionRunner: Send + Sync {
    async fn run(
        &self,
        session_path: &Path,
        trigger: Trigger,
        limits: HistoryLimits,
    ) -> Result<usize, CompactionError>;
}

/// Tracks every open conversation and starts compactions when they are due. The
/// `note_*` methods only take a short lock and return, so recording activity
/// never waits on a compaction.
pub struct CompactionScheduler {
    sessions: Mutex<HashMap<PathBuf, SessionState>>,
    config: SchedulerConfig,
    clock: Arc<dyn Clock>,
    runner: Arc<dyn CompactionRunner>,
}

impl CompactionScheduler {
    pub fn new(runner: Arc<dyn CompactionRunner>) -> Self {
        Self::with(runner, Arc::new(SystemClock), SchedulerConfig::default())
    }

    pub fn with(
        runner: Arc<dyn CompactionRunner>,
        clock: Arc<dyn Clock>,
        config: SchedulerConfig,
    ) -> Self {
        Self { sessions: Mutex::new(HashMap::new()), config, clock, runner }
    }

    fn update(&self, session_path: &Path, change: impl FnOnce(&mut SessionState, Instant)) {
        let now = self.clock.now();
        let mut sessions = self.sessions.lock().unwrap();
        let state =
            sessions.entry(session_path.to_path_buf()).or_insert_with(|| SessionState::new(now));
        change(state, now);
    }

    /// The user sent a message and a reply is about to be generated. Records
    /// the limits that request used, so a later compaction folds exactly
    /// what it left out.
    pub fn note_reply_started(&self, session_path: &Path, limits: HistoryLimits) {
        self.update(session_path, |state, now| {
            state.limits = limits;
            state.reply_in_flight = true;
            state.last_activity = now;
            state.idle_handled = false;
            state.close_pending = false;
        });
    }

    /// The history limits the latest request for this conversation used (the
    /// defaults before any).
    pub fn limits_for(&self, session_path: &Path) -> HistoryLimits {
        self.sessions
            .lock()
            .unwrap()
            .get(session_path)
            .map_or_else(HistoryLimits::default, |state| state.limits)
    }

    /// The history this conversation's latest request actually carried was
    /// smaller than the usual limits, because the fit check had to shed
    /// some. A compaction must then fold what that request left out, or the
    /// dropped turns would be neither sent nor summarized.
    pub fn note_limits(&self, session_path: &Path, limits: HistoryLimits) {
        self.update(session_path, |state, _| state.limits = limits);
    }

    /// The reply finished (or failed). `uncovered_tokens` is how much content
    /// now sits outside the budget without a summary.
    pub fn note_reply_finished(&self, session_path: &Path, uncovered_tokens: usize) {
        self.update(session_path, |state, now| {
            state.reply_in_flight = false;
            state.uncovered_tokens = uncovered_tokens;
            state.last_activity = now;
        });
    }

    /// The conversation's chat window was closed.
    pub fn note_window_closed(&self, session_path: &Path) {
        self.update(session_path, |state, _| state.close_pending = true);
    }

    /// Asks for a compaction right away, regardless of size or quiet time (the
    /// model asked for one).
    pub fn request_now(&self, session_path: &Path) {
        self.update(session_path, |state, _| state.close_pending = true);
    }

    /// Starts every compaction that is due. Returns a handle for each, so a
    /// test can wait for them.
    pub fn tick(self: &Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let now = self.clock.now();
        let mut due = Vec::new();
        {
            let mut sessions = self.sessions.lock().unwrap();
            for (path, state) in sessions.iter_mut() {
                if let Some(trigger) = decide(state, now, &self.config) {
                    state.compaction_in_flight = true;
                    if trigger == Trigger::Idle {
                        state.idle_handled = true;
                    }
                    due.push((path.clone(), trigger, state.limits));
                }
            }
        }
        due.into_iter()
            .map(|(path, trigger, limits)| {
                let scheduler = Arc::clone(self);
                tokio::spawn(async move {
                    let outcome = scheduler.runner.run(&path, trigger, limits).await;
                    scheduler.update(&path, |state, now| {
                        state.compaction_in_flight = false;
                        match outcome {
                            Ok(remaining) => {
                                state.uncovered_tokens = remaining;
                                state.failures = 0;
                                state.last_failure = None;
                                state.close_pending = false;
                            }
                            Err(_) => {
                                state.failures += 1;
                                state.last_failure = Some(now);
                            }
                        }
                    });
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::*;

    struct FakeClock {
        base: Instant,
        offset: Mutex<Duration>,
    }

    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(Self { base: Instant::now(), offset: Mutex::new(Duration::ZERO) })
        }

        fn advance(&self, by: Duration) { *self.offset.lock().unwrap() += by; }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant { self.base + *self.offset.lock().unwrap() }
    }

    struct RecordingRunner {
        calls: Mutex<Vec<(PathBuf, Trigger, HistoryLimits)>>,
        results: Mutex<Vec<Result<usize, CompactionError>>>,
        gate: Option<Arc<Notify>>,
        started: AtomicUsize,
    }

    impl RecordingRunner {
        fn new(results: Vec<Result<usize, CompactionError>>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(vec![]),
                results: Mutex::new(results),
                gate: None,
                started: AtomicUsize::new(0),
            })
        }

        fn gated(gate: Arc<Notify>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(vec![]),
                results: Mutex::new(vec![]),
                gate: Some(gate),
                started: AtomicUsize::new(0),
            })
        }

        fn call_count(&self) -> usize { self.calls.lock().unwrap().len() }
    }

    #[async_trait]
    impl CompactionRunner for RecordingRunner {
        async fn run(
            &self,
            session_path: &Path,
            trigger: Trigger,
            limits: HistoryLimits,
        ) -> Result<usize, CompactionError> {
            self.calls.lock().unwrap().push((session_path.to_path_buf(), trigger, limits));
            self.started.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                gate.notified().await;
                return Ok(0);
            }
            let mut results = self.results.lock().unwrap();
            if results.is_empty() {
                Ok(0)
            } else {
                results.remove(0)
            }
        }
    }

    fn setup(runner: Arc<RecordingRunner>) -> (Arc<CompactionScheduler>, Arc<FakeClock>) {
        let clock = FakeClock::new();
        let scheduler =
            Arc::new(CompactionScheduler::with(runner, clock.clone(), SchedulerConfig::default()));
        (scheduler, clock)
    }

    async fn run_tick(scheduler: &Arc<CompactionScheduler>) {
        for handle in scheduler.tick() {
            handle.await.unwrap();
        }
    }

    fn session() -> PathBuf { PathBuf::from("session.jsonl") }

    #[tokio::test]
    async fn one_extra_exchange_makes_no_summary_call() {
        let runner = RecordingRunner::new(vec![]);
        let (scheduler, clock) = setup(runner.clone());
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), BATCH_THRESHOLD_TOKENS - 1);
        clock.advance(Duration::from_secs(120));
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 0, "below the threshold nothing runs, however quiet it is");
    }

    #[tokio::test]
    async fn built_up_content_triggers_exactly_one_compaction_after_the_quiet_period() {
        let runner = RecordingRunner::new(vec![Ok(0)]);
        let (scheduler, clock) = setup(runner.clone());
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), BATCH_THRESHOLD_TOKENS);

        clock.advance(QUIET_PERIOD - Duration::from_secs(1));
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 0, "still inside the quiet period");

        clock.advance(Duration::from_secs(2));
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 1);
        assert_eq!(runner.calls.lock().unwrap()[0].1, Trigger::Batch);

        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 1, "nothing is left to fold, so no second call");
    }

    #[tokio::test]
    async fn a_message_during_the_wait_resets_the_quiet_period() {
        let runner = RecordingRunner::new(vec![]);
        let (scheduler, clock) = setup(runner.clone());
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), BATCH_THRESHOLD_TOKENS);
        clock.advance(QUIET_PERIOD - Duration::from_secs(2));

        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), BATCH_THRESHOLD_TOKENS);
        clock.advance(Duration::from_secs(5));
        run_tick(&scheduler).await;

        assert_eq!(runner.call_count(), 0, "the new message restarted the quiet period");
    }

    #[tokio::test]
    async fn nothing_starts_while_a_reply_is_being_generated() {
        let runner = RecordingRunner::new(vec![]);
        let (scheduler, clock) = setup(runner.clone());
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), BATCH_THRESHOLD_TOKENS * 3);
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        clock.advance(Duration::from_secs(300));
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 0);
    }

    #[tokio::test]
    async fn overlapping_triggers_coalesce_into_one_run() {
        let gate = Arc::new(Notify::new());
        let runner = RecordingRunner::gated(gate.clone());
        let (scheduler, clock) = setup(runner.clone());
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), BATCH_THRESHOLD_TOKENS * 2);
        clock.advance(QUIET_PERIOD * 2);

        let first = scheduler.tick();
        tokio::task::yield_now().await;
        let second = scheduler.tick();
        assert_eq!(first.len(), 1);
        assert!(second.is_empty(), "a second tick while one is running starts nothing");
        gate.notify_one();
        for handle in first {
            handle.await.unwrap();
        }
        assert_eq!(runner.call_count(), 1);
    }

    #[tokio::test]
    async fn recording_activity_never_waits_on_a_running_compaction() {
        let gate = Arc::new(Notify::new());
        let runner = RecordingRunner::gated(gate.clone());
        let (scheduler, clock) = setup(runner.clone());
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), BATCH_THRESHOLD_TOKENS * 2);
        clock.advance(QUIET_PERIOD * 2);
        let handles = scheduler.tick();
        tokio::task::yield_now().await;

        // The compaction is still blocked; the user's next message must be recorded at
        // once.
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), 0);

        gate.notify_one();
        for handle in handles {
            handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_failure_backs_off_and_a_success_resets_it() {
        let runner = RecordingRunner::new(vec![
            Err(CompactionError::Writer("down".into())),
            Err(CompactionError::Writer("still down".into())),
            Ok(0),
        ]);
        let (scheduler, clock) = setup(runner.clone());
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), BATCH_THRESHOLD_TOKENS * 2);
        clock.advance(QUIET_PERIOD * 2);

        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 1);

        clock.advance(BACKOFF_BASE - Duration::from_secs(1));
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 1, "still backing off after the first failure");

        clock.advance(Duration::from_secs(2));
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 2, "retried once the first delay passed");

        clock.advance(BACKOFF_BASE + Duration::from_secs(1));
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 2, "the second failure doubled the delay");

        clock.advance(BACKOFF_BASE);
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 3);
    }

    #[test]
    fn the_retry_delay_doubles_up_to_a_ceiling() {
        let config = SchedulerConfig::default();
        assert_eq!(backoff(&config, 1), BACKOFF_BASE);
        assert_eq!(backoff(&config, 2), BACKOFF_BASE * 2);
        assert_eq!(backoff(&config, 3), BACKOFF_BASE * 4);
        assert_eq!(backoff(&config, 40), BACKOFF_MAX);
    }

    #[tokio::test]
    async fn idle_fires_once_even_below_the_batch_threshold() {
        let runner = RecordingRunner::new(vec![Ok(500)]);
        let (scheduler, clock) = setup(runner.clone());
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), 300);

        clock.advance(IDLE_TIMEOUT - Duration::from_secs(1));
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 0);

        clock.advance(Duration::from_secs(2));
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 1);
        assert_eq!(runner.calls.lock().unwrap()[0].1, Trigger::Idle);

        clock.advance(IDLE_TIMEOUT);
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 1, "idle is handled once until there is new activity");
    }

    #[tokio::test]
    async fn closing_the_window_summarizes_immediately_without_a_quiet_period() {
        let runner = RecordingRunner::new(vec![Ok(0)]);
        let (scheduler, _clock) = setup(runner.clone());
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), 100);
        scheduler.note_window_closed(&session());

        run_tick(&scheduler).await;

        assert_eq!(runner.call_count(), 1);
        assert_eq!(runner.calls.lock().unwrap()[0].1, Trigger::Close);
    }

    #[tokio::test]
    async fn closing_a_window_with_nothing_to_fold_does_nothing() {
        let runner = RecordingRunner::new(vec![]);
        let (scheduler, _clock) = setup(runner.clone());
        scheduler.note_reply_started(&session(), HistoryLimits::default());
        scheduler.note_reply_finished(&session(), 0);
        scheduler.note_window_closed(&session());
        run_tick(&scheduler).await;
        assert_eq!(runner.call_count(), 0);
    }

    #[tokio::test]
    async fn the_compaction_uses_the_limits_of_the_latest_request() {
        let runner = RecordingRunner::new(vec![Ok(0)]);
        let (scheduler, clock) = setup(runner.clone());
        let small = HistoryLimits::for_window(Some(4096));
        scheduler.note_reply_started(&session(), small);
        scheduler.note_reply_finished(&session(), BATCH_THRESHOLD_TOKENS * 2);
        clock.advance(QUIET_PERIOD * 2);
        run_tick(&scheduler).await;
        assert_eq!(runner.calls.lock().unwrap()[0].2, small);
    }

    #[tokio::test]
    async fn separate_sessions_are_scheduled_independently() {
        let runner = RecordingRunner::new(vec![Ok(0)]);
        let (scheduler, clock) = setup(runner.clone());
        let (a, b) = (PathBuf::from("a.jsonl"), PathBuf::from("b.jsonl"));
        scheduler.note_reply_started(&a, HistoryLimits::default());
        scheduler.note_reply_finished(&a, BATCH_THRESHOLD_TOKENS * 2);
        scheduler.note_reply_started(&b, HistoryLimits::default());
        scheduler.note_reply_finished(&b, 10);
        clock.advance(QUIET_PERIOD * 2);
        run_tick(&scheduler).await;
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, a);
    }
}
