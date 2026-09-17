//! Stage 1 cost/latency guardrails (`ai-provider`'s "Bounded response
//! length" and "Bounded conversation context"), fixed constants rather
//! than user-configurable settings by deliberate design decision.

/// Maximum number of most-recent conversation turns sent as context on
/// each request -- not the full session history.
pub const CONTEXT_WINDOW_TURNS: usize = 20;

/// Maximum output tokens requested per reply, across every provider.
pub const MAX_RESPONSE_TOKENS: u32 = 300;
