//! Shared glue between `reqwest` and [`ChatStream`], used by every real
//! provider (`OpenAiCompatible`, `Anthropic`, `Ollama`): send the
//! request, turn a non-2xx response into a [`ProviderError`] via the
//! provider's own error-body parser, otherwise line-buffer the byte
//! stream and hand each complete line to the provider's own line
//! parser. Kept out of each provider's own file so the actual HTTP
//! transport code -- the one part of each provider that isn't a pure,
//! unit-testable function -- exists exactly once.

use std::pin::Pin;

use futures_core::Stream;
use futures_util::StreamExt;

use crate::{
    message::{ProviderError, StreamChunk},
    provider::ChatStream,
    providers::line_buffer::LineBuffer,
};

pub async fn stream_lines(
    request: reqwest::RequestBuilder,
    parse_line: fn(&str) -> Result<Option<StreamChunk>, ProviderError>,
    parse_error: fn(u16, &str) -> ProviderError,
) -> Result<ChatStream, ProviderError> {
    let response = request.send().await.map_err(|e| ProviderError::Network(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(parse_error(status, &body));
    }

    let mut byte_stream = response.bytes_stream();
    let stream = async_stream::stream! {
        let mut buffer = LineBuffer::new();
        while let Some(chunk) = byte_stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    yield Err(ProviderError::Network(e.to_string()));
                    return;
                }
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            for line in buffer.push(&text) {
                match parse_line(&line) {
                    Ok(Some(chunk)) => yield Ok(chunk),
                    Ok(None) => {}
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        }
    };
    Ok(Box::pin(stream))
}

/// Like [`stream_lines`], but for a parser that can yield zero, one, or
/// several items from a single line -- needed for tool-calling
/// responses (`Ollama::chat_with_tools`), where one NDJSON line can
/// carry a text delta, one or more tool calls, or nothing at all.
/// `stream_lines` itself is left as its own, simpler function rather
/// than rewritten in terms of this one, since it's shared by every
/// existing plain-chat provider and there's no need to touch
/// well-exercised code for a case (`Option<T>`) this new function
/// already generalizes over.
pub async fn stream_lines_multi<T: Send + 'static>(
    request: reqwest::RequestBuilder,
    parse_line: fn(&str) -> Result<Vec<T>, ProviderError>,
    parse_error: fn(u16, &str) -> ProviderError,
) -> Result<Pin<Box<dyn Stream<Item = Result<T, ProviderError>> + Send>>, ProviderError> {
    let response = request.send().await.map_err(|e| ProviderError::Network(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(parse_error(status, &body));
    }

    let mut byte_stream = response.bytes_stream();
    let stream = async_stream::stream! {
        let mut buffer = LineBuffer::new();
        while let Some(chunk) = byte_stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    yield Err(ProviderError::Network(e.to_string()));
                    return;
                }
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            for line in buffer.push(&text) {
                match parse_line(&line) {
                    Ok(items) => {
                        for item in items {
                            yield Ok(item);
                        }
                    }
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        }
    };
    Ok(Box::pin(stream))
}

/// Shared by every real provider's `list_models`: GET a URL, and
/// either hand the body to the provider's own list-parser or turn a
/// non-2xx response into a [`ProviderError`].
pub async fn fetch_and_parse<T>(
    request: reqwest::RequestBuilder,
    parse_body: fn(&str) -> Result<T, ProviderError>,
    parse_error: fn(u16, &str) -> ProviderError,
) -> Result<T, ProviderError> {
    let response = request.send().await.map_err(|e| ProviderError::Network(e.to_string()))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(parse_error(status.as_u16(), &body));
    }
    parse_body(&body)
}
