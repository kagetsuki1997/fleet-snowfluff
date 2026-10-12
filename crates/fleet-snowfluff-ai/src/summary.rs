//! The rolling conversation summary and where it is kept.
//!
//! When turns fall outside the history budget they are folded into a summary
//! instead of vanishing (`context-compaction`). The summary lives in a sidecar
//! file next to the session, never inside the session log: the transcript the
//! user reads stays exactly as written, no new role has to be taught to the log
//! reader or the UI, and a missing or damaged sidecar simply means "no
//! summary", which is the safe state.
//!
//! The summary is a restartable function of the transcript. It covers the first
//! `covered_through` context-visible messages (see
//! [`crate::log::context_visible`]); a failed or interrupted compaction changes
//! nothing, so the next one covers the same slice again.

use std::{
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::log::LogEntry;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummary {
    /// How many of the conversation's context-visible messages this summary
    /// covers.
    pub covered_through: usize,
    pub text: String,
    /// RFC 3339.
    pub updated_at: String,
    /// Timestamp of the last covered message. Used to notice a summary that no
    /// longer matches its transcript (the log was edited or replaced), in
    /// which case the summary is ignored.
    #[serde(default)]
    pub last_covered_timestamp: Option<String>,
}

impl ConversationSummary {
    /// Whether this summary still describes `visible` (the session's
    /// context-visible entries): it must not claim to cover more than
    /// exists, and the last message it covered must still be the one it was
    /// written for.
    pub fn matches(&self, visible: &[&LogEntry]) -> bool {
        if self.covered_through > visible.len() {
            return false;
        }
        if self.covered_through == 0 {
            return true;
        }
        match &self.last_covered_timestamp {
            Some(timestamp) => visible[self.covered_through - 1].timestamp == *timestamp,
            None => true,
        }
    }
}

/// Reads and writes a session's summary. A trait so the compaction code can be
/// tested without a filesystem, the same way
/// [`crate::context_manager::SessionLog`] is.
pub trait SummaryStore: Send + Sync {
    /// `None` when there is no summary, or the stored one cannot be read.
    fn read(&self, session_path: &Path) -> Option<ConversationSummary>;
    fn write(&self, session_path: &Path, summary: &ConversationSummary) -> io::Result<()>;
}

/// The sidecar's path: `<session file>.summary.json` next to the session. The
/// extension is `json`, so session listing (which looks for `.jsonl`) never
/// mistakes it for a conversation.
pub fn summary_path(session_path: &Path) -> PathBuf {
    let mut name = session_path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".summary.json");
    session_path.with_file_name(name)
}

/// Summaries stored as files beside their sessions.
pub struct FileSummaryStore;

impl SummaryStore for FileSummaryStore {
    fn read(&self, session_path: &Path) -> Option<ConversationSummary> {
        let text = std::fs::read_to_string(summary_path(session_path)).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn write(&self, session_path: &Path, summary: &ConversationSummary) -> io::Result<()> {
        let path = summary_path(session_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Written beside the target and renamed into place, so a crash mid-write leaves
        // the old summary or none, never half of one.
        let mut temp_name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        temp_name.push(".tmp");
        let temp = path.with_file_name(temp_name);
        let body = serde_json::to_string(summary).map_err(io::Error::other)?;
        std::fs::write(&temp, body)?;
        std::fs::rename(&temp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{context_visible, LogRole};

    fn temp_session(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-summary-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("2026-10-08_0000000000000001.jsonl")
    }

    fn entry(role: LogRole, content: &str, timestamp: &str) -> LogEntry {
        LogEntry { role, content: content.into(), timestamp: timestamp.into() }
    }

    fn sample() -> ConversationSummary {
        ConversationSummary {
            covered_through: 4,
            text: "The user has a cat named Mochi.".into(),
            updated_at: "2026-10-08T12:00:00Z".into(),
            last_covered_timestamp: Some("t4".into()),
        }
    }

    #[test]
    fn the_sidecar_sits_beside_the_session_and_is_not_a_jsonl_file() {
        let path = summary_path(Path::new("/logs/2026-10-08/a_b.jsonl"));
        assert_eq!(path, Path::new("/logs/2026-10-08/a_b.jsonl.summary.json"));
        assert_ne!(path.extension().and_then(|e| e.to_str()), Some("jsonl"));
    }

    #[test]
    fn a_summary_round_trips_through_the_file_store() {
        let session = temp_session("roundtrip");
        let store = FileSummaryStore;
        store.write(&session, &sample()).unwrap();
        assert_eq!(store.read(&session), Some(sample()));
    }

    #[test]
    fn a_missing_sidecar_reads_as_no_summary() {
        let session = temp_session("missing");
        assert_eq!(FileSummaryStore.read(&session), None);
    }

    #[test]
    fn a_corrupt_sidecar_reads_as_no_summary_without_error() {
        let session = temp_session("corrupt");
        std::fs::write(summary_path(&session), "{ not json").unwrap();
        assert_eq!(FileSummaryStore.read(&session), None);
    }

    #[test]
    fn writing_replaces_the_previous_summary_and_leaves_no_temp_file() {
        let session = temp_session("replace");
        let store = FileSummaryStore;
        store.write(&session, &sample()).unwrap();
        let newer = ConversationSummary { covered_through: 8, text: "newer".into(), ..sample() };
        store.write(&session, &newer).unwrap();
        assert_eq!(store.read(&session).unwrap().covered_through, 8);
        let leftovers: Vec<_> = std::fs::read_dir(session.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn a_summary_matches_the_transcript_it_was_written_for() {
        let entries = vec![
            entry(LogRole::User, "a", "t1"),
            entry(LogRole::Assistant, "b", "t2"),
            entry(LogRole::User, "c", "t3"),
            entry(LogRole::Assistant, "d", "t4"),
            entry(LogRole::User, "e", "t5"),
        ];
        assert!(sample().matches(&context_visible(&entries)));
    }

    #[test]
    fn an_error_entry_in_the_log_does_not_shift_the_covered_index() {
        let entries = vec![
            entry(LogRole::User, "a", "t1"),
            entry(LogRole::Error, "request failed", "t1b"),
            entry(LogRole::Assistant, "b", "t2"),
            entry(LogRole::User, "c", "t3"),
            entry(LogRole::Assistant, "d", "t4"),
        ];
        let visible = context_visible(&entries);
        assert_eq!(visible.len(), 4);
        assert!(sample().matches(&visible), "covered_through counts context-visible messages only");
    }

    #[test]
    fn a_summary_for_a_different_transcript_does_not_match() {
        let entries = vec![
            entry(LogRole::User, "a", "t1"),
            entry(LogRole::Assistant, "b", "t2"),
            entry(LogRole::User, "c", "t3"),
            entry(LogRole::Assistant, "d", "DIFFERENT"),
        ];
        assert!(!sample().matches(&context_visible(&entries)));
    }

    #[test]
    fn a_summary_claiming_more_than_the_transcript_has_does_not_match() {
        let entries = vec![entry(LogRole::User, "a", "t1")];
        assert!(!sample().matches(&context_visible(&entries)));
    }
}
