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
    /// Tokens a reply may use. The usual cap, unless the fit check lowered it
    /// for a small window.
    reply_allowance: u32,
}

impl Ollama {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            client: reqwest::Client::new(),
            reply_allowance: MAX_RESPONSE_TOKENS,
        }
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
    build_chat_with_tools_body_with(model, messages, tools, MAX_RESPONSE_TOKENS)
}

/// [`build_chat_with_tools_body`] with an explicit reply allowance
/// (`num_predict`).
pub fn build_chat_with_tools_body_with(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    reply_allowance: u32,
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
        "options": { "num_predict": reply_allowance },
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

pub fn ps_url(base_url: &str) -> String { format!("{}/api/ps", base_url.trim_end_matches('/')) }

pub fn generate_url(base_url: &str) -> String {
    format!("{}/api/generate", base_url.trim_end_matches('/'))
}

/// How long to wait for a model to load and report its window before giving up
/// on knowing it.
const WINDOW_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
/// How long a looked-up window is reused. It can change if the server is
/// restarted with another setting, so it is not kept indefinitely.
const WINDOW_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

type WindowKey = (String, String);

fn window_cache(
) -> &'static std::sync::Mutex<std::collections::HashMap<WindowKey, (usize, std::time::Instant)>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<WindowKey, (usize, std::time::Instant)>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn cached_window(key: &WindowKey) -> Option<usize> {
    let cache = window_cache().lock().ok()?;
    let (window, stored_at) = cache.get(key)?;
    (stored_at.elapsed() < WINDOW_CACHE_TTL).then_some(*window)
}

fn remember_window(key: WindowKey, window: usize) {
    if let Ok(mut cache) = window_cache().lock() {
        cache.insert(key, (window, std::time::Instant::now()));
    }
}

/// The context window the running model `model` was loaded with, from
/// `/api/ps`'s body. `/api/show` reports only the trained maximum, which is
/// larger than what the server actually loaded.
pub fn parse_running_window(body: &str, model: &str) -> Option<usize> {
    #[derive(Deserialize)]
    struct Running {
        #[serde(default)]
        models: Vec<RunningModel>,
    }
    #[derive(Deserialize)]
    struct RunningModel {
        #[serde(default)]
        name: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        context_length: Option<usize>,
    }
    fn base(name: &str) -> &str { name.strip_suffix(":latest").unwrap_or(name) }
    let wanted = base(model);
    let running: Running = serde_json::from_str(body).ok()?;
    running
        .models
        .into_iter()
        .find(|m| base(&m.name) == wanted || base(&m.model) == wanted)?
        .context_length
        .filter(|window| *window > 0)
}

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
        #[serde(default)]
        done: bool,
        #[serde(default)]
        prompt_eval_count: Option<usize>,
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

    let mut items = Vec::new();
    // The server's own count of the prompt it processed arrives on the final line.
    let prompt_tokens = chunk.done.then_some(chunk.prompt_eval_count).flatten();
    let Some(message) = chunk.message else {
        items.extend(prompt_tokens.map(ToolCallStreamItem::PromptTokens));
        return Ok(items);
    };

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
    items.extend(prompt_tokens.map(ToolCallStreamItem::PromptTokens));
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
        let request = self.client.post(chat_url(&self.base_url)).json(
            &build_chat_with_tools_body_with(&self.model, &messages, &tools, self.reply_allowance),
        );
        http_stream::stream_lines_multi(request, parse_ndjson_tool_line, parse_error_response).await
    }

    fn set_reply_allowance(&mut self, tokens: u32) { self.reply_allowance = tokens; }

    async fn context_window(&self) -> Option<usize> {
        let key = (self.base_url.clone(), self.model.clone());
        if let Some(window) = cached_window(&key) {
            return Some(window);
        }
        let window = tokio::time::timeout(WINDOW_LOOKUP_TIMEOUT, self.load_and_read_window())
            .await
            .ok()
            .flatten()?;
        remember_window(key, window);
        Some(window)
    }
}

impl Ollama {
    async fn running_window(&self) -> Option<usize> {
        let body = self.client.get(ps_url(&self.base_url)).send().await.ok()?.text().await.ok()?;
        parse_running_window(&body, &self.model)
    }

    /// The window of the loaded model. If it is not loaded yet, loads it with
    /// an empty generate call (which Ollama documents as a load-only
    /// request) and asks again. A model load the first chat request would
    /// have needed anyway.
    async fn load_and_read_window(&self) -> Option<usize> {
        if let Some(window) = self.running_window().await {
            return Some(window);
        }
        let load = json!({ "model": self.model, "prompt": "", "stream": false });
        self.client.post(generate_url(&self.base_url)).json(&load).send().await.ok()?;
        self.running_window().await
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
    fn the_reply_allowance_is_what_the_request_asks_the_model_for() {
        let body = build_chat_with_tools_body_with("m", &[Message::user("hi")], &[], 1_024);
        assert_eq!(body["options"]["num_predict"], 1_024);
        let default = build_chat_with_tools_body("m", &[Message::user("hi")], &[]);
        assert_eq!(default["options"]["num_predict"], MAX_RESPONSE_TOKENS);
    }

    #[test]
    fn a_provider_remembers_a_lowered_reply_allowance() {
        let mut provider = Ollama::new("http://localhost:11434", "m");
        provider.set_reply_allowance(700);
        assert_eq!(provider.reply_allowance, 700);
    }

    #[test]
    fn the_running_window_comes_from_the_loaded_model_not_the_trained_maximum() {
        let body = r#"{"models":[{"name":"qwen3:14b","model":"qwen3:14b","size":14424117083,"context_length":32768}]}"#;
        assert_eq!(parse_running_window(body, "qwen3:14b"), Some(32768));
    }

    #[test]
    fn a_model_name_without_a_tag_matches_its_latest_build() {
        let body = r#"{"models":[{"name":"llama3.2:latest","model":"llama3.2:latest","context_length":4096}]}"#;
        assert_eq!(parse_running_window(body, "llama3.2"), Some(4096));
    }

    #[test]
    fn no_window_is_reported_when_the_model_is_not_loaded_or_the_field_is_missing() {
        assert_eq!(parse_running_window(r#"{"models":[]}"#, "qwen3:14b"), None);
        assert_eq!(
            parse_running_window(
                r#"{"models":[{"name":"other:1b","context_length":2048}]}"#,
                "qwen3:14b"
            ),
            None
        );
        assert_eq!(parse_running_window(r#"{"models":[{"name":"qwen3:14b"}]}"#, "qwen3:14b"), None);
        assert_eq!(parse_running_window("not json", "qwen3:14b"), None);
    }

    #[test]
    fn the_final_stream_line_reports_the_prompt_size() {
        let line = r#"{"model":"m","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":3334,"eval_count":21}"#;
        let items = parse_ndjson_tool_line(line).unwrap();
        assert_eq!(items, vec![ToolCallStreamItem::PromptTokens(3334)]);
    }

    #[test]
    fn a_done_line_without_a_prompt_count_adds_nothing() {
        let line = r#"{"model":"m","message":{"role":"assistant","content":""},"done":true}"#;
        assert_eq!(parse_ndjson_tool_line(line).unwrap(), vec![]);
    }

    #[test]
    fn an_ordinary_mid_stream_line_never_reports_a_prompt_size() {
        let line = r#"{"model":"m","message":{"role":"assistant","content":"hi"},"done":false,"prompt_eval_count":99}"#;
        assert_eq!(
            parse_ndjson_tool_line(line).unwrap(),
            vec![ToolCallStreamItem::TextDelta("hi".to_string())]
        );
    }

    #[tokio::test]
    async fn context_window_reads_a_loaded_models_window_and_reuses_it() {
        let (base_url, handle) = crate::providers::test_server::serve_once(
            200,
            crate::providers::test_server::chunked(
                r#"{"models":[{"name":"qwen3:14b","model":"qwen3:14b","context_length":32768}]}"#,
                4096,
            ),
        );
        let provider = Ollama::new(base_url, "qwen3:14b");

        assert_eq!(provider.context_window().await, Some(32768));
        handle.join().unwrap();
        // The server has stopped; a second ask is answered from the cache.
        assert_eq!(provider.context_window().await, Some(32768));
    }

    #[test]
    fn build_chat_body_never_sets_num_ctx() {
        // Deliberately not set -- see limits.rs's own comment on why
        // this crate must not second-guess Ollama's own VRAM-aware
        // auto-detected default with a hardcoded value.
        let body = build_chat_body("llama3.2:3b", &[Message::user("hi")]);
        assert!(body["options"].get("num_ctx").is_none());
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
        assert!(body["options"].get("num_ctx").is_none());
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
