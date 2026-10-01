//! Execution log storage: append-only JSONL files under
//! `chat-logs/<date>/<timestamp>_<session-id>.executions.jsonl`, one
//! per conversation, replacing `cli-session-continuity`'s
//! `cli_session_store.rs` sidecar (`execution-log-and-context`).
//! `fleet_snowfluff_ai::execution` has the pure event format and the
//! `serialize_event`/`parse_events` functions; this module is the
//! file-I/O and path-naming layer around them, same split as
//! `chat_log_store.rs` around `fleet_snowfluff_ai::log`.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use fleet_snowfluff_ai::{execution, Execution, ExecutionEvent, ProfileKey};

/// `<dir>/<ts>_<id>.jsonl` -> `<dir>/<ts>_<id>.executions.jsonl` --
/// same derivation `cli_session_store::sidecar_path` used for
/// `.sessions.json`.
pub fn log_path(session_path: &Path) -> PathBuf {
    let stem = session_path.file_stem().map(|s| s.to_string_lossy().into_owned());
    let name = format!("{}.executions.jsonl", stem.as_deref().unwrap_or("session"));
    session_path.with_file_name(name)
}

/// Appends one event, creating the file/directory if needed.
/// Best-effort: a write failure is logged, not propagated, matching
/// every other `*_store.rs` save function (`chat_log_store::append_entry`
/// foremost among them).
pub fn append(session_path: &Path, event: &ExecutionEvent) {
    let path = log_path(session_path);
    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            log::error!("failed to create chat-logs dir {dir:?}: {err}");
            return;
        }
    }
    let line = format!("{}\n", execution::serialize_event(event));
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            if let Err(err) = file.write_all(line.as_bytes()) {
                log::error!("failed to append to execution log {path:?}: {err}");
            }
        }
        Err(err) => log::error!("failed to open execution log {path:?}: {err}"),
    }
}

/// Reads and pairs every event for a conversation. A missing file
/// (nothing recorded yet) reads as empty, same as
/// `chat_log_store::read_session`.
pub fn read(session_path: &Path) -> Vec<Execution> {
    let events: Vec<ExecutionEvent> = std::fs::read_to_string(log_path(session_path))
        .map(|s| execution::parse_events(&s))
        .unwrap_or_default();
    execution::pair_events(events)
}

/// The id of the resumable session to use for `(profile, working_dir)`,
/// and how many transcript turns it has already seen, or `None` if
/// there is no usable one. "Usable" means: the latest (by file order)
/// execution for this profile whose status is `Completed` or `Errored`
/// (an `Errored` turn can still have captured a real session id right
/// before it failed -- the existing `store_cli_session_id` error-path
/// behavior this must not regress), carrying an `external_ref`, created
/// under the same `working_dir`. `Escalated` and `Cancelled` are never
/// eligible: `Escalated` only ever applies to the local Ollama attempt
/// in `mix` mode, which never has an `external_ref` to begin with, and
/// a `Cancelled` turn's session state (if any) is unknown.
pub fn latest_usable_session(
    session_path: &Path,
    profile: ProfileKey,
    working_dir: &Path,
) -> Option<(String, usize)> {
    read(session_path)
        .into_iter()
        .filter(|execution| execution.profile == profile && execution.working_dir == working_dir)
        .filter_map(|execution| execution.end)
        .filter(|end| {
            matches!(
                end.status,
                fleet_snowfluff_ai::ExecutionStatus::Completed
                    | fleet_snowfluff_ai::ExecutionStatus::Errored
            )
        })
        .filter_map(|end| end.external_ref.map(|id| (id, end.seen_turns.unwrap_or(0))))
        .next_back()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fleet_snowfluff_ai::{
        AuthMethod, ExecutionEnd, ExecutionId, ExecutionPath, ExecutionStart, ExecutionStatus,
        ProviderKind,
    };

    use super::*;

    fn temp_log(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fleet-snowfluff-execution-log-store-test-{}-{name}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("20260924T120000Z_abc123.jsonl")
    }

    fn profile() -> ProfileKey {
        ProfileKey { provider: ProviderKind::Anthropic, auth_method: AuthMethod::Subscription }
    }

    fn start(id: ExecutionId, cwd: &str) -> ExecutionEvent {
        ExecutionEvent::Start(ExecutionStart {
            id,
            conversation_id: fleet_snowfluff_ai::ConversationId::from_session_path(Path::new(
                "/tmp/x",
            )),
            route: ExecutionPath::Direct,
            profile: profile(),
            working_dir: PathBuf::from(cwd),
            started_at: "t0".to_string(),
        })
    }

    fn end(
        id: ExecutionId,
        status: ExecutionStatus,
        external_ref: Option<&str>,
        seen_turns: Option<usize>,
    ) -> ExecutionEvent {
        ExecutionEvent::End(ExecutionEnd {
            id,
            status,
            external_ref: external_ref.map(str::to_string),
            seen_turns,
            trace: vec![],
            ended_at: "t1".to_string(),
        })
    }

    #[test]
    fn log_path_sits_next_to_the_session_log() {
        let session = Path::new("chat-logs/2026-09-24/20260924T120000Z_abc123.jsonl");
        assert_eq!(
            log_path(session),
            Path::new("chat-logs/2026-09-24/20260924T120000Z_abc123.executions.jsonl")
        );
    }

    #[test]
    fn an_appended_event_round_trips() {
        let log = temp_log("round-trip");
        let id = ExecutionId::new();
        append(&log, &start(id, "/proj"));
        append(&log, &end(id, ExecutionStatus::Completed, Some("sess-1"), Some(4)));

        let executions = read(&log);
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].end.as_ref().unwrap().external_ref.as_deref(), Some("sess-1"));
    }

    #[test]
    fn a_missing_file_reads_as_empty() {
        assert!(read(&temp_log("missing")).is_empty());
    }

    #[test]
    fn a_corrupt_line_is_skipped() {
        let log = temp_log("corrupt");
        std::fs::write(log_path(&log), "{ not json\n").unwrap();
        assert!(read(&log).is_empty());
    }

    // -- latest_usable_session --

    #[test]
    fn a_completed_execution_with_an_external_ref_is_usable() {
        let log = temp_log("completed");
        let id = ExecutionId::new();
        append(&log, &start(id, "/proj"));
        append(&log, &end(id, ExecutionStatus::Completed, Some("sess-1"), Some(4)));

        let found = latest_usable_session(&log, profile(), Path::new("/proj"));
        assert_eq!(found, Some(("sess-1".to_string(), 4)));
    }

    #[test]
    fn an_errored_execution_with_a_captured_ref_is_still_usable() {
        // Regression guard: `store_cli_session_id` persists a session
        // captured right before a stream error, and that must stay resumable.
        let log = temp_log("errored");
        let id = ExecutionId::new();
        append(&log, &start(id, "/proj"));
        append(&log, &end(id, ExecutionStatus::Errored, Some("sess-1"), None));

        let found = latest_usable_session(&log, profile(), Path::new("/proj"));
        assert_eq!(found, Some(("sess-1".to_string(), 0)));
    }

    #[test]
    fn a_cancelled_execution_is_not_usable() {
        let log = temp_log("cancelled");
        let id = ExecutionId::new();
        append(&log, &start(id, "/proj"));
        append(&log, &end(id, ExecutionStatus::Cancelled, Some("sess-1"), Some(4)));

        assert_eq!(latest_usable_session(&log, profile(), Path::new("/proj")), None);
    }

    #[test]
    fn a_different_working_directory_is_excluded() {
        let log = temp_log("diff-cwd");
        let id = ExecutionId::new();
        append(&log, &start(id, "/proj"));
        append(&log, &end(id, ExecutionStatus::Completed, Some("sess-1"), Some(4)));

        assert_eq!(latest_usable_session(&log, profile(), Path::new("/other")), None);
    }

    #[test]
    fn the_latest_matching_entry_wins_over_an_earlier_one() {
        let log = temp_log("latest-wins");
        let (old, new) = (ExecutionId::new(), ExecutionId::new());
        append(&log, &start(old, "/proj"));
        append(&log, &end(old, ExecutionStatus::Completed, Some("stale"), Some(2)));
        append(&log, &start(new, "/proj"));
        append(&log, &end(new, ExecutionStatus::Completed, Some("fresh"), Some(6)));

        let found = latest_usable_session(&log, profile(), Path::new("/proj"));
        assert_eq!(found, Some(("fresh".to_string(), 6)));
    }

    #[test]
    fn a_missing_execution_log_falls_back_to_none() {
        assert_eq!(latest_usable_session(&temp_log("no-log"), profile(), Path::new("/proj")), None);
    }
}
