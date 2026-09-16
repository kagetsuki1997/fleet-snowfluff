//! `Ollama`: local HTTP server, no authentication. Streams newline-
//! delimited JSON objects (not SSE's `data:`-prefixed lines), and caps
//! output length via `options.num_predict` rather than a top-level
//! `max_tokens` field.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    limits::MAX_RESPONSE_TOKENS,
    message::{Message, ModelInfo, ProviderError, ProviderKind, StreamChunk},
    provider::{AiProvider, ChatStream},
    providers::http_stream,
};

pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";

pub struct Ollama {
    pub base_url: String,
    pub model: String,
    client: reqwest::Client,
}

impl Ollama {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self { base_url: base_url.into(), model: model.into(), client: reqwest::Client::new() }
    }
}

/// `Message`'s own `Serialize` impl matches Ollama's `role` values
/// (`system`/`user`/`assistant`) exactly, same as OpenAI-compatible --
/// no per-role remapping needed, unlike Anthropic.
pub fn build_chat_body(model: &str, messages: &[Message]) -> Value {
    json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "options": { "num_predict": MAX_RESPONSE_TOKENS },
    })
}

pub fn chat_url(base_url: &str) -> String { format!("{}/api/chat", base_url.trim_end_matches('/')) }

pub fn tags_url(base_url: &str) -> String { format!("{}/api/tags", base_url.trim_end_matches('/')) }

/// Ollama's error body is `{"error": "<message>"}`, not a nested
/// envelope like OpenAI/Anthropic's.
pub fn parse_error_response(status: u16, body: &str) -> ProviderError {
    #[derive(Deserialize)]
    struct ErrorEnvelope {
        error: String,
    }

    let message = serde_json::from_str::<ErrorEnvelope>(body)
        .map(|e| e.error)
        .unwrap_or_else(|_| body.to_string());

    match status {
        401 | 403 => ProviderError::Auth(message),
        429 => ProviderError::RateLimited(message),
        _ => ProviderError::InvalidResponse(format!("HTTP {status}: {message}")),
    }
}

/// Parses one complete NDJSON line -- no `data:` prefix, one JSON
/// object per line, a `"done": true` line closes the stream with no
/// further content.
pub fn parse_ndjson_line(line: &str) -> Result<Option<StreamChunk>, ProviderError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }

    #[derive(Deserialize)]
    struct ChatChunk {
        #[serde(default)]
        message: Option<MessageDelta>,
        done: bool,
    }
    #[derive(Deserialize)]
    struct MessageDelta {
        content: String,
    }

    let chunk: ChatChunk = serde_json::from_str(line)
        .map_err(|e| ProviderError::InvalidResponse(format!("malformed chunk: {e}")))?;

    if chunk.done {
        return Ok(None);
    }
    Ok(chunk
        .message
        .and_then(|m| (!m.content.is_empty()).then_some(StreamChunk { delta: m.content })))
}

pub fn parse_model_list(body: &str) -> Result<Vec<ModelInfo>, ProviderError> {
    #[derive(Deserialize)]
    struct TagsEnvelope {
        models: Vec<TagEntry>,
    }
    #[derive(Deserialize)]
    struct TagEntry {
        name: String,
    }

    let envelope: TagsEnvelope = serde_json::from_str(body)
        .map_err(|e| ProviderError::InvalidResponse(format!("malformed model list: {e}")))?;
    Ok(envelope
        .models
        .into_iter()
        .map(|m| ModelInfo { display_name: m.name.clone(), id: m.name })
        .collect())
}

#[async_trait]
impl AiProvider for Ollama {
    fn kind(&self) -> ProviderKind { ProviderKind::Ollama }

    async fn chat(&self, messages: Vec<Message>) -> Result<ChatStream, ProviderError> {
        let request = self
            .client
            .post(chat_url(&self.base_url))
            .json(&build_chat_body(&self.model, &messages));
        http_stream::stream_lines(request, parse_ndjson_line, parse_error_response).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let request = self.client.get(tags_url(&self.base_url));
        http_stream::fetch_and_parse(request, parse_model_list, parse_error_response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_chat_body_caps_output_via_num_predict() {
        let body = build_chat_body("llama3.2:3b", &[Message::user("hi")]);
        assert_eq!(body["model"], "llama3.2:3b");
        assert_eq!(body["options"]["num_predict"], MAX_RESPONSE_TOKENS);
        assert!(body.get("max_tokens").is_none(), "Ollama has no top-level max_tokens field");
    }

    #[test]
    fn parse_ndjson_line_extracts_content() {
        let line =
            r#"{"model":"llama3.2","message":{"role":"assistant","content":"Hel"},"done":false}"#;
        let chunk = parse_ndjson_line(line).unwrap().unwrap();
        assert_eq!(chunk.delta, "Hel");
    }

    #[test]
    fn parse_ndjson_line_returns_none_on_done() {
        let line = r#"{"model":"llama3.2","done":true,"total_duration":123}"#;
        assert!(parse_ndjson_line(line).unwrap().is_none());
    }

    #[test]
    fn parse_ndjson_line_ignores_blank_lines() {
        assert!(parse_ndjson_line("").unwrap().is_none());
    }

    #[test]
    fn parse_ndjson_line_rejects_malformed_json() {
        let err = parse_ndjson_line("not json").unwrap_err();
        assert!(matches!(err, ProviderError::InvalidResponse(_)));
    }

    #[test]
    fn parse_error_response_extracts_flat_error_field() {
        let body = r#"{"error":"model 'xyz' not found, try pulling it first"}"#;
        let err = parse_error_response(404, body);
        assert!(matches!(err, ProviderError::InvalidResponse(msg) if msg.contains("not found")));
    }

    #[test]
    fn parse_model_list_extracts_local_tags() {
        let body = r#"{"models":[{"name":"llama3.2:3b","model":"llama3.2:3b","modified_at":"2026-01-01T00:00:00Z","size":123}]}"#;
        let models = parse_model_list(body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "llama3.2:3b");
    }

    /// Manual integration test requiring a locally running Ollama with
    /// `llama3.2` pulled. Never run in CI: `cargo test -p
    /// fleet-snowfluff-ai --test-threads=1 -- --ignored ollama_live`.
    #[tokio::test]
    #[ignore = "requires a locally running Ollama with llama3.2 pulled"]
    async fn ollama_live_chat_round_trip() {
        use futures_util::StreamExt;

        let provider = Ollama::new(DEFAULT_BASE_URL, "llama3.2");
        let mut stream =
            provider.chat(vec![Message::user("Say \"hi\" and nothing else.")]).await.unwrap();

        let mut reply = String::new();
        while let Some(chunk) = stream.next().await {
            reply.push_str(&chunk.unwrap().delta);
        }
        assert!(!reply.is_empty(), "expected a non-empty reply from local Ollama");
    }
}
