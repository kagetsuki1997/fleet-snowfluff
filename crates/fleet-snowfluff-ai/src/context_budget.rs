//! Keeping a request inside the model's context window: where the window size
//! comes from, and what the agent loop does when a long run starts to fill it.
//!
//! Two different problems share this module's inputs. The pre-flight fit check
//! (added alongside) decides what a request is built from before it is sent.
//! This safety net handles what happens *during* a run: each tool call adds a
//! result to the conversation, and a run of several large ones can fill a
//! window that the first request fit comfortably.

use crate::{
    message::{Message, Role},
    token_estimate::{estimate_messages_tokens, estimate_tools_tokens},
    tool_provider::ToolDefinition,
};

/// The window assumed for a model whose name is not recognised. Deliberately
/// small: the safety net then acts early rather than late, and the cloud APIs
/// reject an over-long request with a visible error rather than trimming it
/// silently, so a wrong guess costs a little, not the persona.
pub const UNKNOWN_MODEL_WINDOW: usize = 32_768;

/// Context windows for model-name prefixes. Conservative lower bounds, and
/// cheap to extend as models change; nothing here is queried from a provider.
const KNOWN_WINDOWS: &[(&str, usize)] = &[
    ("claude-", 200_000),
    ("gpt-4o", 128_000),
    ("gpt-4.1", 128_000),
    ("gpt-5", 128_000),
    ("o1", 128_000),
    ("o3", 128_000),
    ("o4", 128_000),
];

/// The context window to assume for an API profile's model, by name.
pub fn api_window(model: Option<&str>) -> usize {
    let Some(model) = model else { return UNKNOWN_MODEL_WINDOW };
    let model = model.trim().to_lowercase();
    KNOWN_WINDOWS
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map_or(UNKNOWN_MODEL_WINDOW, |&(_, window)| window)
}

/// The share of the window at which tool results start being removed.
pub const SAFETY_NET_PERCENT: usize = 80;
/// The newest tool results are always kept whole.
pub const KEEP_LATEST_TOOL_RESULTS: usize = 4;

/// Whether a model server's own count of the prompt it processed is so far
/// below our estimate that it almost certainly truncated the prompt. The
/// estimate never runs below the real count (see `token_estimate`), so
/// reported-below-half cannot happen with a prompt that arrived whole. A mild
/// trim is not detectable this way, which is why the pre-flight fit check, not
/// this, is the guard.
pub fn suspected_truncation(reported_prompt_tokens: usize, estimated_prompt_tokens: usize) -> bool {
    estimated_prompt_tokens > 0 && reported_prompt_tokens * 2 < estimated_prompt_tokens
}

const REMOVED_NOTE: &str =
    "[An earlier tool result was removed to make room. Run the tool again if you need it.]";

/// If a tool result ends with the marker an oversize result carries
/// (`bound_tool_output`), the marker is all that is worth keeping: it says
/// where the rest was saved.
fn kept_marker(content: &str) -> Option<&str> {
    let start = content.rfind("[This output was cut off")?;
    Some(&content[start..])
}

fn is_removed(message: &Message) -> bool { message.content.starts_with(REMOVED_NOTE) }

/// Replaces the oldest tool results, keeping the newest
/// [`KEEP_LATEST_TOOL_RESULTS`], until the request (`messages` plus `tools`) is
/// at or below [`SAFETY_NET_PERCENT`] of `window`, or nothing more can be
/// removed. Returns how many results were replaced.
///
/// Routinely removing old results would also invalidate the provider's prompt
/// cache, so this only runs under real pressure, as a last resort.
pub fn shrink_tool_results(
    messages: &mut [Message],
    tools: &[ToolDefinition],
    window: usize,
) -> usize {
    let limit = window * SAFETY_NET_PERCENT / 100;
    let tools_tokens = estimate_tools_tokens(tools);
    let mut removed = 0;
    loop {
        if estimate_messages_tokens(messages) + tools_tokens <= limit {
            return removed;
        }
        let tool_positions: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.role == Role::Tool)
            .map(|(index, _)| index)
            .collect();
        let removable = tool_positions.len().saturating_sub(KEEP_LATEST_TOOL_RESULTS);
        let Some(&index) =
            tool_positions[..removable].iter().find(|&&index| !is_removed(&messages[index]))
        else {
            return removed;
        };
        let replacement = match kept_marker(&messages[index].content) {
            Some(marker) => format!("{REMOVED_NOTE}\n{marker}"),
            None => REMOVED_NOTE.to_string(),
        };
        messages[index].content = replacement;
        removed += 1;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool_message(body: &str) -> Message { Message::tool_result("call", body.to_string()) }

    fn many_tool_results(count: usize, words: usize) -> Vec<Message> {
        let mut messages = vec![Message::system("persona"), Message::user("go")];
        for i in 0..count {
            messages.push(tool_message(&format!("result {i} {}", "word ".repeat(words))));
        }
        messages
    }

    #[test]
    fn a_prompt_the_server_cut_to_a_fraction_is_flagged_but_a_whole_one_is_not() {
        // Measured on the dev machine: a 520-token prompt came back as 130 processed
        // tokens.
        assert!(suspected_truncation(130, 520));
        // A whole prompt reports roughly its real size, which our estimate (1.0x to
        // 1.7x) exceeds.
        assert!(!suspected_truncation(3_334, 3_334));
        assert!(!suspected_truncation(3_334, 5_000));
        assert!(!suspected_truncation(10, 0));
    }

    #[test]
    fn known_model_names_map_to_their_windows() {
        assert_eq!(api_window(Some("claude-sonnet-5-5")), 200_000);
        assert_eq!(api_window(Some("gpt-4o-mini")), 128_000);
        assert_eq!(api_window(Some("gpt-4.1")), 128_000);
        assert_eq!(api_window(Some("o3")), 128_000);
        assert_eq!(api_window(Some("  CLAUDE-opus  ")), 200_000);
    }

    #[test]
    fn an_unknown_or_missing_model_gets_the_conservative_default() {
        assert_eq!(api_window(Some("some-new-model")), UNKNOWN_MODEL_WINDOW);
        assert_eq!(api_window(Some("llama3.2")), UNKNOWN_MODEL_WINDOW);
        assert_eq!(api_window(None), UNKNOWN_MODEL_WINDOW);
    }

    #[test]
    fn a_request_within_the_limit_is_left_alone() {
        let mut messages = many_tool_results(6, 10);
        let before = messages.clone();
        assert_eq!(shrink_tool_results(&mut messages, &[], 32_768), 0);
        assert_eq!(messages, before);
    }

    #[test]
    fn under_pressure_the_oldest_results_go_first_and_the_latest_are_kept_whole() {
        let mut messages = many_tool_results(10, 400);
        let before = messages.clone();
        let window = 3_000;

        let removed = shrink_tool_results(&mut messages, &[], window);

        assert!(removed > 0);
        assert!(estimate_messages_tokens(&messages) < estimate_messages_tokens(&before));
        let tool_indexes: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == Role::Tool)
            .map(|(i, _)| i)
            .collect();
        for &index in &tool_indexes[tool_indexes.len() - KEEP_LATEST_TOOL_RESULTS..] {
            assert_eq!(messages[index], before[index], "the newest results stay whole");
        }
        assert!(is_removed(&messages[tool_indexes[0]]), "the oldest goes first");
    }

    #[test]
    fn only_as_many_are_removed_as_needed() {
        let mut messages = many_tool_results(10, 400);
        let removed_a_little = shrink_tool_results(&mut messages.clone(), &[], 3_800);
        let removed_a_lot = shrink_tool_results(&mut messages, &[], 2_000);
        assert!(removed_a_little < removed_a_lot);
    }

    #[test]
    fn the_system_prompt_and_user_message_are_never_touched() {
        let mut messages = many_tool_results(10, 400);
        shrink_tool_results(&mut messages, &[], 1_000);
        assert_eq!(messages[0].content, "persona");
        assert_eq!(messages[1].content, "go");
    }

    #[test]
    fn a_marker_that_says_where_the_rest_was_saved_survives_removal() {
        let mut messages = vec![Message::system("p"), Message::user("go")];
        messages.push(tool_message(&format!(
            "{}\n\n[This output was cut off after about 100 of 9000 estimated tokens. The rest \
             was saved in /spill/a-part-001.txt. Read it with read_file if you need it.]",
            "word ".repeat(2_000)
        )));
        for _ in 0..KEEP_LATEST_TOOL_RESULTS {
            messages.push(tool_message("kept"));
        }
        shrink_tool_results(&mut messages, &[], 600);
        assert!(messages[2].content.contains("/spill/a-part-001.txt"));
        assert!(messages[2].content.starts_with(REMOVED_NOTE));
    }

    #[test]
    fn tool_definitions_count_toward_the_limit() {
        let tools: Vec<ToolDefinition> = (0..30)
            .map(|i| ToolDefinition {
                name: format!("tool_{i}"),
                description: "word ".repeat(80),
                parameters: json!({"type": "object"}),
            })
            .collect();
        let mut with_tools = many_tool_results(8, 100);
        let mut without = with_tools.clone();
        let removed_with = shrink_tool_results(&mut with_tools, &tools, 4_000);
        let removed_without = shrink_tool_results(&mut without, &[], 4_000);
        assert!(removed_with > removed_without);
    }

    #[test]
    fn it_stops_when_nothing_more_can_be_removed() {
        let mut messages = many_tool_results(KEEP_LATEST_TOOL_RESULTS, 2_000);
        assert_eq!(
            shrink_tool_results(&mut messages, &[], 100),
            0,
            "all are in the protected newest set"
        );
    }
}
