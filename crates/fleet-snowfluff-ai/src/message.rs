//! Wire-agnostic chat message and response types shared by every
//! provider and by the app crate's Tauri commands (hence `Serialize`/
//! `Deserialize` on everything that crosses IPC).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A turn's speaker. `System` carries the assembled persona prompt;
/// each provider's `build_request` decides where that belongs in its
/// own wire format (a leading message for OpenAI-compatible/Ollama, a
/// separate top-level field for Anthropic). `Tool` exists only for
/// `AemeathAgentRuntime`'s internal tool-calling loop (Ollama's
/// `ToolCallingProvider` path) -- it is never persisted to a
/// conversation's session log and never reaches a plain `AiProvider`
/// (`ClaudeCodeCli`/`Codex`/`Anthropic`/`OpenAiCompatible`'s `chat()`),
/// so those providers' own message-mapping code may treat it as
/// unreachable rather than needing a real case for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// One tool call requested by the model, carried on an `Assistant`
/// turn's [`Message::tool_calls`] -- the same shape
/// `tool_provider::ToolCallStreamItem::ToolCall` streams out as, kept
/// here (not re-derived) so the Agent Loop can push a call straight
/// back into the next turn's message list unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Populated only on an `Assistant` turn that called one or more
    /// tools -- empty (and omitted from the wire format entirely, via
    /// `skip_serializing_if`) for every plain-text message, which is
    /// still every message outside `AemeathAgentRuntime`'s own loop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRecord>,
    /// Set only on a `Tool` result turn: the id of the tool call it
    /// answers. OpenAI (`tool_call_id`) and Anthropic (`tool_use_id`)
    /// require this pairing to be explicit; Ollama pairs results with
    /// calls by order and ignores it. Omitted from the wire format
    /// entirely when absent, so every other message serializes exactly
    /// as it did before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// An assistant turn that requested tool calls instead of (or
    /// alongside) replying with text -- `content` is often empty, since
    /// a model that decides to call a tool frequently has nothing else
    /// to say yet.
    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<ToolCallRecord>,
    ) -> Self {
        Self { role: Role::Assistant, content: content.into(), tool_calls, tool_call_id: None }
    }

    /// A tool's result, reported back to the model on its own turn --
    /// Ollama's own wire format for this is `{"role": "tool", "content":
    /// "..."}`, which this maps onto directly.
    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// A tool's result, tied to the call it answers -- what the agent
    /// loop pushes for every call, so providers whose APIs require the
    /// pairing (OpenAI, Anthropic) have the id to send.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// One incremental piece of a streamed reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamChunk {
    pub delta: String,
}

/// A model available for selection, as returned by a provider's live
/// model-listing endpoint (`ai-provider`'s "Live model listing").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
}

/// The four Stage 1 providers (`ai-provider`'s "Provider abstraction").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAi,
    Anthropic,
    Ollama,
    Mock,
}

/// A provider- or transport-level failure, kept coarse enough that the
/// app crate can render a human-readable inline error (`ai-chat`'s
/// "Inline error handling, no automatic retry") without needing to
/// know which provider produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// Could not reach the provider at all (DNS, connection refused,
    /// timeout -- the common case for "Ollama isn't running").
    Network(String),
    /// The provider rejected the credentials.
    Auth(String),
    /// The provider is throttling requests.
    RateLimited(String),
    /// The provider responded, but not in a shape this client understands.
    InvalidResponse(String),
    /// The request was cancelled (explicit stop, or an implicit cancel
    /// from starting a new chat session) before it produced a result.
    Cancelled,
    /// A subscription-auth profile's CLI (`claude`, `codex`) could not
    /// be found on `PATH` or could not be executed -- distinct from
    /// `Auth` because no credential exists to even be wrong yet
    /// (`subscription-first-chat`'s "Distinct provider/runtime failure
    /// states").
    RuntimeUnavailable(String),
    /// The CLI is present, but reports its login/session is no longer
    /// valid (was logged in before; isn't now) -- distinct from `Auth`,
    /// which covers "never had a valid credential in the first place"
    /// (a rejected API key).
    SubscriptionExpired(String),
    /// Authenticated fine, but the current subscription period's usage
    /// allowance is used up. Kept distinct from `RateLimited`: a rate
    /// limit implies "retry shortly will work," quota exhaustion
    /// implies it won't until the period resets, and the two call for
    /// different user-facing guidance.
    QuotaExhausted(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::Auth(msg) => write!(f, "authentication failed: {msg}"),
            Self::RateLimited(msg) => write!(f, "rate limited: {msg}"),
            Self::InvalidResponse(msg) => write!(f, "invalid response: {msg}"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::RuntimeUnavailable(msg) => write!(f, "runtime unavailable: {msg}"),
            Self::SubscriptionExpired(msg) => write!(f, "subscription expired: {msg}"),
            Self::QuotaExhausted(msg) => write!(f, "quota exhausted: {msg}"),
        }
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_role_round_trips_through_json() {
        let msg = Message::user("hi");
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        assert!(json.contains("\"role\":\"user\""));
    }

    #[test]
    fn plain_messages_never_serialize_a_tool_calls_field() {
        // Every message outside `AemeathAgentRuntime`'s own loop is one
        // of these -- the wire format must stay byte-identical to
        // before `tool_calls` existed, since providers that never
        // implement `ToolCallingProvider` (and existing session log
        // files) never expect this field.
        let json = serde_json::to_string(&Message::user("hi")).unwrap();
        assert!(!json.contains("tool_calls"));
    }

    #[test]
    fn messages_without_a_tool_call_id_never_serialize_the_field() {
        // The wire format of every existing message must be untouched.
        for msg in
            [Message::system("s"), Message::user("u"), Message::assistant("a"), Message::tool("t")]
        {
            assert!(!serde_json::to_string(&msg).unwrap().contains("tool_call_id"), "{msg:?}");
        }
    }

    #[test]
    fn a_tool_result_carries_and_round_trips_the_id_of_its_call() {
        let msg = Message::tool_result("call_1", "42");
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_1"));
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"tool_call_id\":\"call_1\""));
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn a_message_missing_tool_call_id_deserializes_with_none() {
        let msg: Message = serde_json::from_str(r#"{"role":"tool","content":"x"}"#).unwrap();
        assert_eq!(msg.tool_call_id, None);
    }

    #[test]
    fn a_message_missing_tool_calls_deserializes_with_an_empty_one() {
        // Backward compatibility for every session log line written
        // before this field existed.
        let msg: Message = serde_json::from_str(r#"{"role":"user","content":"hi"}"#).unwrap();
        assert_eq!(msg.tool_calls, vec![]);
    }

    #[test]
    fn assistant_with_tool_calls_round_trips_through_json() {
        let msg = Message::assistant_with_tool_calls(
            "",
            vec![ToolCallRecord {
                id: "call_0".to_string(),
                name: "get_current_weather".to_string(),
                arguments: serde_json::json!({"location": "Paris"}),
            }],
        );
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        assert_eq!(back.tool_calls[0].name, "get_current_weather");
    }

    #[test]
    fn tool_role_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"tool\"");
        assert_eq!(Message::tool("42 degrees").role, Role::Tool);
    }

    #[test]
    fn provider_kind_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&ProviderKind::OpenAi).unwrap(), "\"open_ai\"");
        assert_eq!(serde_json::to_string(&ProviderKind::Anthropic).unwrap(), "\"anthropic\"");
    }

    #[test]
    fn provider_error_display_is_human_readable() {
        let err = ProviderError::Network("connection refused".into());
        assert_eq!(err.to_string(), "network error: connection refused");
    }

    #[test]
    fn new_subscription_error_variants_display_distinctly() {
        assert_eq!(
            ProviderError::RuntimeUnavailable("claude not on PATH".into()).to_string(),
            "runtime unavailable: claude not on PATH"
        );
        assert_eq!(
            ProviderError::SubscriptionExpired("not logged in".into()).to_string(),
            "subscription expired: not logged in"
        );
        assert_eq!(
            ProviderError::QuotaExhausted("period limit reached".into()).to_string(),
            "quota exhausted: period limit reached"
        );
    }

    #[test]
    fn rate_limited_and_quota_exhausted_produce_visibly_different_messages() {
        let rate_limited = ProviderError::RateLimited("slow down".into()).to_string();
        let quota_exhausted = ProviderError::QuotaExhausted("slow down".into()).to_string();
        assert_ne!(
            rate_limited, quota_exhausted,
            "same underlying message must still read differently so the UI gives different \
             guidance"
        );
        assert!(rate_limited.contains("rate limited"));
        assert!(quota_exhausted.contains("quota exhausted"));
    }
}
