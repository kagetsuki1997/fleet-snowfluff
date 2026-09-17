//! `OpenAiCompatible`: covers OpenAI itself and any self-hosted or
//! third-party endpoint that speaks the same Chat Completions wire
//! format. Request-building and response-parsing are pure functions,
//! kept separate from the thin `reqwest`-based glue, so they're
//! unit-testable without a network -- the split `design.md` calls for.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    limits::MAX_RESPONSE_TOKENS,
    message::{Message, ModelInfo, ProviderError, ProviderKind, StreamChunk},
    provider::{AiProvider, ChatStream},
    providers::http_stream,
};

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiCompatible {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    client: reqwest::Client,
}

impl OpenAiCompatible {
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }
}

/// Builds the chat-completions request body. `Message`'s own
/// `Serialize` impl already matches this wire format exactly (`role`
/// values `system`/`user`/`assistant`), unlike Anthropic, which needs
/// `system`-role messages pulled out into a separate field.
pub fn build_chat_body(model: &str, messages: &[Message]) -> Value {
    json!({
        "model": model,
        "messages": messages,
        "max_tokens": MAX_RESPONSE_TOKENS,
        "stream": true,
    })
}

pub fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

pub fn models_url(base_url: &str) -> String { format!("{}/models", base_url.trim_end_matches('/')) }

/// Parses a non-2xx HTTP response body into a [`ProviderError`].
pub fn parse_error_response(status: u16, body: &str) -> ProviderError {
    #[derive(Deserialize)]
    struct ErrorEnvelope {
        error: ErrorDetail,
    }
    #[derive(Deserialize)]
    struct ErrorDetail {
        message: String,
    }

    let message = serde_json::from_str::<ErrorEnvelope>(body)
        .map(|e| e.error.message)
        .unwrap_or_else(|_| body.to_string());

    match status {
        401 | 403 => ProviderError::Auth(message),
        429 => ProviderError::RateLimited(message),
        _ => ProviderError::InvalidResponse(format!("HTTP {status}: {message}")),
    }
}

/// Parses one complete SSE line. `data: [DONE]`, non-`data:` lines
/// (blank separators), and deltas with no content all yield `Ok(None)`.
pub fn parse_sse_line(line: &str) -> Result<Option<StreamChunk>, ProviderError> {
    let Some(data) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) else {
        return Ok(None);
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }

    #[derive(Deserialize)]
    struct ChunkEnvelope {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        delta: Delta,
    }
    #[derive(Deserialize, Default)]
    struct Delta {
        #[serde(default)]
        content: Option<String>,
    }

    let envelope: ChunkEnvelope = serde_json::from_str(data)
        .map_err(|e| ProviderError::InvalidResponse(format!("malformed chunk: {e}")))?;

    Ok(envelope
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.delta.content)
        .filter(|s| !s.is_empty())
        .map(|delta| StreamChunk { delta }))
}

pub fn parse_model_list(body: &str) -> Result<Vec<ModelInfo>, ProviderError> {
    #[derive(Deserialize)]
    struct ModelListEnvelope {
        data: Vec<ModelEntry>,
    }
    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
    }

    let envelope: ModelListEnvelope = serde_json::from_str(body)
        .map_err(|e| ProviderError::InvalidResponse(format!("malformed model list: {e}")))?;
    Ok(envelope
        .data
        .into_iter()
        .map(|m| ModelInfo { display_name: m.id.clone(), id: m.id })
        .collect())
}

#[async_trait]
impl AiProvider for OpenAiCompatible {
    fn kind(&self) -> ProviderKind { ProviderKind::OpenAi }

    async fn chat(&self, messages: Vec<Message>) -> Result<ChatStream, ProviderError> {
        let request = self
            .client
            .post(chat_completions_url(&self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&build_chat_body(&self.model, &messages));
        http_stream::stream_lines(request, parse_sse_line, parse_error_response).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let request = self
            .client
            .get(models_url(&self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key));
        http_stream::fetch_and_parse(request, parse_model_list, parse_error_response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;

    #[test]
    fn build_chat_body_matches_openai_wire_format() {
        let body =
            build_chat_body("gpt-4o-mini", &[Message::system("be brief"), Message::user("hi")]);
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], MAX_RESPONSE_TOKENS);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hi");
    }

    #[test]
    fn parse_sse_line_extracts_content_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.delta, "Hello");
    }

    #[test]
    fn parse_sse_line_ignores_done_sentinel() {
        assert!(parse_sse_line("data: [DONE]").unwrap().is_none());
    }

    #[test]
    fn parse_sse_line_ignores_non_data_lines() {
        assert!(parse_sse_line("").unwrap().is_none());
        assert!(parse_sse_line(": comment").unwrap().is_none());
    }

    #[test]
    fn parse_sse_line_ignores_empty_finish_delta() {
        let line = r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        assert!(parse_sse_line(line).unwrap().is_none());
    }

    #[test]
    fn parse_sse_line_rejects_malformed_json() {
        let err = parse_sse_line("data: not json").unwrap_err();
        assert!(matches!(err, ProviderError::InvalidResponse(_)));
    }

    #[test]
    fn parse_error_response_maps_401_to_auth() {
        let body = r#"{"error":{"message":"Incorrect API key provided"}}"#;
        assert_eq!(
            parse_error_response(401, body),
            ProviderError::Auth("Incorrect API key provided".into())
        );
    }

    #[test]
    fn parse_error_response_maps_429_to_rate_limited() {
        let body = r#"{"error":{"message":"Rate limit reached"}}"#;
        assert!(matches!(parse_error_response(429, body), ProviderError::RateLimited(_)));
    }

    #[test]
    fn parse_error_response_falls_back_to_raw_body_on_unexpected_shape() {
        let err = parse_error_response(500, "internal server error");
        assert!(
            matches!(err, ProviderError::InvalidResponse(msg) if msg.contains("internal server error"))
        );
    }

    #[test]
    fn parse_model_list_extracts_ids() {
        let body = r#"{"object":"list","data":[{"id":"gpt-4o","object":"model"},{"id":"gpt-4o-mini","object":"model"}]}"#;
        let models = parse_model_list(body).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-4o");
    }

    #[test]
    fn role_serializes_as_expected_by_the_wire_format() {
        assert_eq!(serde_json::to_value(Role::System).unwrap(), "system");
        assert_eq!(serde_json::to_value(Role::Assistant).unwrap(), "assistant");
    }

    /// Manual, credential-gated round trip against the real API.
    /// Never run in CI: `OPENAI_API_KEY=sk-... cargo test -p
    /// fleet-snowfluff-ai --test-threads=1 -- --ignored openai_live`.
    #[tokio::test]
    #[ignore = "requires OPENAI_API_KEY and makes a real network call"]
    async fn openai_live_chat_round_trip() {
        use futures_util::StreamExt;

        let api_key = std::env::var("OPENAI_API_KEY")
            .expect("set OPENAI_API_KEY to run this ignored integration test");
        let provider = OpenAiCompatible::new(api_key, DEFAULT_BASE_URL, "gpt-4o-mini");
        let mut stream =
            provider.chat(vec![Message::user("Say \"hi\" and nothing else.")]).await.unwrap();

        let mut reply = String::new();
        while let Some(chunk) = stream.next().await {
            reply.push_str(&chunk.unwrap().delta);
        }
        assert!(!reply.is_empty(), "expected a non-empty reply from the real API");
    }
}
