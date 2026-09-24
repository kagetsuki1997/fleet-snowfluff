//! `OpenAiCompatible`: covers OpenAI itself and any self-hosted or
//! third-party endpoint that speaks the same Chat Completions wire
//! format. Request-building and response-parsing are pure functions,
//! kept separate from the thin `reqwest`-based glue, so they're
//! unit-testable without a network -- the split `design.md` calls for.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    limits::MAX_RESPONSE_TOKENS,
    message::{Message, ModelInfo, ProviderError, ProviderKind, Role, StreamChunk},
    provider::{AiProvider, ChatStream},
    providers::http_stream::{self, LineParser},
    tool_provider::{ToolCallStream, ToolCallStreamItem, ToolCallingProvider, ToolDefinition},
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

/// One message in OpenAI's own shape, for a request that carries
/// tools. `Message`'s serialized form is not OpenAI's for a tool-call
/// turn (`{id, name, arguments}` vs `{id, type: "function", function:
/// {name, arguments: <JSON string>}}`), so this is a real mapping, not a
/// pass-through -- only the tool-aware request path uses it; plain chat
/// keeps serializing `Message` directly.
fn to_openai_message(message: &Message) -> Value {
    match message.role {
        Role::Assistant if !message.tool_calls.is_empty() => {
            let tool_calls: Vec<Value> = message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            // The API wants the arguments as a JSON *string*.
                            "arguments": call.arguments.to_string(),
                        },
                    })
                })
                .collect();
            json!({
                "role": "assistant",
                // `null`, not "", when the model said nothing before calling.
                "content": if message.content.is_empty() { Value::Null } else { json!(message.content) },
                "tool_calls": tool_calls,
            })
        }
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": message.tool_call_id.clone().unwrap_or_default(),
            "content": message.content,
        }),
        Role::System => json!({ "role": "system", "content": message.content }),
        Role::User => json!({ "role": "user", "content": message.content }),
        Role::Assistant => json!({ "role": "assistant", "content": message.content }),
    }
}

/// The chat-completions request body for a request that offers `tools`
/// -- the tool-aware counterpart of [`build_chat_body`], which is left
/// untouched so plain chat (and any custom endpoint) is unaffected. With
/// no tools the `tools` field is omitted entirely.
pub fn build_chat_with_tools_body(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Value {
    // `max_completion_tokens`, not `max_tokens`: OpenAI's own API spec marks
    // `max_tokens` on chat completions "deprecated in favor of
    // `max_completion_tokens`" and "not compatible with o-series models".
    // Only the provider's own endpoint gets this builder (custom endpoints
    // stay on plain chat -- `supports_tool_calling`), and every chat turn
    // for a default-endpoint profile goes through it, so it has to be the
    // parameter that endpoint accepts for every model. The plain builder
    // keeps `max_tokens`, which is what OpenAI-compatible servers expect.
    let mut body = json!({
        "model": model,
        "messages": messages.iter().map(to_openai_message).collect::<Vec<_>>(),
        "max_completion_tokens": MAX_RESPONSE_TOKENS,
        "stream": true,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
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
                .collect(),
        );
    }
    body
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

/// One tool call being assembled from streamed fragments.
#[derive(Default)]
struct PartialCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// Turns OpenAI's streamed chunks into [`ToolCallStreamItem`]s.
///
/// Text deltas pass straight through. Tool calls do not: each arrives as
/// several `delta.tool_calls[]` fragments keyed by `index` -- the `id` and
/// `function.name` on the first, the `function.arguments` JSON string in
/// pieces across the rest -- so they are accumulated and emitted once,
/// complete, when the model finishes (`finish_reason`, `[DONE]`, or the
/// end of the stream). A caller never sees a partial call.
#[derive(Default)]
pub struct ToolStreamParser {
    calls: BTreeMap<usize, PartialCall>,
}

#[derive(Deserialize)]
struct ToolChunk {
    #[serde(default)]
    choices: Vec<ToolChoice>,
}

#[derive(Deserialize)]
struct ToolChoice {
    #[serde(default)]
    delta: ToolDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ToolDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallFragment>,
}

#[derive(Deserialize)]
struct ToolCallFragment {
    /// Servers that follow the spec always send this; a few
    /// OpenAI-compatible ones omit it, handled in `slot_for`.
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionFragment>,
}

#[derive(Deserialize, Default)]
struct FunctionFragment {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

impl ToolStreamParser {
    /// Which call a fragment belongs to. Normally its `index`; without
    /// one, a fragment carrying an `id` starts a new call and any other
    /// continues the most recent.
    fn slot_for(&self, fragment: &ToolCallFragment) -> usize {
        match fragment.index {
            Some(index) => index,
            None if fragment.id.is_some() => self.calls.keys().next_back().map_or(0, |i| i + 1),
            None => self.calls.keys().next_back().copied().unwrap_or(0),
        }
    }

    /// Emits every accumulated call, in index order, and clears them.
    fn flush(&mut self) -> Result<Vec<ToolCallStreamItem>, ProviderError> {
        std::mem::take(&mut self.calls)
            .into_iter()
            .map(|(index, call)| {
                let name = call.name.filter(|n| !n.is_empty()).ok_or_else(|| {
                    ProviderError::InvalidResponse(format!(
                        "tool call {index} had no function name"
                    ))
                })?;
                let arguments = if call.arguments.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&call.arguments).map_err(|e| {
                        ProviderError::InvalidResponse(format!(
                            "tool call \"{name}\" had malformed arguments ({e}); the reply may \
                             have been cut off"
                        ))
                    })?
                };
                Ok(ToolCallStreamItem::ToolCall {
                    id: call
                        .id
                        .filter(|i| !i.is_empty())
                        .unwrap_or_else(|| format!("call_{index}")),
                    name,
                    arguments,
                })
            })
            .collect()
    }
}

impl LineParser for ToolStreamParser {
    type Item = ToolCallStreamItem;

    fn push_line(&mut self, line: &str) -> Result<Vec<ToolCallStreamItem>, ProviderError> {
        let Some(data) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) else {
            return Ok(Vec::new());
        };
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            return self.flush();
        }

        let chunk: ToolChunk = serde_json::from_str(data)
            .map_err(|e| ProviderError::InvalidResponse(format!("malformed chunk: {e}")))?;
        let mut items = Vec::new();
        // A trailing usage chunk has no choices at all.
        let Some(choice) = chunk.choices.into_iter().next() else {
            return Ok(items);
        };

        if let Some(text) = choice.delta.content.filter(|t| !t.is_empty()) {
            items.push(ToolCallStreamItem::TextDelta(text));
        }
        for fragment in choice.delta.tool_calls {
            let slot = self.slot_for(&fragment);
            let call = self.calls.entry(slot).or_default();
            if let Some(id) = fragment.id.filter(|i| !i.is_empty()) {
                call.id = Some(id);
            }
            if let Some(function) = fragment.function {
                if let Some(name) = function.name.filter(|n| !n.is_empty()) {
                    call.name = Some(name);
                }
                if let Some(arguments) = function.arguments {
                    call.arguments.push_str(&arguments);
                }
            }
        }
        // Any finish reason means the model is done describing its calls.
        if choice.finish_reason.is_some() {
            items.extend(self.flush()?);
        }
        Ok(items)
    }

    fn finish(&mut self) -> Result<Vec<ToolCallStreamItem>, ProviderError> { self.flush() }
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

#[async_trait]
impl ToolCallingProvider for OpenAiCompatible {
    async fn chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ToolCallStream, ProviderError> {
        let request = self
            .client
            .post(chat_completions_url(&self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&build_chat_with_tools_body(&self.model, &messages, &tools));
        http_stream::stream_lines_stateful(
            request,
            ToolStreamParser::default(),
            parse_error_response,
        )
        .await
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

    // -- tool calling: request building (cli tasks 2.1) --

    fn weather_tool() -> ToolDefinition {
        ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get the weather".to_string(),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        }
    }

    fn call_record(id: &str, name: &str, arguments: Value) -> crate::message::ToolCallRecord {
        crate::message::ToolCallRecord { id: id.into(), name: name.into(), arguments }
    }

    #[test]
    fn tools_are_sent_in_openai_function_format() {
        let body =
            build_chat_with_tools_body("gpt-4o-mini", &[Message::user("hi")], &[weather_tool()]);
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(body["tools"][0]["function"]["description"], "Get the weather");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn the_tool_path_bounds_the_reply_with_max_completion_tokens_not_max_tokens() {
        let body = build_chat_with_tools_body("o3-mini", &[Message::user("hi")], &[]);
        assert_eq!(body["max_completion_tokens"], MAX_RESPONSE_TOKENS);
        assert!(body.get("max_tokens").is_none(), "o-series models reject the deprecated field");
    }

    #[test]
    fn plain_chat_keeps_max_tokens_for_openai_compatible_servers() {
        let body = build_chat_body("some-local-model", &[Message::user("hi")]);
        assert_eq!(body["max_tokens"], MAX_RESPONSE_TOKENS);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn a_conversation_with_no_tools_is_identical_to_plain_chat() {
        let messages =
            [Message::system("be brief"), Message::user("hi"), Message::assistant("hey")];
        let with_tools = build_chat_with_tools_body("m", &messages, &[]);
        let plain = build_chat_body("m", &messages);
        assert!(with_tools.get("tools").is_none(), "no tools field when none are offered");
        assert_eq!(with_tools["messages"], plain["messages"]);
    }

    #[test]
    fn an_assistant_turn_with_several_calls_and_their_results_map_to_openai_shapes() {
        let messages = [
            Message::user("weather in Paris and Rome?"),
            Message::assistant_with_tool_calls(
                "",
                vec![
                    call_record("call_a", "get_weather", json!({"city": "Paris"})),
                    call_record("call_b", "get_weather", json!({"city": "Rome"})),
                ],
            ),
            Message::tool_result("call_a", "sunny"),
            Message::tool_result("call_b", "rain"),
        ];
        let body = build_chat_with_tools_body("m", &messages, &[weather_tool()]);
        let sent = &body["messages"];

        assert_eq!(sent[1]["role"], "assistant");
        assert!(sent[1]["content"].is_null(), "no text before the calls means null, not \"\"");
        assert_eq!(sent[1]["tool_calls"][0]["id"], "call_a");
        assert_eq!(sent[1]["tool_calls"][0]["type"], "function");
        assert_eq!(sent[1]["tool_calls"][0]["function"]["name"], "get_weather");
        // The arguments are a JSON *string*, not an object.
        let args = sent[1]["tool_calls"][1]["function"]["arguments"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(args).unwrap(), json!({"city": "Rome"}));

        assert_eq!(sent[2], json!({"role": "tool", "tool_call_id": "call_a", "content": "sunny"}));
        assert_eq!(sent[3], json!({"role": "tool", "tool_call_id": "call_b", "content": "rain"}));
    }

    #[test]
    fn an_assistant_turn_with_text_and_a_call_keeps_the_text() {
        let messages = [Message::assistant_with_tool_calls(
            "Let me check.",
            vec![call_record("call_a", "get_weather", json!({}))],
        )];
        let body = build_chat_with_tools_body("m", &messages, &[]);
        assert_eq!(body["messages"][0]["content"], "Let me check.");
    }

    // -- tool calling: streaming reassembly (cli tasks 2.2) --

    /// Feeds SSE lines to a fresh parser then ends the stream, exactly as
    /// `stream_lines_stateful` does.
    fn run(lines: &[&str]) -> Result<Vec<ToolCallStreamItem>, ProviderError> {
        let mut parser = ToolStreamParser::default();
        let mut items = Vec::new();
        for line in lines {
            items.extend(parser.push_line(line)?);
        }
        items.extend(parser.finish()?);
        Ok(items)
    }

    fn call(id: &str, name: &str, arguments: Value) -> ToolCallStreamItem {
        ToolCallStreamItem::ToolCall { id: id.into(), name: name.into(), arguments }
    }

    #[test]
    fn arguments_split_across_many_fragments_are_reassembled_into_one_call() {
        let items = run(&[
            r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"ci"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ty\": \"Par"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"is\"}"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ])
        .unwrap();
        assert_eq!(items, vec![call("call_abc", "get_weather", json!({"city": "Paris"}))]);
    }

    #[test]
    fn two_calls_in_one_turn_come_out_complete_and_in_order() {
        let items = run(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"get_weather","arguments":"{\"city\":"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"get_time","arguments":"{}"}}]}}]}"#,
            // The first call's remaining arguments arrive after the second began.
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
        .unwrap();
        assert_eq!(
            items,
            vec![
                call("call_a", "get_weather", json!({"city": "Paris"})),
                call("call_b", "get_time", json!({})),
            ]
        );
    }

    #[test]
    fn text_before_a_call_streams_immediately_and_the_call_follows() {
        let items = run(&[
            r#"data: {"choices":[{"delta":{"content":"Let me "}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"check."}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"get_weather","arguments":"{}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
        .unwrap();
        assert_eq!(
            items,
            vec![
                ToolCallStreamItem::TextDelta("Let me ".into()),
                ToolCallStreamItem::TextDelta("check.".into()),
                call("call_a", "get_weather", json!({})),
            ]
        );
    }

    #[test]
    fn a_call_with_no_arguments_becomes_an_empty_object() {
        let items = run(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"get_time","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
        .unwrap();
        assert_eq!(items, vec![call("call_a", "get_time", json!({}))]);
    }

    #[test]
    fn malformed_argument_json_is_a_provider_error_not_a_silent_drop() {
        let err = run(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"get_weather","arguments":"{\"city\": \"Par"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"length"}]}"#,
        ])
        .unwrap_err();
        assert!(
            matches!(&err, ProviderError::InvalidResponse(m) if m.contains("get_weather") && m.contains("malformed")),
            "{err:?}"
        );
    }

    #[test]
    fn a_call_still_buffered_when_the_stream_ends_without_a_marker_is_flushed() {
        // No finish_reason and no [DONE]: `finish()` is the backstop.
        let items = run(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"get_time","arguments":"{}"}}]}}]}"#,
        ])
        .unwrap();
        assert_eq!(items, vec![call("call_a", "get_time", json!({}))]);
    }

    #[test]
    fn a_call_is_emitted_once_even_when_finish_reason_and_done_both_arrive() {
        let items = run(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"get_time","arguments":"{}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ])
        .unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn a_trailing_usage_chunk_with_no_choices_is_ignored() {
        let items = run(&[
            r#"data: {"choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#,
            "data: [DONE]",
        ])
        .unwrap();
        assert_eq!(items, vec![ToolCallStreamItem::TextDelta("hi".into())]);
    }

    #[test]
    fn a_server_that_omits_the_index_still_yields_separate_calls() {
        let items = run(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"call_a","function":{"name":"get_time","arguments":"{}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"call_b","function":{"name":"get_weather","arguments":"{\"city\":\"Rome\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
        .unwrap();
        assert_eq!(
            items,
            vec![
                call("call_a", "get_time", json!({})),
                call("call_b", "get_weather", json!({"city": "Rome"})),
            ]
        );
    }

    #[test]
    fn a_missing_call_id_is_synthesized() {
        let items = run(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"get_time","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        ])
        .unwrap();
        assert_eq!(items, vec![call("call_0", "get_time", json!({}))]);
    }

    #[test]
    fn a_call_with_no_name_is_an_error() {
        let err = run(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        ])
        .unwrap_err();
        assert!(matches!(err, ProviderError::InvalidResponse(m) if m.contains("no function name")));
    }

    #[test]
    fn non_data_lines_and_blank_lines_are_ignored() {
        assert!(run(&["", ": keep-alive", "event: ping"]).unwrap().is_empty());
    }

    // -- tool calling: the real HTTP path against a local server --

    use crate::providers::test_server::{chunked, serve_once};

    async fn collect(mut stream: ToolCallStream) -> Vec<Result<ToolCallStreamItem, ProviderError>> {
        use futures_util::StreamExt;
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item);
        }
        out
    }

    const TOOL_SSE: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Checking. \"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_abc\",\"\
         function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\
         \"{\\\"city\\\": \\\"Paris\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    #[tokio::test]
    async fn chat_with_tools_streams_text_then_a_reassembled_call_over_real_http() {
        // 7-byte chunks split lines, JSON strings and UTF-8 mid-way.
        let (base_url, server) = serve_once(200, chunked(TOOL_SSE, 7));
        let provider = OpenAiCompatible::new("sk-test", format!("{base_url}/v1"), "gpt-4o-mini");

        let stream = provider
            .chat_with_tools(
                vec![
                    Message::user("weather in Paris?"),
                    Message::assistant_with_tool_calls(
                        "",
                        vec![call_record("call_prev", "get_time", json!({}))],
                    ),
                    Message::tool_result("call_prev", "noon"),
                ],
                vec![weather_tool()],
            )
            .await
            .unwrap();
        let items: Vec<_> = collect(stream).await.into_iter().collect::<Result<_, _>>().unwrap();

        assert_eq!(
            items,
            vec![
                ToolCallStreamItem::TextDelta("Checking. ".into()),
                call("call_abc", "get_weather", json!({"city": "Paris"})),
            ]
        );

        // And what actually went over the wire.
        let request = server.join().unwrap();
        assert!(
            request.request_line.starts_with("POST /v1/chat/completions"),
            "{}",
            request.request_line
        );
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
        let sent: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(sent["model"], "gpt-4o-mini");
        assert_eq!(sent["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(sent["messages"][1]["tool_calls"][0]["id"], "call_prev");
        assert_eq!(sent["messages"][2]["tool_call_id"], "call_prev");
    }

    #[tokio::test]
    async fn chat_with_tools_reports_an_http_error_as_a_provider_error() {
        let (base_url, server) =
            serve_once(401, vec![br#"{"error":{"message":"Incorrect API key"}}"#.to_vec()]);
        let provider = OpenAiCompatible::new("bad", format!("{base_url}/v1"), "m");

        let result =
            provider.chat_with_tools(vec![Message::user("hi")], vec![weather_tool()]).await;
        assert!(matches!(result, Err(ProviderError::Auth(m)) if m.contains("Incorrect API key")));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn plain_chat_still_sends_no_tools_and_streams_text() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n";
        let (base_url, server) = serve_once(200, chunked(sse, 5));
        let provider = OpenAiCompatible::new("sk-test", format!("{base_url}/v1"), "m");

        use futures_util::StreamExt;
        let mut stream = provider.chat(vec![Message::user("hi")]).await.unwrap();
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk.unwrap().delta);
        }
        assert_eq!(text, "hello");
        let sent: Value = serde_json::from_str(&server.join().unwrap().body).unwrap();
        assert!(sent.get("tools").is_none(), "plain chat must be exactly what it was before");
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

    /// Manual, credential-gated tool round trip against the real API: the
    /// model must request the tool, and must use its result in the reply.
    /// `OPENAI_API_KEY=sk-... cargo test -p fleet-snowfluff-ai --lib --
    /// --ignored --test-threads=1 openai_live_tool`. Never run in CI, and
    /// **not run while this was written** (no key was available) -- the
    /// wire format is covered by the fixture and local-server tests above.
    #[tokio::test]
    #[ignore = "requires OPENAI_API_KEY and makes real network calls"]
    async fn openai_live_tool_round_trip() {
        let api_key = std::env::var("OPENAI_API_KEY")
            .expect("set OPENAI_API_KEY to run this ignored integration test");
        let provider = OpenAiCompatible::new(api_key, DEFAULT_BASE_URL, "gpt-4o-mini");
        let mut messages = vec![Message::user(
            "What is the weather in Paris right now? You must use the get_weather tool.",
        )];

        let items: Vec<_> = collect(
            provider.chat_with_tools(messages.clone(), vec![weather_tool()]).await.unwrap(),
        )
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
        let (id, name, arguments) = items
            .iter()
            .find_map(|item| match item {
                ToolCallStreamItem::ToolCall { id, name, arguments } => {
                    Some((id.clone(), name.clone(), arguments.clone()))
                }
                _ => None,
            })
            .expect("the model should have requested the tool");
        assert_eq!(name, "get_weather");
        assert!(arguments.get("city").is_some(), "arguments: {arguments}");

        messages
            .push(Message::assistant_with_tool_calls("", vec![call_record(&id, &name, arguments)]));
        messages.push(Message::tool_result(id, "Sunny, 21 degrees Celsius"));
        let reply: String =
            collect(provider.chat_with_tools(messages, vec![weather_tool()]).await.unwrap())
                .await
                .into_iter()
                .filter_map(|item| match item.unwrap() {
                    ToolCallStreamItem::TextDelta(t) => Some(t),
                    _ => None,
                })
                .collect();
        assert!(reply.contains("21") || reply.to_lowercase().contains("sunny"), "reply: {reply}");
    }
}
