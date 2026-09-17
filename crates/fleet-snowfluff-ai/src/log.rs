//! Session log entry format: one JSON object per line (JSONL), matching
//! `ai-chat`'s "Session persistence" requirement. Pure serialize/parse
//! functions plus the context-window conversion; the app crate owns
//! the actual file (path resolution, append, date-folder naming).

use serde::{Deserialize, Serialize};

use crate::message::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogRole {
    User,
    Assistant,
    /// A failed generation, logged for the transcript's own record but
    /// excluded from the context sent on future requests
    /// (`ai-chat`'s "Failure does not pollute future context").
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub role: LogRole,
    pub content: String,
    /// RFC 3339, e.g. `2026-09-17T14:30:22Z`.
    pub timestamp: String,
}

/// Serializes one entry as a single JSONL line (no trailing newline --
/// the caller appends it).
pub fn serialize_entry(entry: &LogEntry) -> String {
    serde_json::to_string(entry).expect("LogEntry serialization is infallible")
}

/// A line that fails to parse is skipped rather than aborting the
/// whole log read -- a single corrupted line (e.g. a truncated write
/// from a crash mid-append) shouldn't make the rest of a session
/// unrecoverable.
pub fn parse_log(contents: &str) -> Vec<LogEntry> {
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Converts persisted log entries into the `Message` list
/// `prompt::assemble_messages` expects as history, excluding `Error`
/// entries.
pub fn entries_to_context(entries: &[LogEntry]) -> Vec<Message> {
    entries
        .iter()
        .filter_map(|entry| match entry.role {
            LogRole::User => Some(Message::user(entry.content.clone())),
            LogRole::Assistant => Some(Message::assistant(entry.content.clone())),
            LogRole::Error => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(role: LogRole, content: &str) -> LogEntry {
        LogEntry {
            role,
            content: content.to_string(),
            timestamp: "2026-09-17T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn round_trips_a_single_entry() {
        let original = entry(LogRole::User, "hello");
        let line = serialize_entry(&original);
        let parsed = parse_log(&line);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].content, "hello");
        assert_eq!(parsed[0].role, LogRole::User);
    }

    #[test]
    fn parses_multiple_lines_in_order() {
        let contents = format!(
            "{}\n{}\n",
            serialize_entry(&entry(LogRole::User, "hi")),
            serialize_entry(&entry(LogRole::Assistant, "hello there"))
        );
        let entries = parse_log(&contents);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, LogRole::User);
        assert_eq!(entries[1].role, LogRole::Assistant);
    }

    #[test]
    fn skips_a_corrupted_line_without_failing_the_whole_read() {
        let contents = format!(
            "{}\nnot valid json\n{}\n",
            serialize_entry(&entry(LogRole::User, "a")),
            serialize_entry(&entry(LogRole::User, "b"))
        );
        let entries = parse_log(&contents);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].content, "a");
        assert_eq!(entries[1].content, "b");
    }

    #[test]
    fn blank_lines_are_ignored() {
        assert!(parse_log("\n\n").is_empty());
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn error_entries_are_excluded_from_context() {
        let entries = vec![
            entry(LogRole::User, "question"),
            entry(LogRole::Error, "request failed"),
            entry(LogRole::User, "retry"),
        ];
        let context = entries_to_context(&entries);
        assert_eq!(context.len(), 2);
        assert!(context.iter().all(|m| m.content != "request failed"));
    }

    #[test]
    fn user_and_assistant_entries_map_to_the_matching_message_role() {
        let entries = vec![entry(LogRole::User, "hi"), entry(LogRole::Assistant, "hello")];
        let context = entries_to_context(&entries);
        assert!(matches!(context[0].role, crate::message::Role::User));
        assert!(matches!(context[1].role, crate::message::Role::Assistant));
    }
}
