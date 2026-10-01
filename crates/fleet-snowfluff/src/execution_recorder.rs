//! `ExecutionRecorder`: an RAII guard that persists one `Execution`'s
//! start and end to the conversation's execution log
//! (`execution-log-and-context`, design.md's Decision 5).
//!
//! Constructed right before a provider is ever called -- writing the
//! `start` event immediately -- and marked with its outcome right
//! before whichever `run_generation*` function owns it returns.
//! `Drop` writes the `end` event using whatever was last marked,
//! defaulting to `Cancelled` if nothing was. This covers Stop and an
//! unexpected process exit uniformly: `tokio::task::JoinHandle::abort()`
//! drops the task at its next `.await` point, so the task's own code
//! never resumes to mark anything, and a crash looks identical from
//! here -- both are "dropped without being told it finished," which is
//! fine, since recovering from either looks the same (start fresh,
//! seeded from history).

use std::path::PathBuf;

use fleet_snowfluff_ai::{
    ConversationId, ExecutionEnd, ExecutionEvent, ExecutionId, ExecutionPath, ExecutionStart,
    ExecutionStatus, ProfileKey, ToolInvocation,
};

use crate::execution_log_store;

enum Outcome {
    Completed {
        external_ref: Option<String>,
        seen_turns: Option<usize>,
        trace: Vec<ToolInvocation>,
    },
    Errored {
        external_ref: Option<String>,
        seen_turns: Option<usize>,
        trace: Vec<ToolInvocation>,
    },
    Escalated,
}

pub struct ExecutionRecorder {
    session_path: PathBuf,
    id: ExecutionId,
    outcome: Option<Outcome>,
}

impl ExecutionRecorder {
    /// Writes the `start` event immediately and returns a guard that
    /// will write the matching `end` event when dropped.
    pub fn start(
        session_path: PathBuf,
        conversation_id: ConversationId,
        route: ExecutionPath,
        profile: ProfileKey,
        working_dir: PathBuf,
    ) -> Self {
        let id = ExecutionId::new();
        execution_log_store::append(
            &session_path,
            &ExecutionEvent::Start(ExecutionStart {
                id,
                conversation_id,
                route,
                profile,
                working_dir,
                started_at: now_rfc3339(),
            }),
        );
        Self { session_path, id, outcome: None }
    }

    /// A completed turn's `external_ref`/`seen_turns` are `None` for any
    /// provider without a resumable session concept (every provider but
    /// the CLI-backed ones); `trace` is the agent loop's own tool-call
    /// trace, empty for a plain `chat()` attempt.
    pub fn mark_completed(
        &mut self,
        external_ref: Option<String>,
        seen_turns: Option<usize>,
        trace: Vec<ToolInvocation>,
    ) {
        self.outcome = Some(Outcome::Completed { external_ref, seen_turns, trace });
    }

    /// Mirrors `mark_completed`, for a turn that errored. A CLI-backed
    /// provider can still have captured a real session id right before
    /// failing (confirmed shipped behavior this must not regress); its
    /// `seen_turns` is the catch-up cursor the session already had
    /// *before* this turn (`TurnCtx::prior_seen_turns`), since an
    /// errored turn's own effect on what the session absorbed is
    /// unknown -- carrying the prior value forward is strictly more
    /// accurate than resetting it, which would only cost a redundant
    /// history resend on the next resume, never a correctness bug.
    pub fn mark_errored(
        &mut self,
        external_ref: Option<String>,
        seen_turns: Option<usize>,
        trace: Vec<ToolInvocation>,
    ) {
        self.outcome = Some(Outcome::Errored { external_ref, seen_turns, trace });
    }

    /// For a `mix`-mode local attempt that handed off by its own content
    /// decision (the `<<ESCALATE>>` marker) -- never for an infra-level
    /// failure, which is `mark_errored` instead (see
    /// `fleet_snowfluff_ai::ExecutionStatus::Escalated`'s own doc comment
    /// for why these are kept distinct).
    pub fn mark_escalated(&mut self) { self.outcome = Some(Outcome::Escalated); }
}

impl Drop for ExecutionRecorder {
    fn drop(&mut self) {
        let (status, external_ref, seen_turns, trace) = match self.outcome.take() {
            Some(Outcome::Completed { external_ref, seen_turns, trace }) => {
                (ExecutionStatus::Completed, external_ref, seen_turns, trace)
            }
            Some(Outcome::Errored { external_ref, seen_turns, trace }) => {
                (ExecutionStatus::Errored, external_ref, seen_turns, trace)
            }
            Some(Outcome::Escalated) => (ExecutionStatus::Escalated, None, None, Vec::new()),
            None => (ExecutionStatus::Cancelled, None, None, Vec::new()),
        };
        execution_log_store::append(
            &self.session_path,
            &ExecutionEvent::End(ExecutionEnd {
                id: self.id,
                status,
                external_ref,
                seen_turns,
                trace,
                ended_at: now_rfc3339(),
            }),
        );
    }
}

fn now_rfc3339() -> String { chrono::Utc::now().to_rfc3339() }

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use fleet_snowfluff_ai::{AuthMethod, ProviderKind};

    use super::*;

    fn temp_log(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-execution-recorder-test-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("20260924T120000Z_abc123.jsonl")
    }

    fn profile() -> ProfileKey {
        ProfileKey { provider: ProviderKind::Anthropic, auth_method: AuthMethod::Subscription }
    }

    fn new_recorder(log: &Path) -> ExecutionRecorder {
        ExecutionRecorder::start(
            log.to_path_buf(),
            fleet_snowfluff_ai::ConversationId::from_session_path(log),
            ExecutionPath::Direct,
            profile(),
            PathBuf::from("/proj"),
        )
    }

    #[test]
    fn constructing_a_recorder_writes_the_start_event_immediately() {
        let log = temp_log("start");
        let _recorder = new_recorder(&log);
        let executions = execution_log_store::read(&log);
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].end, None, "no end yet -- only start was written");
    }

    #[test]
    fn dropping_without_marking_anything_records_cancelled() {
        let log = temp_log("cancelled");
        let recorder = new_recorder(&log);
        drop(recorder);

        let executions = execution_log_store::read(&log);
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].end.as_ref().unwrap().status, ExecutionStatus::Cancelled);
    }

    #[test]
    fn mark_completed_is_recorded_with_its_fields() {
        let log = temp_log("completed");
        let mut recorder = new_recorder(&log);
        recorder.mark_completed(Some("sess-1".to_string()), Some(4), vec![]);
        drop(recorder);

        let end = execution_log_store::read(&log).remove(0).end.unwrap();
        assert_eq!(end.status, ExecutionStatus::Completed);
        assert_eq!(end.external_ref.as_deref(), Some("sess-1"));
        assert_eq!(end.seen_turns, Some(4));
    }

    #[test]
    fn mark_errored_is_recorded_with_its_fields() {
        let log = temp_log("errored");
        let mut recorder = new_recorder(&log);
        recorder.mark_errored(Some("sess-1".to_string()), Some(2), vec![]);
        drop(recorder);

        let end = execution_log_store::read(&log).remove(0).end.unwrap();
        assert_eq!(end.status, ExecutionStatus::Errored);
        assert_eq!(end.external_ref.as_deref(), Some("sess-1"));
        assert_eq!(end.seen_turns, Some(2));
    }

    #[test]
    fn mark_escalated_is_recorded_with_no_external_ref_or_trace() {
        let log = temp_log("escalated");
        let mut recorder = new_recorder(&log);
        recorder.mark_escalated();
        drop(recorder);

        let end = execution_log_store::read(&log).remove(0).end.unwrap();
        assert_eq!(end.status, ExecutionStatus::Escalated);
        assert_eq!(end.external_ref, None);
        assert!(end.trace.is_empty());
    }
}
