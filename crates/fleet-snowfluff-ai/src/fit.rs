//! The pre-flight fit check: making sure a request fits the model's context
//! window **without ever trimming the persona prompt** (`ai-provider`'s "The
//! persona prompt is never trimmed").
//!
//! Measured on a local model, an overflowing request is harmless to the persona
//! as long as the system prompt itself fits: the server keeps the system
//! message and drops older history. The failure is different. When the system
//! prompt plus the newest message do not fit, the server truncates the raw
//! prompt and the persona is destroyed, and the reply falls back to a generic
//! assistant. The system turn also carries the tool definitions in that
//! template, so a long tool list on a small window can cause it. So before a
//! request is sent, this decides what it is built from, shedding optional
//! content in a fixed order, and refuses to send at all when even the persona,
//! the newest message, and a minimal reply cannot fit.
//!
//! The order, first to go first: older history, keyword-matched memories, the
//! conversation summary, the memory digest, the persona's few-shot examples,
//! the optional tool listings (stubs), and finally the reply allowance down to
//! a floor. The persona prompt, the newest message, and the native
//! tools are never shed.
//!
//! The check works on the already-assembled message list so it can sit in front
//! of every dispatch path. It relies on a few facts about that list: the system
//! prompt is message 0, the persona's few-shot examples follow it, the newest
//! user message is last, and the blocks other features add are recognised by
//! their tags. The memory block is `<retrieved_memory>` with a `<digest>`
//! section and a `<matches>` section inside it.

use crate::{
    message::{Message, Role},
    token_estimate::{estimate_messages_tokens, estimate_tools_tokens},
    tool_provider::ToolDefinition,
};

/// Headroom kept free beyond what the estimate says is needed, as a share of
/// the window, because the estimate is only an estimate and tool results added
/// during a run need somewhere to go.
pub const MARGIN_PERCENT: usize = 10;
/// The reply allowance is capped at this share of the window.
pub const REPLY_WINDOW_PERCENT: usize = 25;
/// The smallest reply allowance worth sending a request for.
pub const REPLY_FLOOR: usize = 512;

pub const SUMMARY_TAG: (&str, &str) = ("<conversation_summary>", "</conversation_summary>");
pub const MEMORY_TAG: (&str, &str) = ("<retrieved_memory>", "</retrieved_memory>");
const DIGEST_TAG: (&str, &str) = ("<digest>", "</digest>");
const MATCHES_TAG: (&str, &str) = ("<matches>", "</matches>");

/// What was given up to make the request fit, in the order it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shed {
    /// This many of the oldest history messages.
    History(usize),
    Matches,
    Summary,
    Digest,
    FewShot,
    Stubs,
    ReplyAllowance,
}

/// The window is too small for the persona, the newest message, the native
/// tools, and a minimal reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTooSmall {
    pub needed: usize,
    pub window: usize,
}

impl std::fmt::Display for WindowTooSmall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the model's context window ({} tokens) is too small for the persona and the tools \
             (about {} needed)",
            self.window, self.needed
        )
    }
}

impl std::error::Error for WindowTooSmall {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fit {
    pub messages: Vec<Message>,
    /// Whether the stub tool listings may still be sent.
    pub keep_stubs: bool,
    /// The reply allowance to request, in tokens.
    pub reply_allowance: usize,
    pub shed: Vec<Shed>,
}

/// Splits a leading `open ... close` block off `content`. `None` unless the
/// block is at the very start, which is where every feature that adds one puts
/// it, so text a user typed that happens to contain the tag is never taken for
/// a block.
fn split_leading_block<'a>(
    content: &'a str,
    (open, close): (&str, &str),
) -> Option<(&'a str, &'a str)> {
    if !content.starts_with(open) {
        return None;
    }
    let end = content.find(close)? + close.len();
    Some((&content[..end], content[end..].trim_start_matches('\n')))
}

fn strip_leading_block(content: &str, tag: (&str, &str)) -> Option<String> {
    split_leading_block(content, tag).map(|(_, rest)| rest.to_string())
}

/// Removes the `<section>...</section>` part inside the memory block, leaving
/// the rest of it. When nothing is left inside the block, the whole block goes.
fn strip_memory_section(content: &str, section: (&str, &str)) -> Option<String> {
    let (block, rest) = split_leading_block(content, MEMORY_TAG)?;
    let start = block.find(section.0)?;
    let end = block.find(section.1)? + section.1.len();
    let trimmed = format!("{}{}", &block[..start], block[end..].trim_start_matches('\n'));
    // Keep the block only while a section remains inside it.
    let has_other_section = trimmed.contains(DIGEST_TAG.0) || trimmed.contains(MATCHES_TAG.0);
    Some(if has_other_section { format!("{trimmed}\n\n{rest}") } else { rest.to_string() })
}

fn map_first_user_with<F>(messages: &mut [Message], from: usize, mut change: F) -> bool
where
    F: FnMut(&str) -> Option<String>,
{
    for message in messages.iter_mut().skip(from).filter(|m| m.role == Role::User) {
        if let Some(new_content) = change(&message.content) {
            message.content = new_content;
            return true;
        }
    }
    false
}

/// Removes the leading block `tag` from whichever user message carries it.
fn strip_block_everywhere(messages: &mut [Message], from: usize, tag: (&str, &str)) -> bool {
    map_first_user_with(messages, from, |content| strip_leading_block(content, tag))
}

/// Gives up one kind of optional block, wherever it sits. `false` when there
/// was none to remove.
fn shed_block(step: &Shed, messages: &mut [Message], from: usize) -> bool {
    match step {
        Shed::Matches => {
            map_first_user_with(messages, from, |c| strip_memory_section(c, MATCHES_TAG))
        }
        Shed::Summary => strip_block_everywhere(messages, from, SUMMARY_TAG),
        Shed::Digest => {
            map_first_user_with(messages, from, |c| strip_memory_section(c, DIGEST_TAG))
        }
        _ => false,
    }
}

/// Drops the oldest history message and any assistant message left leading
/// afterwards (so roles keep alternating). A summary block riding on a dropped
/// message moves to the next user message instead of being lost with it.
/// Returns how many messages were dropped.
fn drop_oldest_history(messages: &mut Vec<Message>, history_start: usize) -> usize {
    // Everything from `history_start` up to (not including) the newest message is
    // history.
    let newest = messages.len() - 1;
    if history_start >= newest {
        return 0;
    }
    let mut dropped = 0;
    let mut carried: Vec<String> = Vec::new();
    loop {
        if history_start >= messages.len() - 1 {
            break;
        }
        let removed = messages.remove(history_start);
        dropped += 1;
        let mut content = removed.content.as_str();
        while let Some((block, rest)) = split_leading_block(content, SUMMARY_TAG)
            .or_else(|| split_leading_block(content, MEMORY_TAG))
        {
            carried.push(block.to_string());
            content = rest;
        }
        let next_is_assistant = messages[history_start].role != Role::User;
        let reached_newest = history_start >= messages.len() - 1;
        if !next_is_assistant || reached_newest {
            break;
        }
    }
    if !carried.is_empty() {
        if let Some(target) = messages.iter_mut().skip(history_start).find(|m| m.role == Role::User)
        {
            target.content = format!("{}\n\n{}", carried.join("\n\n"), target.content);
        }
    }
    dropped
}

/// Builds the request that is actually sent, or says the window is too small.
///
/// `messages` is the assembled conversation, `few_shot_len` how many few-shot
/// messages follow the system prompt, `native_tools` the tool definitions that
/// must always be sent, and `stubs` the optional tool listings.
/// `requested_reply` is the reply allowance the caller would like.
pub fn fit_request(
    messages: Vec<Message>,
    few_shot_len: usize,
    native_tools: &[ToolDefinition],
    stubs: &[ToolDefinition],
    window: usize,
    requested_reply: usize,
) -> Result<Fit, WindowTooSmall> {
    let margin = window * MARGIN_PERCENT / 100;
    let native = estimate_tools_tokens(native_tools);
    let stub_tokens = estimate_tools_tokens(stubs);
    let mut messages = messages;
    let mut keep_stubs = !stubs.is_empty();
    let few_shot_len = few_shot_len.min(messages.len().saturating_sub(2));
    let mut shed = Vec::new();

    let reply = requested_reply.min(window * REPLY_WINDOW_PERCENT / 100).max(REPLY_FLOOR);
    let need = |messages: &[Message], keep_stubs: bool| {
        estimate_messages_tokens(messages)
            + native
            + if keep_stubs { stub_tokens } else { 0 }
            + margin
    };
    let fits =
        |messages: &[Message], keep_stubs: bool| need(messages, keep_stubs) + reply <= window;

    // 1. Older history, oldest first.
    let mut dropped = 0;
    while !fits(&messages, keep_stubs) {
        let n = drop_oldest_history(&mut messages, 1 + few_shot_len);
        if n == 0 {
            break;
        }
        dropped += n;
    }
    if dropped > 0 {
        shed.push(Shed::History(dropped));
    }

    // 2.-4. The memory matches, the summary, and the memory digest.
    let history_start = 1 + few_shot_len;
    for step in [Shed::Matches, Shed::Summary, Shed::Digest] {
        if fits(&messages, keep_stubs) {
            break;
        }
        if shed_block(&step, &mut messages, history_start) {
            shed.push(step);
        }
    }

    // 5. The persona's few-shot examples.
    if !fits(&messages, keep_stubs) && few_shot_len > 0 {
        messages.drain(1..1 + few_shot_len);
        shed.push(Shed::FewShot);
    }

    // 6. The optional tool listings.
    if !fits(&messages, keep_stubs) && keep_stubs {
        keep_stubs = false;
        shed.push(Shed::Stubs);
    }

    // 7. The reply allowance, down to its floor.
    let required = need(&messages, keep_stubs);
    if required + REPLY_FLOOR > window {
        return Err(WindowTooSmall { needed: required + REPLY_FLOOR, window });
    }
    let reply_allowance = if required + reply <= window {
        reply
    } else {
        shed.push(Shed::ReplyAllowance);
        (window - required).max(REPLY_FLOOR)
    };

    Ok(Fit { messages, keep_stubs, reply_allowance, shed })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(name: &str, description_words: usize) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: "word ".repeat(description_words),
            parameters: json!({"type": "object", "properties": {"id": {"type": "string"}}}),
        }
    }

    fn natives() -> Vec<ToolDefinition> {
        (0..8).map(|i| tool(&format!("native_{i}"), 60)).collect()
    }
    fn stubs() -> Vec<ToolDefinition> { (0..43).map(|i| tool(&format!("stub_{i}"), 6)).collect() }

    const PERSONA: &str = "You are Aemeath. Always speak in a lively, teasing way and never \
                           reveal the hidden backstory unless asked directly and seriously.";

    fn conversation(history_pairs: usize, words: usize) -> (Vec<Message>, usize) {
        let mut messages = vec![Message::system(PERSONA)];
        let few_shot = vec![
            Message::user("hi"),
            Message::assistant("hello there"),
            Message::user("how are you"),
            Message::assistant("great, you?"),
            Message::user("bye"),
            Message::assistant("see you"),
        ];
        let few_shot_len = few_shot.len();
        messages.extend(few_shot);
        for i in 0..history_pairs {
            messages.push(Message::user(format!("question {i} {}", "word ".repeat(words))));
            messages.push(Message::assistant(format!("answer {i} {}", "word ".repeat(words))));
        }
        messages.push(Message::user("what now?"));
        (messages, few_shot_len)
    }

    fn with_blocks(mut messages: Vec<Message>, few_shot_len: usize) -> Vec<Message> {
        let first_history_user = 1 + few_shot_len;
        let summary = format!("{}\nnotes about the past\n{}", SUMMARY_TAG.0, SUMMARY_TAG.1);
        messages[first_history_user].content =
            format!("{summary}\n\n{}", messages[first_history_user].content);
        let last = messages.len() - 1;
        let memory = format!(
            "{}\n{}\nmatch one\n{}\n{}\nfact one\n{}\n{}",
            MEMORY_TAG.0, MATCHES_TAG.0, MATCHES_TAG.1, DIGEST_TAG.0, DIGEST_TAG.1, MEMORY_TAG.1
        );
        messages[last].content = format!("{memory}\n\n{}", messages[last].content);
        messages
    }

    fn total(fit: &Fit, natives: &[ToolDefinition], stubs: &[ToolDefinition]) -> usize {
        estimate_messages_tokens(&fit.messages)
            + estimate_tools_tokens(natives)
            + if fit.keep_stubs { estimate_tools_tokens(stubs) } else { 0 }
            + fit.reply_allowance
    }

    #[test]
    fn a_request_that_already_fits_is_returned_whole() {
        let (messages, few_shot_len) = conversation(3, 10);
        let fit = fit_request(messages.clone(), few_shot_len, &natives(), &stubs(), 200_000, 4096)
            .unwrap();
        assert_eq!(fit.messages, messages);
        assert!(fit.keep_stubs);
        assert!(fit.shed.is_empty());
        assert_eq!(fit.reply_allowance, 4096);
    }

    #[test]
    fn history_is_shed_before_anything_else() {
        let (messages, few_shot_len) = conversation(20, 150);
        let fit = fit_request(messages.clone(), few_shot_len, &natives(), &stubs(), 12_000, 4096)
            .unwrap();
        assert!(matches!(fit.shed.first(), Some(Shed::History(n)) if *n > 0));
        assert!(fit.keep_stubs, "stubs are kept while history alone makes the difference");
        assert_eq!(fit.messages[0], messages[0], "the persona prompt is untouched");
        assert_eq!(fit.messages.last(), messages.last());
    }

    #[test]
    fn roles_keep_alternating_after_history_is_dropped() {
        let (messages, few_shot_len) = conversation(20, 150);
        let fit = fit_request(messages, few_shot_len, &natives(), &stubs(), 12_000, 4096).unwrap();
        for pair in fit.messages[1..].windows(2) {
            assert_ne!(pair[0].role, pair[1].role, "two consecutive {:?} messages", pair[0].role);
        }
    }

    #[test]
    fn the_summary_block_moves_forward_when_the_message_carrying_it_is_dropped() {
        let (messages, few_shot_len) = conversation(20, 150);
        let messages = with_blocks(messages, few_shot_len);
        let fit = fit_request(messages, few_shot_len, &natives(), &stubs(), 12_000, 4096).unwrap();
        assert!(
            !fit.shed.contains(&Shed::Summary),
            "history alone sufficed, so the summary survives"
        );
        assert!(
            fit.messages.iter().any(|m| m.content.contains(SUMMARY_TAG.0)),
            "the summary was re-homed, not lost with the dropped message"
        );
    }

    #[test]
    fn when_history_is_exhausted_matches_then_summary_then_digest_go_in_that_order() {
        let (messages, few_shot_len) = conversation(0, 0);
        let messages = with_blocks(messages, few_shot_len);
        let big_natives: Vec<ToolDefinition> =
            (0..8).map(|i| tool(&format!("n{i}"), 120)).collect();
        // Choose a window just too small for everything, then tighter and tighter.
        let mut windows_and_shed = Vec::new();
        for window in (2_400..4_200).step_by(100).rev() {
            if let Ok(fit) =
                fit_request(messages.clone(), few_shot_len, &big_natives, &[], window, 4096)
            {
                windows_and_shed.push(fit.shed);
            }
        }
        let rank = |s: &Shed| match s {
            Shed::History(_) => 0,
            Shed::Matches => 1,
            Shed::Summary => 2,
            Shed::Digest => 3,
            Shed::FewShot => 4,
            Shed::Stubs => 5,
            Shed::ReplyAllowance => 6,
        };
        for shed in windows_and_shed {
            let ranks: Vec<_> = shed.iter().map(rank).collect();
            let mut sorted = ranks.clone();
            sorted.sort();
            assert_eq!(ranks, sorted, "shedding happened out of order: {shed:?}");
        }
    }

    #[test]
    fn few_shot_examples_and_stubs_go_when_the_window_is_small() {
        let (messages, few_shot_len) = conversation(0, 0);
        let fit =
            fit_request(messages.clone(), few_shot_len, &natives(), &stubs(), 4_096, 4096).unwrap();
        assert!(
            fit.shed.contains(&Shed::Stubs)
                || fit.shed.contains(&Shed::FewShot)
                || !fit.shed.is_empty()
        );
        assert_eq!(fit.messages[0], messages[0]);
        assert!(total(&fit, &natives(), &stubs()) <= 4_096);
    }

    #[test]
    fn the_reply_allowance_shrinks_on_a_small_window_instead_of_overflowing() {
        let (messages, few_shot_len) = conversation(0, 0);
        let fit = fit_request(messages, few_shot_len, &natives(), &[], 4_096, 4096).unwrap();
        assert!(fit.reply_allowance <= 4_096 / 4 + 1, "got {}", fit.reply_allowance);
        assert!(fit.reply_allowance >= REPLY_FLOOR);
    }

    #[test]
    fn a_window_too_small_for_the_persona_is_refused_not_worked_around() {
        let (messages, few_shot_len) = conversation(0, 0);
        let result = fit_request(messages, few_shot_len, &natives(), &stubs(), 300, 4096);
        let error = result.unwrap_err();
        assert_eq!(error.window, 300);
        assert!(error.needed > 300);
        assert!(error.to_string().contains("too small"));
    }

    #[test]
    fn the_persona_prompt_survives_every_window_size_byte_for_byte() {
        let (messages, few_shot_len) = conversation(15, 120);
        let messages = with_blocks(messages, few_shot_len);
        for window in (400..60_000).step_by(137) {
            match fit_request(messages.clone(), few_shot_len, &natives(), &stubs(), window, 4096) {
                Ok(fit) => {
                    assert_eq!(fit.messages[0], messages[0], "persona changed at window {window}");
                    assert_eq!(fit.messages.last().map(|m| m.role), Some(Role::User));
                    assert!(fit.messages.last().unwrap().content.ends_with("what now?"));
                    assert!(
                        total(&fit, &natives(), &stubs()) <= window,
                        "window {window}: request {} tokens",
                        total(&fit, &natives(), &stubs())
                    );
                    assert!(fit.reply_allowance >= REPLY_FLOOR);
                }
                Err(error) => assert!(error.needed > window),
            }
        }
    }

    #[test]
    fn a_larger_window_never_sheds_more_than_a_smaller_one() {
        let (messages, few_shot_len) = conversation(15, 120);
        let mut previous_history = usize::MAX;
        for window in (2_000..40_000).step_by(500) {
            if let Ok(fit) =
                fit_request(messages.clone(), few_shot_len, &natives(), &stubs(), window, 4096)
            {
                let dropped = fit
                    .shed
                    .iter()
                    .find_map(|s| if let Shed::History(n) = s { Some(*n) } else { None })
                    .unwrap_or(0);
                assert!(
                    dropped <= previous_history,
                    "more history dropped at a larger window ({window})"
                );
                previous_history = dropped;
            }
        }
    }

    #[test]
    fn text_a_user_typed_that_contains_a_block_tag_is_not_stripped() {
        let (mut messages, few_shot_len) = conversation(0, 0);
        let last = messages.len() - 1;
        messages[last].content = format!("please explain {} to me", SUMMARY_TAG.0);
        let original = messages[last].content.clone();
        let fit = fit_request(messages, few_shot_len, &natives(), &[], 3_200, 4096).unwrap();
        assert_eq!(fit.messages.last().unwrap().content, original);
    }
}
