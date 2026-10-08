//! Stage 1 cost/latency guardrails (`ai-provider`'s "Bounded response
//! length" and "Bounded conversation context"), fixed constants rather
//! than user-configurable settings by deliberate design decision.

/// How much recent conversation history one request may carry, measured in
/// estimated tokens (`token_estimate`), not in a count of messages: many short
/// chat messages fit, one pasted page does not crowd out the rest. The budget
/// actually used is the smaller of this and [`HISTORY_WINDOW_PERCENT`] of the
/// active model's context window.
pub const HISTORY_TOKEN_BUDGET: usize = 3_000;

/// Share of a model's context window that history may use, so history cannot
/// crowd out the persona, the tool definitions, and the reply on a small
/// window.
pub const HISTORY_WINDOW_PERCENT: usize = 40;

/// Older messages that fell outside the budget but are not yet covered by a
/// conversation summary keep being sent, up to this many extra tokens, so
/// nothing is missing while a summary is pending.
pub const HISTORY_UNCOVERED_EXTRA_TOKENS: usize = 1_500;

/// Upper bound on the number of history messages, whatever their size: a
/// backstop against a flood of tiny messages.
pub const HISTORY_MAX_MESSAGES: usize = 60;

/// Maximum output tokens requested per reply, across every provider.
pub const MAX_RESPONSE_TOKENS: u32 = 4096;

// There is deliberately no `OLLAMA_NUM_CTX` constant here. An earlier
// version of this file added one (first 8192, then 16384), reasoning
// that Ollama's own default context window was a small fixed value
// (incorrectly assumed to be "historically 2048"). A real report
// confirmed the opposite: Ollama's *actual* own default auto-scales
// with the host's available VRAM (per Ollama's own docs -- 4k under
// 24 GiB, 32k from 24-48 GiB, 256k at 48 GiB+), and `ollama ps` on the
// machine that hit this showed **32768** already, well above either
// fixed value this file tried to impose. Setting `num_ctx` explicitly
// didn't fix a too-small window -- it *replaced* Ollama's own sensible,
// hardware-aware default with a smaller fixed one, making the actual
// problem worse. Leaving `num_ctx` unset lets Ollama's own
// auto-detection do its job; second-guessing it with a hardcoded
// number here was the mistake, not the fix.
