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
    tool_provider::{ToolCallStream, ToolCallStreamItem, ToolCallingProvider, ToolDefinition},
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
///
/// `"think": false` is load-bearing, not cosmetic: a "thinking"-capable
/// local model (confirmed against a real Ollama instance running
/// `qwen3:8b`) streams its chain-of-thought into a separate `thinking`
/// field while `message.content` stays empty for the entire reasoning
/// phase -- and `MAX_RESPONSE_TOKENS` counts *all* generated tokens,
/// reasoning included. A verbose thinking phase can consume the whole
/// budget before any visible text is ever produced, so the request
/// "succeeds" with zero real content and the chat silently shows an
/// empty reply. Disabling thinking also fits the persona itself far
/// better -- a short in-character quip has no use for exposed
/// reasoning, visible or not.
pub fn build_chat_body(model: &str, messages: &[Message]) -> Value {
    json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "think": false,
        "options": { "num_predict": MAX_RESPONSE_TOKENS },
    })
}

/// `chat_with_tools`'s own request body, kept as a separate function
/// from [`build_chat_body`] rather than adding an optional `tools`
/// parameter to it -- every plain-chat call site (`AiProvider::chat`)
/// would otherwise have to pass `&[]` for a field it never uses.
/// `"think": false` is required here for a second, stronger reason than
/// plain chat's own (see this module's doc comment): [ollama/ollama#10976](https://github.com/ollama/ollama/issues/10976)
/// documents `think: true` combined with tool definitions producing
/// **empty output entirely** for Qwen3 models, independent of budget
/// size -- not just "wastes the budget on invisible reasoning" like the
/// plain-chat case.
pub fn build_chat_with_tools_body(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Value {
    // Ollama pairs a tool result with its call by order and has no
    // `tool_call_id`; `Message` now carries one for the APIs that need
    // it (OpenAI, Anthropic), so drop it here to keep Ollama's request
    // exactly what it was before that field existed.
    let mut messages_json =
        serde_json::to_value(messages).expect("Message serialization is infallible");
    if let Value::Array(items) = &mut messages_json {
        for item in items {
            if let Some(object) = item.as_object_mut() {
                object.remove("tool_call_id");
            }
        }
    }
    let mut body = json!({
        "model": model,
        "messages": messages_json,
        "stream": true,
        "think": false,
        "options": { "num_predict": MAX_RESPONSE_TOKENS },
    });
    if !tools.is_empty() {
        let tools_json: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    },
                })
            })
            .collect();
        body["tools"] = Value::Array(tools_json);
    }
    body
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

/// `chat_with_tools`'s own line parser -- distinct from
/// [`parse_ndjson_line`] because that one only deserializes
/// `message.content`/`done` and would silently drop a `tool_calls`
/// field if one appeared. Returns every item a single line carries
/// (zero, one, or several) rather than `Option<T>`, since a line can in
/// principle carry both a text delta and one or more tool calls at
/// once. Ollama's own tool-call objects have no `id` field (unlike
/// OpenAI's) -- one is synthesized from the call's position in the
/// array, which is stable within a single line/turn.
pub fn parse_ndjson_tool_line(line: &str) -> Result<Vec<ToolCallStreamItem>, ProviderError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(vec![]);
    }

    #[derive(Deserialize)]
    struct ChatToolChunk {
        #[serde(default)]
        message: Option<ToolMessageDelta>,
    }
    #[derive(Deserialize)]
    struct ToolMessageDelta {
        #[serde(default)]
        content: String,
        #[serde(default)]
        tool_calls: Vec<OllamaToolCall>,
    }
    #[derive(Deserialize)]
    struct OllamaToolCall {
        #[serde(default)]
        id: Option<String>,
        function: OllamaFunctionCall,
    }
    #[derive(Deserialize)]
    struct OllamaFunctionCall {
        name: String,
        #[serde(default)]
        arguments: Value,
    }

    let chunk: ChatToolChunk = serde_json::from_str(line)
        .map_err(|e| ProviderError::InvalidResponse(format!("malformed chunk: {e}")))?;

    let Some(message) = chunk.message else {
        return Ok(vec![]);
    };

    let mut items = Vec::new();
    if !message.content.is_empty() {
        items.push(ToolCallStreamItem::TextDelta(message.content));
    }
    for (index, call) in message.tool_calls.into_iter().enumerate() {
        let id = call.id.unwrap_or_else(|| format!("call_{index}"));
        items.push(ToolCallStreamItem::ToolCall {
            id,
            name: call.function.name,
            arguments: call.function.arguments,
        });
    }
    Ok(items)
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

#[async_trait]
impl ToolCallingProvider for Ollama {
    async fn chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ToolCallStream, ProviderError> {
        let request = self.client.post(chat_url(&self.base_url)).json(&build_chat_with_tools_body(
            &self.model,
            &messages,
            &tools,
        ));
        http_stream::stream_lines_multi(request, parse_ndjson_tool_line, parse_error_response).await
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
    fn build_chat_body_disables_thinking() {
        // A "thinking"-capable model (e.g. qwen3) puts its reasoning in
        // a separate field while `content` stays empty for the whole
        // reasoning phase -- without this, MAX_RESPONSE_TOKENS can be
        // entirely consumed by invisible reasoning, yielding a reply
        // that "succeeds" with zero visible content.
        let body = build_chat_body("qwen3:8b", &[Message::user("hi")]);
        assert_eq!(body["think"], false);
    }

    #[test]
    fn parse_ndjson_line_extracts_content() {
        let line =
            r#"{"model":"llama3.2","message":{"role":"assistant","content":"Hello"},"done":false}"#;
        let chunk = parse_ndjson_line(line).unwrap().unwrap();
        assert_eq!(chunk.delta, "Hello");
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
    fn build_chat_with_tools_body_disables_thinking_even_with_tools_present() {
        let tools = vec![ToolDefinition {
            name: "get_current_weather".into(),
            description: "Get the current weather for a location".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }];
        let body = build_chat_with_tools_body("qwen3:8b", &[Message::user("hi")], &tools);
        assert_eq!(body["think"], false);
        assert_eq!(body["tools"][0]["function"]["name"], "get_current_weather");
    }

    #[test]
    fn build_chat_with_tools_body_never_sends_a_tool_call_id_to_ollama() {
        let messages = [
            Message::user("what is 6*7?"),
            Message::assistant_with_tool_calls(
                "",
                vec![crate::message::ToolCallRecord {
                    id: "call_1".into(),
                    name: "calc".into(),
                    arguments: json!({"expr": "6*7"}),
                }],
            ),
            Message::tool_result("call_1", "42"),
        ];
        let body = build_chat_with_tools_body("qwen3:8b", &messages, &[]);
        assert!(
            !body.to_string().contains("tool_call_id"),
            "Ollama has no such field, so it must not be sent: {body}"
        );
        // The result itself is still there, in Ollama's own shape.
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["content"], "42");
    }

    #[test]
    fn build_chat_with_tools_body_omits_tools_field_when_empty() {
        let body = build_chat_with_tools_body("llama3.2", &[Message::user("hi")], &[]);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn parse_ndjson_tool_line_extracts_a_text_delta() {
        let line =
            r#"{"model":"llama3.2","message":{"role":"assistant","content":"Hello"},"done":false}"#;
        let items = parse_ndjson_tool_line(line).unwrap();
        assert_eq!(items, vec![ToolCallStreamItem::TextDelta("Hello".to_string())]);
    }

    #[test]
    fn parse_ndjson_tool_line_extracts_a_tool_call_per_ollamas_documented_shape() {
        // Ollama's own tool-call response shape has no `id` field on
        // each entry (unlike OpenAI's) -- `parse_ndjson_tool_line`
        // synthesizes one from position instead.
        let line = r#"{"model":"llama3.2","message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"get_current_weather","arguments":{"format":"celsius","location":"Paris, FR"}}}]},"done":false}"#;
        let items = parse_ndjson_tool_line(line).unwrap();
        assert_eq!(
            items,
            vec![ToolCallStreamItem::ToolCall {
                id: "call_0".to_string(),
                name: "get_current_weather".to_string(),
                arguments: json!({"format": "celsius", "location": "Paris, FR"}),
            }]
        );
    }

    #[test]
    fn parse_ndjson_tool_line_returns_empty_on_a_done_line_with_no_message() {
        let line = r#"{"model":"llama3.2","done":true,"total_duration":123}"#;
        assert_eq!(parse_ndjson_tool_line(line).unwrap(), vec![]);
    }

    #[test]
    fn parse_ndjson_tool_line_ignores_blank_lines() {
        assert_eq!(parse_ndjson_tool_line("").unwrap(), vec![]);
    }

    #[test]
    fn parse_ndjson_tool_line_rejects_malformed_json() {
        let err = parse_ndjson_tool_line("not json").unwrap_err();
        assert!(matches!(err, ProviderError::InvalidResponse(_)));
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
