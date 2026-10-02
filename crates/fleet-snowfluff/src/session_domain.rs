//! Three concepts a chat turn touches that must not be conflated
//! (added per a `docs/fleet-snowfluff-feature-planning.md` update made
//! mid-implementation of `subscription-first-chat`, and grilled against
//! this change's own already-built code before writing any of this --
//! see that change's design.md for the full resolution):
//!
//! - **`ConversationId`**: the user-visible, long-lived chat container --
//!   already exists in substance as `chat_log_store`'s session file, identified
//!   by its `PathBuf`; this just gives that identity a name and a type instead
//!   of leaving it an implicit path. **Now defined in `fleet-snowfluff-ai`**
//!   (`conversation.rs`, re-exported below) -- `agent-core-and-task-router`'s
//!   `ToolContext` needs it and must not depend on this app crate, so the type
//!   moved to where both sides can share it, and this module keeps a `pub use`
//!   so every existing call site here is unaffected.
//! - **`ExecutionId`**: one turn's execution. **Now also defined in
//!   `fleet-snowfluff-ai`** (`execution.rs`, re-exported below) --
//!   `execution-log-and-context` needs `Execution`/`ExecutionEvent` records
//!   constructible from `ContextManager`, which must not depend on this app
//!   crate, so the id moved for the same reason `ConversationId` already did.
//!   Originally typing-only and never persisted; now persisted as part of every
//!   `ExecutionStart`/`ExecutionEnd` event.
//! - **`ExternalSessionRef`**: a CLI-backed subscription provider's own
//!   session/thread id (Claude's `session_id`, Codex's `thread_id`) -- already
//!   exists in substance as the bare `String` values in
//!   `ChatRuntimeState.cli_sessions`; this gives that a name too.
//!
//! `ExternalSessionRef` stays in this app crate -- it's tied to
//! `ChatRuntimeState`, a purely app-crate/Tauri-runtime concept with no
//! reason for `fleet-snowfluff-ai` to know about it. The persisted
//! execution-log record stores a bare `String` for the same value
//! (`ExecutionEnd::external_ref`), not this wrapper.
//!
//! `PendingGeneration` in `chat_commands.rs` is deliberately left
//! without an `execution_id` field -- see this section's own design.md
//! entry for why that's not the same call as re-keying `cli_sessions`
//! (which *is* changed, to include `ConversationId`).

pub use fleet_snowfluff_ai::{ConversationId, ExecutionId};

/// A CLI-backed subscription provider's own session/thread id (Claude's
/// `session_id`, Codex's `thread_id`) -- a bare wrapper, deliberately
/// with no internal tag for which runtime it belongs to. Wherever this
/// is stored (`ChatRuntimeState.cli_sessions: HashMap<(ConversationId,
/// ProfileKey), ExternalSessionRef>`), the map's own key already
/// disambiguates the conversation and provider; a redundant tag on the
/// value could only ever drift from the key, never add real
/// information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSessionRef(pub String);

/// An [`ExternalSessionRef`] together with the working directory the CLI
/// was run in when it was created. `cli_sessions` holds these (and the
/// conversation's execution log persists them -- see `execution_log_store`)
/// because a CLI session may only be resumed from the directory it was
/// created under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSessionEntry {
    pub session: ExternalSessionRef,
    pub cwd: std::path::PathBuf,
    /// How many transcript messages (user/assistant, in log order) the
    /// CLI session is known to hold. Turns beyond this were answered
    /// without it -- by another provider in `mix` mode -- and are sent
    /// along when the session is next resumed.
    pub seen_turns: usize,
}

// `ConversationId`'s and `ExecutionId`'s own tests now live with their
// definitions in `fleet-snowfluff-ai/src/conversation.rs` and
// `execution.rs` respectively.
