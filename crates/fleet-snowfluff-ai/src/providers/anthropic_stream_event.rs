//! Shared parsing for Anthropic's native Messages-API streaming event
//! shape (`message_start`, `content_block_start`/`stop`,
//! `content_block_delta` with a `text_delta`, `message_delta`,
//! `message_stop`, `ping`, `error`) -- used by both `Anthropic` (where
//! this shape arrives as raw SSE `data:` lines) and `ClaudeCodeCli`
//! (where the identical shape arrives wrapped as the `event` field of
//! `claude -p --output-format stream-json`'s own JSONL lines). Kept in
//! one place so a future change to either wire format's event
//! structure only needs updating once.

use serde_json::Value;

use crate::message::{ProviderError, StreamChunk};

/// Extracts visible text from one already-JSON-parsed Anthropic stream
/// event. Only `content_block_delta` events with a `text_delta` carry
/// visible text; every other event type yields `Ok(None)`. An `error`
/// event is surfaced as a real error rather than silently dropped.
pub fn extract_chunk(value: &Value) -> Result<Option<StreamChunk>, ProviderError> {
    match value.get("type").and_then(Value::as_str) {
        Some("content_block_delta") => Ok(value
            .get("delta")
            .and_then(|d| d.get("text"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|text| StreamChunk { delta: text.to_string() })),
        Some("error") => {
            let message = value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string();
            Err(ProviderError::InvalidResponse(message))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn extracts_text_delta() {
        let event = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": "Hello" }
        });
        let chunk = extract_chunk(&event).unwrap().unwrap();
        assert_eq!(chunk.delta, "Hello");
    }

    #[test]
    fn ignores_non_content_events() {
        assert!(extract_chunk(&json!({ "type": "message_start" })).unwrap().is_none());
        assert!(extract_chunk(&json!({ "type": "message_stop" })).unwrap().is_none());
        assert!(extract_chunk(&json!({ "type": "ping" })).unwrap().is_none());
    }

    #[test]
    fn surfaces_error_events() {
        let event = json!({ "type": "error", "error": { "type": "overloaded_error", "message": "servers overloaded" } });
        let err = extract_chunk(&event).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidResponse(msg) if msg == "servers overloaded"));
    }
}
