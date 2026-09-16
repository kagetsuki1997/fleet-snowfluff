//! AI provider abstraction, persona loading, and prompt assembly.
//!
//! This crate depends on nothing project-specific (`fleet-snowfluff-core`
//! or the app crate) so it stays independently testable and so
//! `fleet-snowfluff-core` never has to know AI features exist. The app
//! crate is the only place that integrates this crate with `core`.

pub mod limits;
pub mod message;
pub mod persona;
pub mod prompt;
pub mod provider;

pub use message::{Message, ModelInfo, ProviderError, ProviderKind, Role, StreamChunk};
pub use persona::{FewShotExample, Language, Persona, PersonaParseError, ResponseLanguage};
pub use provider::{AiProvider, ChatStream};
