//! `Anthropic`: Claude's native Messages API. A distinct implementation
//! from `OpenAiCompatible` on purpose -- different auth header
//! (`x-api-key` + `anthropic-version`, not `Authorization: Bearer`),
//! system prompt as a top-level field rather than a `system`-role
//! message, and a differently-shaped SSE event stream (typed
//! `content_block_delta` events, not a flat `choices[0].delta`).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    limits::MAX_RESPONSE_TOKENS,
    message::{Message, ModelInfo, ProviderError, ProviderKind, Role, StreamChunk},
    provider::{AiProvider, ChatStream},
    providers::{anthropic_stream_event, http_stream},
};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";
/// Confirmed against Anthropic's own API docs during design (the
/// `List Models` reference example uses this exact value).
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// API-key-only, exactly as in Stage 1. Subscription auth for Anthropic
/// is a separate implementation, `providers::claude_code_cli::ClaudeCodeCli`
/// (subprocess-wrapped `claude -p`) -- an earlier version of this
/// struct grew an `AnthropicAuth`/subscription variant that sent a
/// `claude setup-token` bearer token straight to this same HTTP client,
/// which turned out to be both rejected by the Messages API and a
/// Consumer Terms of Service violation for subscription-sourced OAuth
/// tokens. See `subscription-first-chat`'s design.md for the corrected
/// architecture.
pub struct Anthropic {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    client: reqwest::Client,
}

impl Anthropic {
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

/// Builds the Messages-API request body: `system`-role messages are
/// pulled out of `messages` and joined into the top-level `system`
/// field (Anthropic rejects a `system` role inside the `messages`
/// array), everything else becomes a `user`/`assistant` turn.
pub fn build_chat_body(model: &str, messages: &[Message]) -> Value {
    let system_prompt = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let conversation: Vec<Value> = messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => unreachable!("filtered out above"),
            };
            json!({ "role": role, "content": m.content })
        })
        .collect();

    let mut body = json!({
        "model": model,
        "max_tokens": MAX_RESPONSE_TOKENS,
        "messages": conversation,
        "stream": true,
    });
    if !system_prompt.is_empty() {
        body["system"] = Value::String(system_prompt);
    }
    body
}

pub fn messages_url(base_url: &str) -> String {
    format!("{}/messages", base_url.trim_end_matches('/'))
}

pub fn models_url(base_url: &str) -> String {
    format!("{}/models?limit=1000", base_url.trim_end_matches('/'))
}

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

/// Parses one complete SSE line, delegating the actual event-shape
/// matching to `anthropic_stream_event::extract_chunk` (shared with
/// `ClaudeCodeCli`, which sees the identical event shape wrapped
/// differently).
pub fn parse_sse_line(line: &str) -> Result<Option<StreamChunk>, ProviderError> {
    let Some(data) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) else {
        return Ok(None);
    };
    let data = data.trim();
    if data.is_empty() {
        return Ok(None);
    }

    let value: Value = serde_json::from_str(data)
        .map_err(|e| ProviderError::InvalidResponse(format!("malformed event: {e}")))?;
    anthropic_stream_event::extract_chunk(&value)
}

pub fn parse_model_list(body: &str) -> Result<Vec<ModelInfo>, ProviderError> {
    #[derive(Deserialize)]
    struct ModelListEnvelope {
        data: Vec<ModelEntry>,
    }
    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
        display_name: String,
    }

    let envelope: ModelListEnvelope = serde_json::from_str(body)
        .map_err(|e| ProviderError::InvalidResponse(format!("malformed model list: {e}")))?;
    Ok(envelope
        .data
        .into_iter()
        .map(|m| ModelInfo { id: m.id, display_name: m.display_name })
        .collect())
}

#[async_trait]
impl AiProvider for Anthropic {
    fn kind(&self) -> ProviderKind { ProviderKind::Anthropic }

    async fn chat(&self, messages: Vec<Message>) -> Result<ChatStream, ProviderError> {
        let request = self
            .client
            .post(messages_url(&self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&build_chat_body(&self.model, &messages));
        http_stream::stream_lines(request, parse_sse_line, parse_error_response).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let request = self
            .client
            .get(models_url(&self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION);
        http_stream::fetch_and_parse(request, parse_model_list, parse_error_response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_role_messages_move_to_the_top_level_system_field() {
        let body =
            build_chat_body("claude-sonnet-5", &[Message::system("be brief"), Message::user("hi")]);
        assert_eq!(body["system"], "be brief");
        assert_eq!(
            body["messages"].as_array().unwrap().len(),
            1,
            "system message must not stay in messages"
        );
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn no_system_field_when_there_is_no_system_message() {
        let body = build_chat_body("claude-sonnet-5", &[Message::user("hi")]);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn multiple_system_messages_are_joined() {
        let body = build_chat_body(
            "claude-sonnet-5",
            &[Message::system("part one"), Message::system("part two")],
        );
        assert_eq!(body["system"], "part one\n\npart two");
    }

    #[test]
    fn parse_sse_line_extracts_text_delta() {
        let line = r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.delta, "Hello");
    }

    #[test]
    fn parse_sse_line_ignores_non_content_events() {
        assert!(parse_sse_line(r#"data: {"type":"message_start"}"#).unwrap().is_none());
        assert!(parse_sse_line(r#"data: {"type":"message_stop"}"#).unwrap().is_none());
        assert!(parse_sse_line("event: content_block_delta").unwrap().is_none());
    }

    #[test]
    fn parse_sse_line_surfaces_error_events() {
        let line = r#"data: {"type":"error","error":{"type":"overloaded_error","message":"servers overloaded"}}"#;
        let err = parse_sse_line(line).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidResponse(msg) if msg == "servers overloaded"));
    }

    #[test]
    fn parse_error_response_maps_401_to_auth() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        assert_eq!(
            parse_error_response(401, body),
            ProviderError::Auth("invalid x-api-key".into())
        );
    }

    #[test]
    fn parse_model_list_extracts_id_and_display_name() {
        let body = r#"{"data":[{"type":"model","id":"claude-opus-5","display_name":"Claude Opus 5"}],"has_more":false}"#;
        let models = parse_model_list(body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-opus-5");
        assert_eq!(models[0].display_name, "Claude Opus 5");
    }

    /// Manual, credential-gated round trip against the real API.
    /// Never run in CI: `ANTHROPIC_API_KEY=sk-ant-... cargo test -p
    /// fleet-snowfluff-ai --test-threads=1 -- --ignored anthropic_live`.
    #[tokio::test]
    #[ignore = "requires ANTHROPIC_API_KEY and makes a real network call"]
    async fn anthropic_live_chat_round_trip() {
        use futures_util::StreamExt;

        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .expect("set ANTHROPIC_API_KEY to run this ignored integration test");
        let provider = Anthropic::new(api_key, DEFAULT_BASE_URL, "claude-haiku-4-5-20251001");
        let mut stream =
            provider.chat(vec![Message::user("Say \"hi\" and nothing else.")]).await.unwrap();

        let mut reply = String::new();
        while let Some(chunk) = stream.next().await {
            reply.push_str(&chunk.unwrap().delta);
        }
        assert!(!reply.is_empty(), "expected a non-empty reply from the real API");
    }
}
