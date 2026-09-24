//! History preamble for CLI-backed subscription providers
//! (`cli-session-continuity`). `ClaudeCodeCli`/`Codex` send only the
//! latest user message and rely on the CLI's own `--resume` session for
//! memory; whenever no session is actually resumed (the first message,
//! a stale-resume fallback, or a message escalated from another
//! provider), the CLI has no memory of earlier turns, so the prompt is
//! seeded with a compact rendering of the recent transcript instead.
//!
//! The history is supplied by the caller, not extracted from the
//! message list passed to `chat()`: that list also carries persona
//! few-shot examples as user/assistant pairs, indistinguishable from
//! real turns (see design.md's Decision 2).

use crate::message::{Message, Role};

/// The preamble is spliced into a single process argument, so it is
/// bounded by characters, deliberately modest to stay well inside
/// per-platform argument-length limits (Windows' is the tightest).
pub const HISTORY_CHAR_BUDGET: usize = 6000;

const HEADER: &str = "Earlier in this conversation (context only; reply to the current message):";
/// For a resumed session that missed some turns: it remembers everything
/// up to a point, and these are the turns since.
const CATCH_UP_HEADER: &str = "Since we last spoke, the conversation also included (context only; \
                               reply to the current message):";
const CURRENT_HEADER: &str = "Current message:";
const TRUNCATION_MARKER: &str = " [truncated]";

/// Renders one history turn, or `None` for roles that are not part of a
/// conversation transcript (system prompts, tool results).
fn render_turn(message: &Message) -> Option<String> {
    let speaker = match message.role {
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::System | Role::Tool => return None,
    };
    Some(format!("{speaker}: {}", message.content))
}

/// Cuts `text` to at most `max_chars` characters (never mid-character),
/// appending a marker when anything was cut.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((byte_index, _)) => format!("{}{TRUNCATION_MARKER}", &text[..byte_index]),
        None => text.to_string(),
    }
}

/// Builds the prompt for a session that is *not* being resumed: a
/// header, the most recent turns oldest-to-newest, then the current
/// message. The newest turns win when `char_budget` is exceeded; if even
/// the newest turn alone does not fit, it is included truncated rather
/// than dropped. With no renderable history the current message is
/// returned unchanged.
pub fn render_with_history(history: &[Message], current: &str, char_budget: usize) -> String {
    render(history, current, char_budget, HEADER)
}

fn render(history: &[Message], current: &str, char_budget: usize, header: &str) -> String {
    let mut selected: Vec<String> = Vec::new();
    let mut used = 0usize;
    for turn in history.iter().rev().filter_map(render_turn) {
        let cost = turn.chars().count() + 1; // + newline
        if used + cost <= char_budget {
            used += cost;
            selected.push(turn);
        } else if selected.is_empty() {
            selected.push(truncate_chars(&turn, char_budget.saturating_sub(1)));
            break;
        } else {
            break;
        }
    }
    if selected.is_empty() {
        return current.to_string();
    }
    selected.reverse();
    format!("{header}\n{}\n\n{CURRENT_HEADER}\n{current}", selected.join("\n"))
}

/// The prompt a CLI provider sends.
///
/// - Not resuming: seeded from the whole transcript.
/// - Resuming: the session already holds `history[..seen_turns]`, so only the
///   turns after that -- ones it never saw, such as those another provider
///   answered in `mix` mode -- are sent, and nothing but the latest message
///   when it has missed none.
///
/// `seen_turns` past the end of `history` is treated as "has seen it
/// all" rather than panicking.
pub fn prompt_for_session(
    history: &[Message],
    seen_turns: usize,
    latest: &str,
    resuming: bool,
) -> String {
    if !resuming {
        return render_with_history(history, latest, HISTORY_CHAR_BUDGET);
    }
    let missed = &history[seen_turns.min(history.len())..];
    render(missed, latest, HISTORY_CHAR_BUDGET, CATCH_UP_HEADER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(role: Role, content: &str) -> Message {
        Message { role, content: content.to_string(), tool_calls: Vec::new(), tool_call_id: None }
    }

    #[test]
    fn empty_history_returns_the_current_message_unchanged() {
        assert_eq!(render_with_history(&[], "hello", 6000), "hello");
    }

    #[test]
    fn non_transcript_roles_are_not_rendered() {
        let history = [turn(Role::System, "persona"), turn(Role::Tool, "result")];
        assert_eq!(render_with_history(&history, "hello", 6000), "hello");
    }

    #[test]
    fn turns_are_rendered_oldest_to_newest_before_the_current_message() {
        let history = [
            turn(Role::User, "first question"),
            turn(Role::Assistant, "first answer"),
            turn(Role::User, "second question"),
        ];
        let rendered = render_with_history(&history, "now", 6000);
        let first = rendered.find("User: first question").unwrap();
        let answer = rendered.find("Assistant: first answer").unwrap();
        let second = rendered.find("User: second question").unwrap();
        let current = rendered.find("Current message:\nnow").unwrap();
        assert!(first < answer && answer < second && second < current);
        assert!(rendered.starts_with(HEADER));
    }

    #[test]
    fn the_budget_keeps_the_newest_turns_and_drops_the_oldest() {
        let history = [
            turn(Role::User, "oldest turn that should be dropped"),
            turn(Role::Assistant, "middle"),
            turn(Role::User, "newest"),
        ];
        // Room for "Assistant: middle" (17 + 1) and "User: newest" (12 + 1) only.
        let rendered = render_with_history(&history, "now", 31);
        assert!(!rendered.contains("oldest turn"));
        assert!(rendered.contains("Assistant: middle"));
        assert!(rendered.contains("User: newest"));
    }

    #[test]
    fn a_single_oversized_turn_is_truncated_not_dropped() {
        let history = [turn(Role::User, &"x".repeat(500))];
        let rendered = render_with_history(&history, "now", 50);
        assert!(rendered.contains("User: xxx"));
        assert!(rendered.contains("[truncated]"));
        assert!(!rendered.contains(&"x".repeat(100)));
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        let history = [turn(Role::User, &"雪".repeat(200))];
        let rendered = render_with_history(&history, "now", 20);
        assert!(rendered.contains("[truncated]"));
        // Would panic on a mid-character slice; reaching here is the assertion.
        assert!(rendered.is_char_boundary(rendered.len()));
    }

    #[test]
    fn a_resumed_session_that_missed_nothing_gets_only_the_latest_message() {
        let history = [turn(Role::User, "earlier"), turn(Role::Assistant, "reply")];
        assert_eq!(prompt_for_session(&history, 2, "latest", true), "latest");
    }

    #[test]
    fn a_resumed_session_is_sent_only_the_turns_it_has_not_seen() {
        let history = [
            turn(Role::User, "seen question"),
            turn(Role::Assistant, "seen answer"),
            turn(Role::User, "missed question"),
            turn(Role::Assistant, "missed answer"),
        ];
        let prompt = prompt_for_session(&history, 2, "latest", true);
        assert!(prompt.contains("User: missed question"));
        assert!(prompt.contains("Assistant: missed answer"));
        assert!(!prompt.contains("seen question"), "already held by the session");
        assert!(prompt.starts_with(CATCH_UP_HEADER));
        assert!(prompt.ends_with("Current message:\nlatest"));
    }

    #[test]
    fn a_seen_count_past_the_end_is_treated_as_having_seen_everything() {
        let history = [turn(Role::User, "only turn")];
        assert_eq!(prompt_for_session(&history, 99, "latest", true), "latest");
    }

    #[test]
    fn a_fresh_session_ignores_the_seen_count_and_gets_everything() {
        let history = [turn(Role::User, "earlier"), turn(Role::Assistant, "reply")];
        let prompt = prompt_for_session(&history, 2, "latest", false);
        assert!(prompt.contains("User: earlier"));
        assert!(prompt.starts_with(HEADER));
    }

    #[test]
    fn a_fresh_session_gets_the_history_preamble() {
        let history = [turn(Role::User, "earlier"), turn(Role::Assistant, "reply")];
        let prompt = prompt_for_session(&history, 0, "latest", false);
        assert!(prompt.contains("User: earlier"));
        assert!(prompt.contains("Assistant: reply"));
        assert!(prompt.ends_with("Current message:\nlatest"));
    }

    #[test]
    fn a_fresh_session_with_no_history_adds_nothing() {
        assert_eq!(prompt_for_session(&[], 0, "latest", false), "latest");
    }
}
