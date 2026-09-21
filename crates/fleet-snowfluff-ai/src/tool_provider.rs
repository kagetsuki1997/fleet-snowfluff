//! `ToolCallingProvider`: an additive, opt-in extension of `AiProvider`
//! for providers that can call tools mid-conversation
//! (`agent-core-and-task-router`'s Group 3). A separate trait rather
//! than a change to `AiProvider::chat()` itself -- see design.md's
//! "`ToolCallingProvider` is a separate trait from `AiProvider`" for
//! why: extending `ChatStream`'s item type would touch all six
//! existing providers, including `ClaudeCodeCli`/`Codex`, which must
//! never receive Fleet's own tool definitions (they own their native
//! tool loops entirely).
//!
//! **v1 implements this for `Ollama` only** -- `OpenAiCompatible`'s
//! fragmented `delta.tool_calls[].function.arguments` streaming and
//! `Anthropic`'s `tool_use`/`input_json_delta` content blocks are each
//! independent, real parsing work, deliberately deferred to a fast-
//! follow change.

use std::pin::Pin;

use futures_core::Stream;
use serde_json::Value;

use crate::{
    message::{Message, ProviderError},
    provider::AiProvider,
};

/// One tool a `ToolCallingProvider` may offer the model, in the shape
/// every provider's own wire format can be built from (Ollama's
/// `{"type": "function", "function": {name, description, parameters}}`
/// mirrors OpenAI's own tool schema almost exactly, so this shape is
/// deliberately provider-agnostic rather than Ollama-specific, even
/// though only Ollama consumes it today).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// A JSON Schema object describing the tool's arguments.
    pub parameters: Value,
}

/// One item from a [`ToolCallStream`]: either a piece of visible reply
/// text, or a request to run a tool. A single streamed turn may yield
/// any mix of both (a model can narrate before calling a tool), so
/// these are not mutually exclusive phases of the stream.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallStreamItem {
    TextDelta(String),
    ToolCall { id: String, name: String, arguments: Value },
}

/// A streamed reply that may contain tool calls -- boxed for the same
/// reason [`crate::provider::ChatStream`] is: returned from a trait
/// object (`Box<dyn ToolCallingProvider>`) regardless of each
/// provider's own concrete stream type.
pub type ToolCallStream =
    Pin<Box<dyn Stream<Item = Result<ToolCallStreamItem, ProviderError>> + Send>>;

/// Implemented by providers that can call tools mid-conversation.
/// `: AiProvider` so a tool-capable provider is still usable everywhere
/// a plain `AiProvider` is expected (e.g. `list_models`/
/// `check_availability`) -- `chat_with_tools` is additive, not a
/// replacement for `chat()`.
#[async_trait::async_trait]
pub trait ToolCallingProvider: AiProvider {
    /// Sends `messages` with `tools` available for the model to call,
    /// and returns a stream that may interleave
    /// [`ToolCallStreamItem::TextDelta`]
    /// and [`ToolCallStreamItem::ToolCall`] items.
    async fn chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ToolCallStream, ProviderError>;
}

#[cfg(test)]
mod tests {
    use futures_util::stream::{once, BoxStream};

    use super::*;
    use crate::{message::ModelInfo, provider::ChatStream, ProviderKind};

    /// A trivial, non-networked implementation used only to prove the
    /// trait is actually object-safe and usable behind `Box<dyn
    /// ToolCallingProvider>` -- mirrors `AiProvider`'s own
    /// `TrivialProvider` object-safety test.
    struct TrivialToolProvider;

    #[async_trait::async_trait]
    impl AiProvider for TrivialToolProvider {
        fn kind(&self) -> ProviderKind { ProviderKind::Mock }

        async fn chat(&self, _messages: Vec<Message>) -> Result<ChatStream, ProviderError> {
            let chunk = crate::message::StreamChunk { delta: "ok".to_string() };
            let stream: BoxStream<'static, Result<crate::message::StreamChunk, ProviderError>> =
                Box::pin(once(async { Ok(chunk) }));
            Ok(stream)
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> { Ok(vec![]) }
    }

    #[async_trait::async_trait]
    impl ToolCallingProvider for TrivialToolProvider {
        async fn chat_with_tools(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<ToolCallStream, ProviderError> {
            let item = ToolCallStreamItem::TextDelta("ok".to_string());
            let stream: BoxStream<'static, Result<ToolCallStreamItem, ProviderError>> =
                Box::pin(once(async { Ok(item) }));
            Ok(stream)
        }
    }

    #[tokio::test]
    async fn trait_object_is_usable_behind_a_box_dyn() {
        let provider: Box<dyn ToolCallingProvider> = Box::new(TrivialToolProvider);
        assert_eq!(provider.kind(), ProviderKind::Mock);
        let mut stream = provider.chat_with_tools(vec![], vec![]).await.unwrap();
        use futures_util::StreamExt;
        let item = stream.next().await.unwrap().unwrap();
        assert_eq!(item, ToolCallStreamItem::TextDelta("ok".to_string()));
    }
}
