//! `Anthropic`: Claude's native Messages API. A distinct implementation
//! from `OpenAiCompatible` on purpose -- different auth header
//! (`x-api-key` + `anthropic-version`, not `Authorization: Bearer`),
//! system prompt as a top-level field rather than a `system`-role
//! message, and a differently-shaped SSE event stream (typed
//! `content_block_delta` events, not a flat `choices[0].delta`).

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    limits::MAX_RESPONSE_TOKENS,
    message::{Message, ModelInfo, ProviderError, ProviderKind, Role, StreamChunk},
    provider::{AiProvider, ChatStream},
    providers::{
        anthropic_stream_event,
        http_stream::{self, LineParser},
    },
    tool_provider::{ToolCallStream, ToolCallStreamItem, ToolCallingProvider, ToolDefinition},
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
                Role::Tool => unreachable!(
                    "Role::Tool only appears inside AemeathAgentRuntime's own loop for Ollama's \
                     ToolCallingProvider path -- Anthropic's plain chat() (this function) never \
                     receives it, and it is never persisted to a conversation's session log"
                ),
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

/// The conversation as Anthropic wants it for a request that carries
/// tools. Differs from plain chat in two ways that matter:
///
/// - an assistant turn that called tools is a list of content blocks (`text`,
///   then one `tool_use` per call), not a string;
/// - **every** result for that turn must arrive together in the very next user
///   message, as `tool_result` blocks -- so a run of consecutive `Role::Tool`
///   messages is merged into one user message rather than emitted as separate
///   turns (Anthropic rejects the latter).
fn to_anthropic_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut pending_results: Vec<Value> = Vec::new();

    fn flush(out: &mut Vec<Value>, pending: &mut Vec<Value>) {
        if !pending.is_empty() {
            out.push(json!({ "role": "user", "content": std::mem::take(pending) }));
        }
    }

    for message in messages.iter().filter(|m| m.role != Role::System) {
        match message.role {
            Role::Tool => pending_results.push(json!({
                "type": "tool_result",
                "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                "content": message.content,
            })),
            Role::Assistant if !message.tool_calls.is_empty() => {
                flush(&mut out, &mut pending_results);
                let mut blocks: Vec<Value> = Vec::new();
                // An empty text block is rejected, so only include real text.
                if !message.content.is_empty() {
                    blocks.push(json!({ "type": "text", "text": message.content }));
                }
                blocks.extend(message.tool_calls.iter().map(|call| {
                    json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    })
                }));
                out.push(json!({ "role": "assistant", "content": blocks }));
            }
            Role::User | Role::Assistant => {
                flush(&mut out, &mut pending_results);
                let role = if message.role == Role::User { "user" } else { "assistant" };
                out.push(json!({ "role": role, "content": message.content }));
            }
            Role::System => unreachable!("filtered out above"),
        }
    }
    flush(&mut out, &mut pending_results);
    out
}

/// The Messages-API request body for a request that offers `tools` --
/// the tool-aware counterpart of [`build_chat_body`], which (with its
/// `Role::Tool` guard) is left untouched so plain chat is unaffected.
/// The system prompt stays a top-level field; with no tools the `tools`
/// field is omitted.
pub fn build_chat_with_tools_body(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Value {
    let system_prompt = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut body = json!({
        "model": model,
        "max_tokens": MAX_RESPONSE_TOKENS,
        "messages": to_anthropic_messages(messages),
        "stream": true,
    });
    if !system_prompt.is_empty() {
        body["system"] = Value::String(system_prompt);
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters,
                    })
                })
                .collect(),
        );
    }
    body
}

/// One `tool_use` content block being assembled from `input_json_delta`
/// events.
struct ToolBlock {
    id: String,
    name: String,
    input_json: String,
}

/// Turns Anthropic's streamed events into [`ToolCallStreamItem`]s.
///
/// Text deltas pass straight through (via the same extraction
/// `ClaudeCodeCli` uses, which is left unchanged). A tool call arrives as
/// a `content_block_start` (`tool_use`: `id`, `name`), then the `input`
/// JSON in `input_json_delta` pieces, then `content_block_stop` -- it is
/// emitted, complete, at the stop. A stream that ends with a block still
/// open was cut off, and that is an error rather than a partial call.
#[derive(Default)]
pub struct ToolStreamParser {
    blocks: BTreeMap<usize, ToolBlock>,
}

impl LineParser for ToolStreamParser {
    type Item = ToolCallStreamItem;

    fn push_line(&mut self, line: &str) -> Result<Vec<ToolCallStreamItem>, ProviderError> {
        // `event: ...` lines carry nothing the `data:` payload's own
        // `type` does not.
        let Some(data) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) else {
            return Ok(Vec::new());
        };
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let event: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::InvalidResponse(format!("malformed event: {e}")))?;

        let mut items = Vec::new();
        // Visible text (and an `error` event, surfaced as a real error).
        if let Some(chunk) = anthropic_stream_event::extract_chunk(&event)? {
            items.push(ToolCallStreamItem::TextDelta(chunk.delta));
        }

        let index = event.get("index").and_then(Value::as_u64).map(|i| i as usize);
        match (event.get("type").and_then(Value::as_str), index) {
            (Some("content_block_start"), Some(index)) => {
                let block = event.get("content_block");
                if block.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("tool_use") {
                    let field = |name: &str| {
                        block
                            .and_then(|b| b.get(name))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    };
                    self.blocks.insert(
                        index,
                        ToolBlock {
                            id: field("id"),
                            name: field("name"),
                            input_json: String::new(),
                        },
                    );
                }
            }
            (Some("content_block_delta"), Some(index)) => {
                let delta = event.get("delta");
                if delta.and_then(|d| d.get("type")).and_then(Value::as_str)
                    == Some("input_json_delta")
                {
                    if let (Some(block), Some(piece)) = (
                        self.blocks.get_mut(&index),
                        delta.and_then(|d| d.get("partial_json")).and_then(Value::as_str),
                    ) {
                        block.input_json.push_str(piece);
                    }
                }
            }
            (Some("content_block_stop"), Some(index)) => {
                if let Some(block) = self.blocks.remove(&index) {
                    let input = if block.input_json.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&block.input_json).map_err(|e| {
                            ProviderError::InvalidResponse(format!(
                                "tool call \"{}\" had malformed input ({e})",
                                block.name
                            ))
                        })?
                    };
                    items.push(ToolCallStreamItem::ToolCall {
                        id: block.id,
                        name: block.name,
                        arguments: input,
                    });
                }
            }
            _ => {}
        }
        Ok(items)
    }

    fn finish(&mut self) -> Result<Vec<ToolCallStreamItem>, ProviderError> {
        match self.blocks.values().next() {
            Some(block) => Err(ProviderError::InvalidResponse(format!(
                "tool call \"{}\" was cut off before it completed",
                block.name
            ))),
            None => Ok(Vec::new()),
        }
    }
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

#[async_trait]
impl ToolCallingProvider for Anthropic {
    async fn chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ToolCallStream, ProviderError> {
        let request = self
            .client
            .post(messages_url(&self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
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
    use crate::{
        message::ToolCallRecord,
        providers::test_server::{chunked, serve_once},
    };

    // -- tool calling: request building (cli tasks 3.1) --

    fn weather_tool() -> ToolDefinition {
        ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get the weather".to_string(),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        }
    }

    fn record(id: &str, name: &str, arguments: Value) -> ToolCallRecord {
        ToolCallRecord { id: id.into(), name: name.into(), arguments }
    }

    #[test]
    fn tools_are_sent_with_an_input_schema() {
        let body =
            build_chat_with_tools_body("claude-x", &[Message::user("hi")], &[weather_tool()]);
        assert_eq!(body["tools"][0]["name"], "get_weather");
        assert_eq!(body["tools"][0]["description"], "Get the weather");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn the_system_prompt_stays_top_level_and_never_enters_messages() {
        let body = build_chat_with_tools_body(
            "m",
            &[Message::system("be brief"), Message::user("hi")],
            &[weather_tool()],
        );
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn a_conversation_with_no_tools_matches_plain_chat_and_omits_the_tools_field() {
        let messages =
            [Message::system("be brief"), Message::user("hi"), Message::assistant("hey")];
        let with_tools = build_chat_with_tools_body("m", &messages, &[]);
        let plain = build_chat_body("m", &messages);
        assert!(with_tools.get("tools").is_none());
        assert_eq!(with_tools["messages"], plain["messages"]);
        assert_eq!(with_tools["system"], plain["system"]);
    }

    #[test]
    fn a_single_call_and_its_result_map_to_tool_use_and_tool_result_blocks() {
        let messages = [
            Message::user("weather in Paris?"),
            Message::assistant_with_tool_calls(
                "",
                vec![record("toolu_1", "get_weather", json!({"city": "Paris"}))],
            ),
            Message::tool_result("toolu_1", "sunny"),
        ];
        let body = build_chat_with_tools_body("m", &messages, &[weather_tool()]);
        let sent = &body["messages"];

        assert_eq!(sent[1]["role"], "assistant");
        assert_eq!(
            sent[1]["content"],
            json!([{"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}]),
            "no empty text block before the call"
        );
        assert_eq!(sent[2]["role"], "user");
        assert_eq!(
            sent[2]["content"],
            json!([{"type": "tool_result", "tool_use_id": "toolu_1", "content": "sunny"}])
        );
    }

    #[test]
    fn several_results_from_one_turn_merge_into_a_single_user_message() {
        let messages = [
            Message::user("weather in Paris and Rome?"),
            Message::assistant_with_tool_calls(
                "",
                vec![
                    record("toolu_a", "get_weather", json!({"city": "Paris"})),
                    record("toolu_b", "get_weather", json!({"city": "Rome"})),
                ],
            ),
            Message::tool_result("toolu_a", "sunny"),
            Message::tool_result("toolu_b", "rain"),
        ];
        let body = build_chat_with_tools_body("m", &messages, &[weather_tool()]);
        let sent = body["messages"].as_array().unwrap();

        // user, assistant, and exactly ONE user turn holding both results.
        assert_eq!(sent.len(), 3, "results must not become separate user turns: {sent:?}");
        let results = sent[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["tool_use_id"], "toolu_a");
        assert_eq!(results[1]["tool_use_id"], "toolu_b");
    }

    #[test]
    fn text_before_a_call_is_kept_as_a_text_block_ahead_of_the_tool_use() {
        let messages = [Message::assistant_with_tool_calls(
            "Let me check.",
            vec![record("toolu_1", "get_weather", json!({}))],
        )];
        let body = build_chat_with_tools_body("m", &messages, &[]);
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[0], json!({"type": "text", "text": "Let me check."}));
        assert_eq!(blocks[1]["type"], "tool_use");
    }

    #[test]
    fn a_second_round_of_calls_keeps_each_results_message_after_its_own_turn() {
        let messages = [
            Message::user("go"),
            Message::assistant_with_tool_calls("", vec![record("t1", "a", json!({}))]),
            Message::tool_result("t1", "one"),
            Message::assistant_with_tool_calls("", vec![record("t2", "b", json!({}))]),
            Message::tool_result("t2", "two"),
        ];
        let body = build_chat_with_tools_body("m", &messages, &[]);
        let roles: Vec<&str> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant", "user"]);
    }

    // -- tool calling: streaming reassembly (cli tasks 3.2) --

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

    const START: &str = r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01A","name":"get_weather","input":{}}}"#;
    const STOP_1: &str = r#"data: {"type":"content_block_stop","index":1}"#;

    #[test]
    fn input_json_split_across_deltas_is_reassembled_into_one_call() {
        let items = run(&[
            "event: content_block_start",
            START,
            "event: content_block_delta",
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":""}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"ci"}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"ty\": \"Par"}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"is\"}"}}"#,
            STOP_1,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
            r#"data: {"type":"message_stop"}"#,
        ])
        .unwrap();
        assert_eq!(items, vec![call("toolu_01A", "get_weather", json!({"city": "Paris"}))]);
    }

    #[test]
    fn text_then_a_tool_call_streams_the_text_first() {
        let items = run(&[
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me "}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"check."}}"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            START,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
            STOP_1,
        ])
        .unwrap();
        assert_eq!(
            items,
            vec![
                ToolCallStreamItem::TextDelta("Let me ".into()),
                ToolCallStreamItem::TextDelta("check.".into()),
                call("toolu_01A", "get_weather", json!({})),
            ]
        );
    }

    #[test]
    fn two_tool_blocks_in_one_turn_are_emitted_in_stream_order() {
        let items = run(&[
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_a","name":"get_weather","input":{}}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Paris\"}"}}"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_b","name":"get_time","input":{}}}"#,
            r#"data: {"type":"content_block_stop","index":1}"#,
        ])
        .unwrap();
        assert_eq!(
            items,
            vec![
                call("toolu_a", "get_weather", json!({"city": "Paris"})),
                call("toolu_b", "get_time", json!({})),
            ]
        );
    }

    #[test]
    fn a_tool_call_with_no_input_deltas_becomes_an_empty_object() {
        assert_eq!(
            run(&[START, STOP_1]).unwrap(),
            vec![call("toolu_01A", "get_weather", json!({}))]
        );
    }

    #[test]
    fn an_error_event_mid_stream_is_surfaced_as_an_error() {
        let err = run(&[
            START,
            r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ])
        .unwrap_err();
        assert!(matches!(err, ProviderError::InvalidResponse(m) if m == "Overloaded"));
    }

    #[test]
    fn a_block_still_open_when_the_stream_ends_is_an_error_not_a_partial_call() {
        let err = run(&[
            START,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\": \"Pa"}}"#,
        ])
        .unwrap_err();
        assert!(
            matches!(&err, ProviderError::InvalidResponse(m) if m.contains("cut off")),
            "{err:?}"
        );
    }

    #[test]
    fn malformed_input_json_is_a_provider_error() {
        let err = run(&[
            START,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{not json"}}"#,
            STOP_1,
        ])
        .unwrap_err();
        assert!(
            matches!(&err, ProviderError::InvalidResponse(m) if m.contains("malformed")),
            "{err:?}"
        );
    }

    #[test]
    fn pings_message_events_and_event_lines_are_ignored() {
        let items = run(&[
            "event: ping",
            r#"data: {"type":"ping"}"#,
            r#"data: {"type":"message_start","message":{"id":"msg_1"}}"#,
            "",
        ])
        .unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn plain_text_extraction_shared_with_claude_code_is_unchanged() {
        // The shared extractor must still ignore tool-input deltas.
        let delta = json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}});
        assert!(anthropic_stream_event::extract_chunk(&delta).unwrap().is_none());
    }

    // -- tool calling: the real HTTP path against a local server (cli task 3.3) --

    const TOOL_SSE: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"\
         text\":\"Checking. \"}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"\
         tool_use\",\"id\":\"toolu_01A\",\"name\":\"get_weather\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"\
         input_json_delta\",\"partial_json\":\"{\\\"city\\\": \\\"Paris\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    async fn collect(mut stream: ToolCallStream) -> Vec<Result<ToolCallStreamItem, ProviderError>> {
        use futures_util::StreamExt;
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item);
        }
        out
    }

    #[tokio::test]
    async fn chat_with_tools_streams_text_then_a_reassembled_call_over_real_http() {
        let (base_url, server) = serve_once(200, chunked(TOOL_SSE, 7));
        let provider = Anthropic::new("sk-ant-test", format!("{base_url}/v1"), "claude-x");

        let stream = provider
            .chat_with_tools(
                vec![
                    Message::system("be brief"),
                    Message::user("weather in Paris?"),
                    Message::assistant_with_tool_calls(
                        "",
                        vec![record("toolu_prev", "get_time", json!({}))],
                    ),
                    Message::tool_result("toolu_prev", "noon"),
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
                call("toolu_01A", "get_weather", json!({"city": "Paris"})),
            ]
        );

        let request = server.join().unwrap();
        assert!(request.request_line.starts_with("POST /v1/messages"), "{}", request.request_line);
        assert_eq!(request.header("x-api-key"), Some("sk-ant-test"));
        assert_eq!(request.header("anthropic-version"), Some(ANTHROPIC_VERSION));
        let sent: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(sent["system"], "be brief");
        assert_eq!(sent["tools"][0]["input_schema"]["type"], "object");
        // The system message is not in `messages`: user, assistant(tool_use),
        // user(tool_result).
        assert_eq!(sent["messages"].as_array().unwrap().len(), 3);
        assert_eq!(sent["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(sent["messages"][2]["content"][0]["tool_use_id"], "toolu_prev");
    }

    #[tokio::test]
    async fn chat_with_tools_reports_an_http_error_as_a_provider_error() {
        let (base_url, server) = serve_once(
            401,
            vec![br#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#.to_vec()],
        );
        let provider = Anthropic::new("bad", format!("{base_url}/v1"), "m");
        let result =
            provider.chat_with_tools(vec![Message::user("hi")], vec![weather_tool()]).await;
        assert!(matches!(result, Err(ProviderError::Auth(m)) if m.contains("invalid x-api-key")));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn plain_chat_still_sends_no_tools_and_streams_text() {
        let sse = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"\
                   text_delta\",\"text\":\"hello\"}}\n\n";
        let (base_url, server) = serve_once(200, chunked(sse, 5));
        let provider = Anthropic::new("k", format!("{base_url}/v1"), "m");

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

    /// Manual, credential-gated tool round trip against the real API.
    /// `ANTHROPIC_API_KEY=sk-ant-... cargo test -p fleet-snowfluff-ai
    /// --lib -- --ignored --test-threads=1 anthropic_live_tool`. Never run
    /// in CI, and **not run while this was written** (no key was
    /// available) -- the wire format is covered by the fixture and
    /// local-server tests above.
    #[tokio::test]
    #[ignore = "requires ANTHROPIC_API_KEY and makes real network calls"]
    async fn anthropic_live_tool_round_trip() {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .expect("set ANTHROPIC_API_KEY to run this ignored integration test");
        let provider = Anthropic::new(api_key, DEFAULT_BASE_URL, "claude-haiku-4-5-20251001");
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

        messages.push(Message::assistant_with_tool_calls("", vec![record(&id, &name, arguments)]));
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
