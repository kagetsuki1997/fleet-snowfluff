//! Chat session log storage: JSONL files under
//! `chat-logs/<date>/<timestamp>_<session-id>.jsonl`
//! (`ai-chat`'s "Session persistence"). `fleet_snowfluff_ai::log` has
//! the pure entry format; this module is the file-I/O and path-naming
//! layer around it, same split as every other `*_store.rs` module.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use chrono::Utc;
use fleet_snowfluff_ai::{log as ai_log, LogEntry};
use rand::Rng;
use tauri::{AppHandle, Manager};

fn chat_logs_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("chat-logs"))
}

/// A session's folder/filename is derived once, at creation, from
/// `now` -- fixed for the session's whole lifetime regardless of how
/// long it runs (`ai-chat`'s "Session spans midnight" scenario: the
/// date-folder placement never changes after this).
fn new_session_path(base_dir: &Path, now: chrono::DateTime<Utc>) -> PathBuf {
    let date = now.format("%Y-%m-%d").to_string();
    let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let session_id: u64 = rand::rng().random();
    base_dir.join(date).join(format!("{timestamp}_{session_id:016x}.jsonl"))
}

/// Creates a brand-new session file path (not yet created on disk --
/// the first `append_entry` call creates it). Called only for an
/// explicit "New Chat" action, or when no session exists at all yet.
pub fn create_new_session(app: &AppHandle) -> Option<PathBuf> {
    Some(new_session_path(&chat_logs_dir(app)?, Utc::now()))
}

/// Finds the most recently created session across every date folder.
/// Filenames start with a sortable timestamp, so the
/// lexicographically-last filename is the most recent session
/// (`ai-chat`'s "Reopening does not start a new session" / "Session
/// resume with scrollback" -- this is what resume finds).
pub fn find_latest_session(app: &AppHandle) -> Option<PathBuf> {
    let dir = chat_logs_dir(app)?;
    let mut latest: Option<PathBuf> = None;
    for date_entry in std::fs::read_dir(&dir).ok()?.flatten() {
        if !date_entry.path().is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(date_entry.path()) else { continue };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let is_newer =
                latest.as_ref().is_none_or(|current| path.file_name() > current.file_name());
            if is_newer {
                latest = Some(path);
            }
        }
    }
    latest
}

/// Resolves the session to use right now: the latest existing one, or
/// a freshly created path if none exists yet (the very first message
/// ever sent implicitly creates a session -- explicit "New Chat" is
/// only about *rotating* to a new one, not about the initial one).
pub fn current_or_new_session(app: &AppHandle) -> Option<PathBuf> {
    find_latest_session(app).or_else(|| create_new_session(app))
}

/// Loads the entries of an already-existing session file. A missing
/// file (nothing sent yet) reads as an empty session, not an error.
pub fn read_session(path: &Path) -> Vec<LogEntry> {
    std::fs::read_to_string(path).map(|s| ai_log::parse_log(&s)).unwrap_or_default()
}

/// Appends one entry to a session file, creating the file/directory if
/// needed. Best-effort: a write failure is logged, not propagated,
/// same as every other `*_store.rs` save function.
pub fn append_entry(path: &Path, entry: &LogEntry) {
    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            log::error!("failed to create chat-logs dir {dir:?}: {err}");
            return;
        }
    }
    let line = format!("{}\n", ai_log::serialize_entry(entry));
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(err) = file.write_all(line.as_bytes()) {
                log::error!("failed to append to session log {path:?}: {err}");
            }
        }
        Err(err) => log::error!("failed to open session log {path:?}: {err}"),
    }
}

/// One entry in the past-conversations list
/// (`execution-log-and-context`'s "List and open past conversations").
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConversationSummary {
    pub session_path: PathBuf,
    pub title: String,
    pub last_activity: String,
}

const TITLE_MAX_CHARS: usize = 60;

/// Every conversation under `chat-logs/*/*.jsonl`, newest-first: a
/// directory scan, not an index file kept in sync (design.md's Decision
/// 7 -- a realistically small number of small log files makes this
/// unmeasurably fast, and adds no new failure mode).
pub fn list_conversations(app: &AppHandle) -> Vec<ConversationSummary> {
    match chat_logs_dir(app) {
        Some(dir) => list_conversations_in(&dir),
        None => Vec::new(),
    }
}

/// `list_conversations`'s own scan, taking the resolved directory
/// directly -- `chat_logs_dir` needs an `AppHandle`, which a unit test
/// doesn't have (same split `find_latest_session`'s own test already
/// uses, factored into a real function here instead of copy-pasted into
/// the test).
fn list_conversations_in(dir: &Path) -> Vec<ConversationSummary> {
    let Ok(date_entries) = std::fs::read_dir(dir) else { return Vec::new() };

    let mut summaries: Vec<ConversationSummary> = date_entries
        .flatten()
        .filter(|date_entry| date_entry.path().is_dir())
        .filter_map(|date_entry| std::fs::read_dir(date_entry.path()).ok())
        .flatten()
        .flatten()
        .map(|file_entry| file_entry.path())
        .filter(|path| is_conversation_file(path))
        .map(|path| summarize_conversation(&path))
        .collect();

    summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    summaries
}

/// `Path::extension()` only looks at the text after the *last* `.`, so
/// a conversation's own `<ts>_<id>.executions.jsonl` sibling
/// (`execution_log_store::log_path`) also reports an extension of
/// `jsonl` and would otherwise be mistaken for a second conversation.
fn is_conversation_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        && !path.to_string_lossy().ends_with(".executions.jsonl")
}

/// `title` is the first line's content, truncated; `last_activity` is
/// the last line's own timestamp, or the file's mtime for a
/// conversation with no entries yet (a session file is created the
/// moment the first message is appended, so this is reachable for at
/// most a brief window, not a normal steady state). A corrupt first
/// line never crashes this -- `read_session`/`parse_log` already skip
/// an unparseable line, so "first" here means the first *valid* one.
fn summarize_conversation(path: &Path) -> ConversationSummary {
    let entries = read_session(path);
    let title = entries.first().map(|entry| truncate_chars(&entry.content, TITLE_MAX_CHARS));
    let last_activity = entries
        .last()
        .map(|entry| entry.timestamp.clone())
        .or_else(|| file_mtime_rfc3339(path))
        .unwrap_or_default();
    ConversationSummary {
        session_path: path.to_path_buf(),
        title: title.unwrap_or_default(),
        last_activity,
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((byte_index, _)) => text[..byte_index].to_string(),
        None => text.to_string(),
    }
}

fn file_mtime_rfc3339(path: &Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(chrono::DateTime::<Utc>::from(modified).to_rfc3339())
}

/// Implements `fleet_snowfluff_ai::context_manager::SessionLog` by
/// delegating straight to `read_session`/`append_entry` above --
/// `ContextManager` lives in the ai crate and must not depend on this
/// one (path resolution here needs `AppHandle`), so this is the
/// adapter that lets it use this module's real file storage anyway.
pub struct ChatLogStore;

impl fleet_snowfluff_ai::SessionLog for ChatLogStore {
    fn read(&self, session_path: &Path) -> Vec<LogEntry> { read_session(session_path) }

    fn append(&self, session_path: &Path, entry: &LogEntry) { append_entry(session_path, entry) }
}

#[cfg(test)]
mod tests {
    use fleet_snowfluff_ai::{LogRole, SessionLog};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-chat-log-test-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn entry(role: LogRole, content: &str) -> LogEntry {
        LogEntry {
            role,
            content: content.to_string(),
            timestamp: "2026-09-17T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn new_session_path_is_foldered_by_the_given_date() {
        let base = PathBuf::from("/tmp/chat-logs");
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-17T23:58:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let path = new_session_path(&base, now);
        assert_eq!(path.parent().unwrap().parent().unwrap(), base);
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "2026-09-17");
        assert!(path.file_name().unwrap().to_str().unwrap().starts_with("20260917T235800Z_"));
    }

    #[test]
    fn append_then_read_round_trips() {
        let dir = temp_dir("append-read");
        let path = dir.join("2026-09-17").join("session.jsonl");

        append_entry(&path, &entry(LogRole::User, "hi"));
        append_entry(&path, &entry(LogRole::Assistant, "hello"));

        let entries = read_session(&path);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].content, "hi");
        assert_eq!(entries[1].content, "hello");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_session_on_a_missing_file_is_an_empty_session_not_an_error() {
        let path = temp_dir("missing").join("2026-09-17").join("nope.jsonl");
        assert!(read_session(&path).is_empty());
    }

    #[test]
    fn find_latest_session_picks_the_most_recent_filename() {
        let dir = temp_dir("latest");
        let older = dir.join("2026-09-16").join("20260916T090000Z_0000000000000001.jsonl");
        let newer = dir.join("2026-09-17").join("20260917T090000Z_0000000000000002.jsonl");
        append_entry(&older, &entry(LogRole::User, "old"));
        append_entry(&newer, &entry(LogRole::User, "new"));

        // find_latest_session itself takes an AppHandle we don't have
        // in a unit test; exercise the directory-scanning logic
        // directly against the same layout it scans.
        let mut latest: Option<PathBuf> = None;
        for date_entry in std::fs::read_dir(&dir).unwrap().flatten() {
            for file_entry in std::fs::read_dir(date_entry.path()).unwrap().flatten() {
                let path = file_entry.path();
                let is_newer =
                    latest.as_ref().is_none_or(|current| path.file_name() > current.file_name());
                if is_newer {
                    latest = Some(path);
                }
            }
        }
        assert_eq!(latest.unwrap(), newer);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chat_log_store_adapter_delegates_to_the_real_functions() {
        let dir = temp_dir("adapter");
        let path = dir.join("2026-09-17").join("session.jsonl");

        let adapter = ChatLogStore;
        adapter.append(&path, &entry(LogRole::User, "hi"));
        let entries = adapter.read(&path);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "hi");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn entry_at(role: LogRole, content: &str, timestamp: &str) -> LogEntry {
        LogEntry { role, content: content.to_string(), timestamp: timestamp.to_string() }
    }

    #[test]
    fn list_conversations_orders_newest_first_by_last_entry_timestamp() {
        let dir = temp_dir("list-order");
        let older = dir.join("2026-09-16").join("20260916T090000Z_0000000000000001.jsonl");
        let newer = dir.join("2026-09-17").join("20260917T090000Z_0000000000000002.jsonl");
        append_entry(&older, &entry_at(LogRole::User, "older hello", "2026-09-16T09:00:00Z"));
        append_entry(&newer, &entry_at(LogRole::User, "newer hello", "2026-09-17T09:00:00Z"));

        let summaries = list_conversations_in(&dir);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].session_path, newer);
        assert_eq!(summaries[1].session_path, older);
        assert_eq!(summaries[0].title, "newer hello");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_conversation_with_no_entries_yet_still_appears_with_an_empty_title() {
        let dir = temp_dir("list-empty");
        let path = dir.join("2026-09-17").join("20260917T090000Z_0000000000000001.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();

        let summaries = list_conversations_in(&dir);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].title, "");
        assert!(!summaries[0].last_activity.is_empty(), "falls back to the file's own mtime");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupt_first_line_does_not_crash_the_scan() {
        let dir = temp_dir("list-corrupt");
        let path = dir.join("2026-09-17").join("20260917T090000Z_0000000000000001.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let valid =
            ai_log::serialize_entry(&entry_at(LogRole::User, "still here", "2026-09-17T09:00:00Z"));
        std::fs::write(&path, format!("not valid json\n{valid}\n")).unwrap();

        let summaries = list_conversations_in(&dir);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].title, "still here");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_executions_log_sibling_is_not_counted_as_its_own_conversation() {
        let dir = temp_dir("list-executions-sibling");
        let transcript = dir.join("2026-09-17").join("20260917T090000Z_abc123.jsonl");
        let executions = dir.join("2026-09-17").join("20260917T090000Z_abc123.executions.jsonl");
        append_entry(&transcript, &entry_at(LogRole::User, "hello", "2026-09-17T09:00:00Z"));
        std::fs::write(&executions, "{\"t\":\"start\"}\n").unwrap();

        let summaries = list_conversations_in(&dir);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_path, transcript);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_title_longer_than_the_max_is_truncated() {
        assert_eq!(truncate_chars(&"x".repeat(100), 60).chars().count(), 60);
        assert_eq!(truncate_chars("short", 60), "short");
    }
}
