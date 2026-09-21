//! AI provider abstraction, persona loading, and prompt assembly.
//!
//! This crate depends on nothing project-specific (`fleet-snowfluff-core`
//! or the app crate) so it stays independently testable and so
//! `fleet-snowfluff-core` never has to know AI features exist. The app
//! crate is the only place that integrates this crate with `core`.

pub mod credentials;
pub mod limits;
pub mod log;
pub mod message;
pub mod persona;
pub mod prompt;
pub mod provider;
pub mod providers;
pub mod settings;
pub mod task_router;

pub use credentials::ProviderCredentials;
pub use log::{LogEntry, LogRole};
pub use message::{Message, ModelInfo, ProviderError, ProviderKind, Role, StreamChunk};
pub use persona::{FewShotExample, Language, Persona, PersonaParseError, ResponseLanguage};
pub use provider::{AiProvider, ChatStream};
pub use providers::{Anthropic, ClaudeCodeCli, Codex, Mock, Ollama, OpenAiCompatible};
pub use settings::{AiSettings, AuthMethod, ProfileKey, ProviderProfile, TaskRouterMode};
pub use task_router::{
    detect_escalation, with_task_router_rules, DefaultTaskRouter, EscalationDecision,
    ExecutionRoute, RoutingContext, SessionStrategy, Task, TaskRequirements, TaskRouter,
    BUNDLED_DEFAULT_TASK_ROUTER_RULES, ESCALATE_MARKER,
};
