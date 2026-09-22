//! The four Stage 1 `AiProvider` implementations, plus
//! `subscription-first-chat`'s subscription-auth support.

mod anthropic_stream_event;
mod cli_locator;
mod cli_process;
mod http_stream;
mod line_buffer;

pub mod anthropic;
pub mod claude_code_cli;
pub mod codex;
pub mod mock;
pub mod ollama;
pub mod openai;

pub use anthropic::Anthropic;
pub use claude_code_cli::ClaudeCodeCli;
pub use codex::Codex;
pub use mock::Mock;
pub use ollama::Ollama;
pub use openai::OpenAiCompatible;
