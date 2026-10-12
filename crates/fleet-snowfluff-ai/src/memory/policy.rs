//! The rules every memory write goes through, whichever path it came from (the
//! model's tool call, the host-side backstops, or extraction at compaction).
//!
//! - A memory that says the same thing as an active one is not stored twice.
//! - Only a memory the user asked for may replace another; one derived from
//!   conversation never does.
//! - A derived memory about a sensitive topic is discarded. A memory the user
//!   asked for is kept, but marked sensitive so cloud profiles never see it.

use super::{tokenizer::normalize, MemoryError, MemoryRecord, MemoryService, NewMemory, Origin};
use crate::sensitive;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved(MemoryRecord),
    /// Replaced the active memory with this id; the record is the new one.
    Superseded {
        new: MemoryRecord,
        old_id: String,
    },
    /// An active memory already says this; nothing was written.
    Duplicate(MemoryRecord),
    /// Derived from conversation and about a sensitive topic, so not kept.
    Discarded,
}

fn same_text(a: &str, b: &str) -> bool { normalize(a).trim() == normalize(b).trim() }

/// Applies the write policy to `new`. `supersedes` names the memory a
/// correction replaces.
pub async fn save(
    service: &dyn MemoryService,
    mut new: NewMemory,
    supersedes: Option<&str>,
) -> Result<SaveOutcome, MemoryError> {
    if new.text.trim().is_empty() {
        return Ok(SaveOutcome::Discarded);
    }
    let topic_is_sensitive = sensitive::is_sensitive(&new.text);
    match new.origin {
        Origin::Auto if topic_is_sensitive => return Ok(SaveOutcome::Discarded),
        Origin::Told if topic_is_sensitive => new.sensitive = true,
        _ => {}
    }

    // Duplicates are checked against the whole list the user sees, sensitive
    // entries included.
    let existing = service.list(None).await?;
    if let Some(duplicate) = existing
        .into_iter()
        .find(|record| same_text(&record.text, &new.text) && Some(record.id.as_str()) != supersedes)
    {
        return Ok(SaveOutcome::Duplicate(duplicate));
    }

    if new.origin == Origin::Told {
        if let Some(old_id) = supersedes {
            if let Some(saved) = service.supersede(old_id, new.clone()).await? {
                return Ok(SaveOutcome::Superseded { new: saved, old_id: old_id.to_string() });
            }
            // The named memory is gone or already replaced: keep the fact as a
            // new memory.
        }
    }
    Ok(SaveOutcome::Saved(service.add(new).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{fake::InMemoryMemory, Audience, Status};

    #[tokio::test]
    async fn a_duplicate_is_not_stored_twice() {
        let service = InMemoryMemory::new();
        let first = save(&service, NewMemory::told("Likes green tea"), None).await.unwrap();
        let second = save(&service, NewMemory::auto("  likes GREEN tea "), None).await.unwrap();
        assert!(matches!(first, SaveOutcome::Saved(_)));
        assert!(matches!(second, SaveOutcome::Duplicate(_)));
        assert_eq!(service.list(None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_traditional_and_simplified_spelling_count_as_the_same_memory() {
        let service = InMemoryMemory::new();
        save(&service, NewMemory::told("住在台北學習日語"), None).await.unwrap();
        let again = save(&service, NewMemory::told("住在台北学习日语"), None).await.unwrap();
        assert!(matches!(again, SaveOutcome::Duplicate(_)));
    }

    #[tokio::test]
    async fn a_correction_hides_the_old_memory() {
        let service = InMemoryMemory::new();
        let SaveOutcome::Saved(old) =
            save(&service, NewMemory::told("Lives in Taichung"), None).await.unwrap()
        else {
            panic!("saved")
        };
        let outcome =
            save(&service, NewMemory::told("Lives in Taipei"), Some(&old.id)).await.unwrap();
        let SaveOutcome::Superseded { new, old_id } = outcome else { panic!("{outcome:?}") };
        assert_eq!(old_id, old.id);
        assert_eq!(new.text, "Lives in Taipei");
        let active = service.active(Audience::Local).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(service.history().await.unwrap()[0].status, Status::Superseded);
    }

    #[tokio::test]
    async fn a_memory_derived_from_conversation_never_supersedes() {
        let service = InMemoryMemory::new();
        let SaveOutcome::Saved(old) =
            save(&service, NewMemory::told("Lives in Taichung"), None).await.unwrap()
        else {
            panic!("saved")
        };
        let outcome =
            save(&service, NewMemory::auto("Lives in Taipei"), Some(&old.id)).await.unwrap();
        assert!(matches!(outcome, SaveOutcome::Saved(_)));
        assert_eq!(service.active(Audience::Local).await.unwrap().len(), 2, "both stay active");
        assert!(service.history().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_correction_naming_a_missing_memory_is_kept_as_a_new_one() {
        let service = InMemoryMemory::new();
        let outcome =
            save(&service, NewMemory::told("Lives in Taipei"), Some("gone")).await.unwrap();
        assert!(matches!(outcome, SaveOutcome::Saved(_)));
        assert_eq!(service.list(None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_derived_memory_about_a_sensitive_topic_is_discarded() {
        let service = InMemoryMemory::new();
        let outcome =
            save(&service, NewMemory::auto("Takes antidepressant medication every day"), None)
                .await
                .unwrap();
        assert_eq!(outcome, SaveOutcome::Discarded);
        assert!(service.list(None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_sensitive_memory_the_user_asked_for_is_kept_but_hidden_from_cloud() {
        let service = InMemoryMemory::new();
        let outcome =
            save(&service, NewMemory::told("Takes antidepressant medication every day"), None)
                .await
                .unwrap();
        let SaveOutcome::Saved(saved) = outcome else { panic!("{outcome:?}") };
        assert!(saved.sensitive);
        assert!(service.active(Audience::Cloud).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn blank_text_is_not_saved() {
        let service = InMemoryMemory::new();
        assert_eq!(
            save(&service, NewMemory::told("   "), None).await.unwrap(),
            SaveOutcome::Discarded
        );
    }
}
