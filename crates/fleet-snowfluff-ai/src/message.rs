//! Wire-agnostic chat message and response types shared by every
//! provider and by the app crate's Tauri commands (hence `Serialize`/
//! `Deserialize` on everything that crosses IPC).

use serde::{Deserialize, Serialize};

/// A turn's speaker. `System` carries the assembled persona prompt;
/// each provider's `build_request` decides where that belongs in its
/// own wire format (a leading message for OpenAI-compatible/Ollama, a
/// separate top-level field for Anthropic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: Role::System, content: content.into() }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into() }
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
