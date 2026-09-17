//! The `AiProvider` trait: the one interface every concrete provider
//! (`OpenAiCompatible`, `Anthropic`, `Ollama`, `Mock`) implements, and
//! the only thing the app crate depends on to send a chat message
//! without caring which provider is active.

use std::pin::Pin;

use futures_core::Stream;

use crate::message::{Message, ModelInfo, ProviderError, ProviderKind, StreamChunk};

/// A streamed reply: a `Send` stream of chunks, boxed so it can be
/// returned from a trait object (`Box<dyn AiProvider>`) regardless of
/// each provider's own concrete stream type.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>;

/// Implemented by every concrete provider. `async_trait` is used
/// (rather than a native `async fn` in the trait) specifically so this
/// trait stays object-safe -- the app crate holds whichever provider is
/// currently active behind `Box<dyn AiProvider>`, selected at runtime
/// from `ai-config.json`'s `active_provider`.
#[async_trait::async_trait]
pub trait AiProvider: Send + Sync {
    /// Which provider this is, for logging/UI purposes.
    fn kind(&self) -> ProviderKind;

    /// Sends `messages` (already assembled by `prompt::assemble_messages`)
    /// and returns a stream of incremental chunks
    /// (`ai-provider`'s "Streaming responses").
    async fn chat(&self, messages: Vec<Message>) -> Result<ChatStream, ProviderError>;

    /// Lists models currently available from this provider
    /// (`ai-provider`'s "Live model listing"). Mock has no real models
    /// to list; its implementation returns an empty list rather than
    /// treating this as an error.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
}

#[cfg(test)]
mod tests {
    use futures_util::stream::{once, BoxStream};

    use super::*;

    /// A trivial, non-networked implementation used only to prove the
    /// trait is actually object-safe and usable behind `Box<dyn
    /// AiProvider>` -- the real `Mock` provider (with artificial
    /// streaming delay, per `ai-provider`'s Mock requirement) is built
    /// separately.
    struct TrivialProvider;

    #[async_trait::async_trait]
    impl AiProvider for TrivialProvider {
        fn kind(&self) -> ProviderKind { ProviderKind::Mock }

        async fn chat(&self, _messages: Vec<Message>) -> Result<ChatStream, ProviderError> {
            let chunk = StreamChunk { delta: "ok".to_string() };
            let stream: BoxStream<'static, Result<StreamChunk, ProviderError>> =
                Box::pin(once(async { Ok(chunk) }));
            Ok(stream)
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> { Ok(vec![]) }
    }

    #[tokio::test]
    async fn trait_object_is_usable_behind_a_box_dyn() {
        let provider: Box<dyn AiProvider> = Box::new(TrivialProvider);
        assert_eq!(provider.kind(), ProviderKind::Mock);
        assert!(provider.list_models().await.unwrap().is_empty());
    }
}
