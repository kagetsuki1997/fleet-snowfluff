//! A deliberately conservative token estimate, used to bound history, memory
//! blocks, tool output, and whole requests against a model's context window.
//!
//! A provider's real tokenizer is not available offline, so this counts
//! characters by class and multiplies by a safety factor. The weights were
//! fitted against the prompt token counts a local model (qwen3:14b) reported
//! for fourteen kinds of text, and the result never fell below the real
//! count on any of them (see `design.md`, "Spike results"). An earlier rule of
//! "ASCII at four characters per token, anything else one token" underestimated
//! structured text badly: JSON by 43%, log lines by 57%, digit runs by 75%.
//! Erring high means the budgets shed a little early; erring low would mean a
//! request that does not fit its window.

use serde_json::json;

use crate::{message::Message, tool_provider::ToolDefinition};

const LETTER: f64 = 0.25;
const DIGIT: f64 = 1.0;
const PUNCTUATION: f64 = 1.0;
const WHITESPACE: f64 = 0.1;
const NON_ASCII: f64 = 1.0;
const SAFETY_FACTOR: f64 = 1.15;

/// Role markers and message separators the chat template adds around each
/// message.
const PER_MESSAGE_OVERHEAD: usize = 6;

fn weight(c: char) -> f64 {
    if !c.is_ascii() {
        NON_ASCII
    } else if c.is_ascii_alphabetic() {
        LETTER
    } else if c.is_ascii_digit() {
        DIGIT
    } else if c.is_ascii_whitespace() {
        WHITESPACE
    } else {
        PUNCTUATION
    }
}

/// Estimated tokens for `text`.
pub fn estimate_tokens(text: &str) -> usize {
    let raw: f64 = text.chars().map(weight).sum();
    (raw * SAFETY_FACTOR).ceil() as usize
}

/// Estimated tokens for one message, including the tool calls an assistant turn
/// carries.
pub fn estimate_message_tokens(message: &Message) -> usize {
    let calls: usize = message
        .tool_calls
        .iter()
        .map(|call| estimate_tokens(&call.name) + estimate_tokens(&call.arguments.to_string()))
        .sum();
    PER_MESSAGE_OVERHEAD + estimate_tokens(&message.content) + calls
}

pub fn estimate_messages_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Estimated tokens for the tool definitions as they go on the wire (the
/// provider wraps each in a `{"type": "function", "function": {...}}`
/// envelope).
pub fn estimate_tools_tokens(tools: &[ToolDefinition]) -> usize {
    tools
        .iter()
        .map(|tool| {
            let wire = json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                },
            });
            estimate_tokens(&wire.to_string())
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct Calibration {
        samples: Vec<Sample>,
    }

    #[derive(Deserialize)]
    struct Sample {
        name: String,
        text: String,
        tokens: usize,
    }

    fn calibration() -> Calibration {
        serde_json::from_str(include_str!("../tests/fixtures/memory/token_calibration.json"))
            .expect("calibration fixture parses")
    }

    #[test]
    fn empty_text_is_zero_tokens() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn the_estimate_never_falls_below_the_measured_count_on_any_sample() {
        for sample in calibration().samples {
            let estimate = estimate_tokens(&sample.text);
            assert!(
                estimate >= sample.tokens,
                "{}: estimated {estimate} but the model counted {}",
                sample.name,
                sample.tokens
            );
        }
    }

    #[test]
    fn the_estimate_stays_within_twice_the_measured_count() {
        for sample in calibration().samples {
            let estimate = estimate_tokens(&sample.text);
            assert!(
                estimate <= sample.tokens * 2,
                "{}: estimated {estimate} for {} real tokens, too wasteful",
                sample.name,
                sample.tokens
            );
        }
    }

    #[test]
    fn structured_text_costs_more_per_character_than_prose() {
        let prose = "the quick brown fox jumps over the lazy dog ".repeat(20);
        let numbers = "1234567890 ".repeat(80);
        assert!(
            estimate_tokens(&numbers) as f64 / numbers.len() as f64
                > estimate_tokens(&prose) as f64 / prose.len() as f64
        );
    }

    #[test]
    fn cjk_characters_count_about_one_token_each() {
        let text = "今天想喝綠茶".repeat(10);
        let chars = text.chars().count();
        assert!(estimate_tokens(&text) >= chars);
    }

    #[test]
    fn a_message_costs_its_content_plus_a_fixed_overhead() {
        let message = Message::user("hello world");
        assert_eq!(
            estimate_message_tokens(&message),
            PER_MESSAGE_OVERHEAD + estimate_tokens("hello world")
        );
    }

    #[test]
    fn an_assistant_turn_with_tool_calls_counts_the_calls() {
        let plain = Message::assistant("");
        let with_call = Message::assistant_with_tool_calls(
            "",
            vec![crate::message::ToolCallRecord {
                id: "c1".into(),
                name: "web_search".into(),
                arguments: json!({"query": "weather in Taipei"}),
            }],
        );
        assert!(estimate_message_tokens(&with_call) > estimate_message_tokens(&plain));
    }

    #[test]
    fn tool_definitions_are_costed_with_their_wire_envelope() {
        let tool = ToolDefinition {
            name: "read_file".into(),
            description: "Reads the contents of a text file".into(),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        };
        let bare = estimate_tokens(&tool.description) + estimate_tokens(&tool.name);
        assert!(estimate_tools_tokens(&[tool]) > bare);
    }
}
