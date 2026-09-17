//! The four Stage 1 `AiProvider` implementations.

mod http_stream;
mod line_buffer;

pub mod anthropic;
pub mod mock;
pub mod ollama;
pub mod openai;

pub use anthropic::Anthropic;
pub use mock::Mock;
pub use ollama::Ollama;
pub use openai::OpenAiCompatible;
