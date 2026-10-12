//! What the assistant remembers about the user, across conversations
//! (`memory`).
//!
//! Everything here sits behind [`MemoryService`], so the runtime never depends
//! on how memories are stored. The shipped backend is SQLite with a keyword
//! index ([`store::SqliteMemory`]); an embedding-based one can be added later
//! as another implementation without touching callers.
//!
//! Three rules shape the interface:
//! - A memory is **superseded**, not overwritten, when the user corrects it:
//!   the old one leaves retrieval but stays viewable. **Forgetting** is a hard
//!   erase of the row and its index entry.
//! - A **sensitive** memory entry is visible only to a local audience. The
//!   audience is part of every read, applied inside the query, so a caller
//!   cannot forget to filter.
//! - The persona name recorded on a memory is provenance only; it is never used
//!   to filter.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod background;
pub mod block;
pub mod extraction;
pub mod fake;
pub mod policy;
pub mod rank;
pub mod store;
pub use crate::native_tools::memory_tools as tools;
mod t2s_table;
pub mod tokenizer;

#[cfg(test)]
mod suite;

/// Who asked for a memory, which decides whether sensitive entries may be
/// shown. Derived from the profile that will receive the request: only the
/// local Ollama profile is [`Audience::Local`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    Local,
    Cloud,
}

impl Audience {
    /// Who receives a request handled by `profile`. Only the local Ollama
    /// profile (and the mock used in tests) keeps text on this machine;
    /// every OpenAI or Anthropic profile is cloud, a CLI-backed
    /// subscription one included.
    pub fn for_profile(profile: crate::settings::ProfileKey) -> Audience {
        match profile.provider {
            crate::message::ProviderKind::Ollama | crate::message::ProviderKind::Mock => {
                Audience::Local
            }
            crate::message::ProviderKind::OpenAi | crate::message::ProviderKind::Anthropic => {
                Audience::Cloud
            }
        }
    }
}

/// Whether the user asked for the memory or it was derived from conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Told,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Preference,
    LifeContext,
    Correction,
    SharedMoment,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Preference => "preference",
            Kind::LifeContext => "life_context",
            Kind::Correction => "correction",
            Kind::SharedMoment => "shared_moment",
            Kind::Other => "other",
        }
    }

    pub fn parse(text: &str) -> Kind {
        match text {
            "preference" => Kind::Preference,
            "life_context" => Kind::LifeContext,
            "correction" => Kind::Correction,
            "shared_moment" => Kind::SharedMoment,
            _ => Kind::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    /// Replaced by a correction: out of retrieval, still viewable in history.
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    /// The text as the user would read it, exactly as written.
    pub text: String,
    pub kind: Kind,
    /// Topic keywords in the user's language and English; searchable.
    pub tags: Vec<String>,
    pub origin: Origin,
    /// 1 (minor) to 3 (important).
    pub importance: u8,
    pub pinned: bool,
    pub status: Status,
    pub superseded_by: Option<String>,
    /// The persona name in effect when it was written. Provenance only.
    pub persona_name: Option<String>,
    /// When set, never shown to a cloud audience.
    pub sensitive: bool,
    /// Where it came from, for the user's reference (a session and turn range).
    pub source: Option<String>,
    /// RFC 3339.
    pub created_at: String,
    pub updated_at: String,
    pub last_used_at: Option<String>,
}

/// A memory about to be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMemory {
    pub text: String,
    pub kind: Kind,
    pub tags: Vec<String>,
    pub origin: Origin,
    pub importance: u8,
    pub pinned: bool,
    pub sensitive: bool,
    pub persona_name: Option<String>,
    pub source: Option<String>,
}

impl NewMemory {
    /// A memory the user asked for, with ordinary importance and no tags.
    pub fn told(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: Kind::Other,
            tags: Vec::new(),
            origin: Origin::Told,
            importance: 2,
            pinned: false,
            sensitive: false,
            persona_name: None,
            source: None,
        }
    }

    /// A memory derived from conversation.
    pub fn auto(text: impl Into<String>) -> Self {
        Self { origin: Origin::Auto, ..Self::told(text) }
    }
}

/// Changes to an existing memory. A `None` field is left as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryEdit {
    pub text: Option<String>,
    pub kind: Option<Kind>,
    pub tags: Option<Vec<String>>,
    pub importance: Option<u8>,
    pub pinned: Option<bool>,
    pub sensitive: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredMemory {
    pub record: MemoryRecord,
    /// Relevance, higher is better.
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryError(pub String);

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

impl std::error::Error for MemoryError {}

/// The smallest score a keyword match must have to be injected. Tuned on the
/// fixtures: the weakest true top hit scored 2.52 and the strongest false one
/// 1.92. It depends on corpus size, so it is worth re-checking as the number of
/// memories grows.
pub const SCORE_FLOOR: f64 = 2.2;

/// Storage and retrieval of memories. Every read takes the [`Audience`] and
/// applies it inside the query.
#[async_trait]
pub trait MemoryService: Send + Sync {
    /// Saves a memory as given. Dedupe and supersession decisions belong to the
    /// write policy, not here.
    async fn add(&self, new: NewMemory) -> Result<MemoryRecord, MemoryError>;

    /// One memory by id, whatever its status, if the audience may see it.
    async fn get(&self, id: &str, audience: Audience) -> Result<Option<MemoryRecord>, MemoryError>;

    /// Active memories matching `query` by keyword, best first, scoring at
    /// least [`SCORE_FLOOR`].
    async fn search(
        &self,
        query: &str,
        limit: usize,
        audience: Audience,
    ) -> Result<Vec<ScoredMemory>, MemoryError>;

    /// Every active memory the audience may see (the digest ranks these).
    async fn active(&self, audience: Audience) -> Result<Vec<MemoryRecord>, MemoryError>;

    /// Active memories for the settings list, newest first, optionally narrowed
    /// to those whose text or tags contain `contains`. Shows sensitive ones
    /// too: it is the user's own list.
    async fn list(&self, contains: Option<&str>) -> Result<Vec<MemoryRecord>, MemoryError>;

    /// Superseded memories, newest first.
    async fn history(&self) -> Result<Vec<MemoryRecord>, MemoryError>;

    /// Applies `edit`, re-indexing if the text or tags changed. `None` if there
    /// is no such memory.
    async fn edit(&self, id: &str, edit: MemoryEdit) -> Result<Option<MemoryRecord>, MemoryError>;

    /// Saves `new` and marks the active memory `old_id` superseded by it.
    /// `None` (and nothing saved) if `old_id` is not an active memory.
    async fn supersede(
        &self,
        old_id: &str,
        new: NewMemory,
    ) -> Result<Option<MemoryRecord>, MemoryError>;

    /// Erases a memory entirely, including from the keyword index. `false` if
    /// there was none.
    async fn forget(&self, id: &str) -> Result<bool, MemoryError>;

    /// Erases every memory, superseded history included. Returns how many there
    /// were.
    async fn erase_all(&self) -> Result<usize, MemoryError>;

    /// Records that these memories were just shown to the model.
    async fn mark_used(&self, ids: &[String]) -> Result<(), MemoryError>;
}

pub(crate) fn new_id() -> String { format!("{:016x}", rand::random::<u64>()) }

pub(crate) fn now() -> String { chrono::Utc::now().to_rfc3339() }

/// Importance is a small whole number; anything outside 1 to 3 is clamped.
pub(crate) fn clamp_importance(importance: u8) -> u8 { importance.clamp(1, 3) }

#[cfg(test)]
mod audience_tests {
    use super::*;
    use crate::{
        message::ProviderKind,
        settings::{AuthMethod, ProfileKey},
    };

    fn key(provider: ProviderKind, auth_method: AuthMethod) -> ProfileKey {
        ProfileKey { provider, auth_method }
    }

    #[test]
    fn only_ollama_and_mock_are_local_and_every_cloud_profile_cli_backed_included_is_cloud() {
        assert_eq!(
            Audience::for_profile(key(ProviderKind::Ollama, AuthMethod::Local)),
            Audience::Local
        );
        assert_eq!(
            Audience::for_profile(key(ProviderKind::Mock, AuthMethod::Local)),
            Audience::Local
        );
        for provider in [ProviderKind::OpenAi, ProviderKind::Anthropic] {
            for auth in [AuthMethod::ApiKey, AuthMethod::Subscription] {
                assert_eq!(
                    Audience::for_profile(key(provider, auth)),
                    Audience::Cloud,
                    "{provider:?} {auth:?}"
                );
            }
        }
    }
}
