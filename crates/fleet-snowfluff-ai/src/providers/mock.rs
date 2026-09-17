//! `Mock`: an offline, no-network provider that echoes the input back
//! in artificially-delayed chunks, so it exercises the real streaming
//! path end to end. Unlike the other three providers, this one is not
//! test-only scaffolding -- it's a real, user-selectable option in the
//! settings UI ("offline demo" mode), per `ai-provider`'s Mock
//! requirement.

use std::time::Duration;

use async_trait::async_trait;

use crate::{
    message::{Message, ModelInfo, ProviderError, ProviderKind, Role, StreamChunk},
    provider::{AiProvider, ChatStream},
};

/// Delay between chunks -- small enough that a chat feels responsive,
/// large enough to visibly demonstrate streaming rather than just
/// flashing the whole reply in at once.
const CHUNK_DELAY: Duration = Duration::from_millis(60);
const CHUNK_SIZE_CHARS: usize = 4;

pub struct Mock;

/// Splits `text` into small chunks for the artificial streaming delay
/// -- pure, testable without an async runtime.
pub fn chunk_text(text: &str, chunk_size: usize) -> Vec<String> {
    let chunk_size = chunk_size.max(1);
    text.chars().collect::<Vec<_>>().chunks(chunk_size).map(|c| c.iter().collect()).collect()
}

fn last_user_message(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

#[async_trait]
impl AiProvider for Mock {
    fn kind(&self) -> ProviderKind { ProviderKind::Mock }

    async fn chat(&self, messages: Vec<Message>) -> Result<ChatStream, ProviderError> {
        let echoed = format!("🔁 {}", last_user_message(&messages));
        let chunks = chunk_text(&echoed, CHUNK_SIZE_CHARS);

        let stream = async_stream::stream! {
            for chunk in chunks {
                tokio::time::sleep(CHUNK_DELAY).await;
                yield Ok(StreamChunk { delta: chunk });
            }
        };
        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        // Nothing to select -- Mock has no real models, and that's not
        // an error condition (`ai-provider`'s "Live model listing"
        // note on Mock).
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::*;

    #[test]
    fn chunk_text_splits_into_multiple_pieces() {
        let chunks = chunk_text("hello world", 4);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), "hello world");
    }

    #[test]
    fn chunk_text_handles_multibyte_characters_safely() {
        let chunks = chunk_text("你好世界", 2);
        assert_eq!(chunks.concat(), "你好世界");
    }

    #[tokio::test]
    async fn chat_echoes_the_last_user_message_across_multiple_chunks() {
        let mock = Mock;
        let messages = vec![Message::system("ignored"), Message::user("hello there")];
        let mut stream = mock.chat(messages).await.unwrap();

        let mut collected = String::new();
        let mut chunk_count = 0;
        while let Some(chunk) = stream.next().await {
            collected.push_str(&chunk.unwrap().delta);
            chunk_count += 1;
        }

        assert!(collected.contains("hello there"), "must echo the user's message");
        assert!(chunk_count > 1, "must actually stream, not return everything in one chunk");
    }

    #[tokio::test]
    async fn list_models_returns_an_empty_list_not_an_error() {
        let models = Mock.list_models().await.unwrap();
        assert!(models.is_empty());
    }
}
