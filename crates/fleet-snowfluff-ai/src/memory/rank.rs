//! Choosing which memories ride along on every request: the digest.
//!
//! Without embeddings the digest does the heavy lifting, since it needs no
//! search and works the same in every language. It takes the highest-ranked
//! active memories that fit a token budget: pinned ones first, then by
//! importance and recency.

use chrono::{DateTime, Utc};

use super::MemoryRecord;
use crate::token_estimate::estimate_tokens;

/// Tokens the digest may use.
pub const DIGEST_BUDGET_TOKENS: usize = 400;
/// How quickly a memory's recency bonus fades.
const RECENCY_HALF_LIFE_DAYS: f64 = 30.0;
/// A pinned memory outranks everything unpinned.
const PINNED_BONUS: f64 = 1_000.0;

fn age_days(created_at: &str, now: DateTime<Utc>) -> f64 {
    DateTime::parse_from_rfc3339(created_at)
        .map(|created| (now - created.with_timezone(&Utc)).num_seconds().max(0) as f64 / 86_400.0)
        .unwrap_or(0.0)
}

/// Importance (1 to 3) plus a recency bonus that halves every thirty days.
/// Chosen without real usage data and meant to be revisited after real use.
fn score(record: &MemoryRecord, now: DateTime<Utc>) -> f64 {
    let recency = 2.0 * (-age_days(&record.created_at, now) / RECENCY_HALF_LIFE_DAYS).exp();
    let pinned = if record.pinned { PINNED_BONUS } else { 0.0 };
    pinned + f64::from(record.importance) + recency
}

/// The line a memory takes in the digest.
pub fn render_line(record: &MemoryRecord) -> String { format!("- {}", record.text) }

/// The highest-ranked of `records` that fit `budget_tokens`, best first. A
/// memory too big for what is left is skipped rather than ending the list, so
/// one long entry cannot crowd out the rest.
pub fn digest(
    mut records: Vec<MemoryRecord>,
    now: DateTime<Utc>,
    budget_tokens: usize,
) -> Vec<MemoryRecord> {
    records.sort_by(|a, b| {
        score(b, now)
            .total_cmp(&score(a, now))
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut used = 0usize;
    let mut chosen = Vec::new();
    for record in records {
        let cost = estimate_tokens(&render_line(&record)) + 1;
        if used + cost > budget_tokens {
            continue;
        }
        used += cost;
        chosen.push(record);
    }
    chosen
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::memory::{Kind, Origin, Status};

    fn record(
        id: &str,
        text: &str,
        importance: u8,
        pinned: bool,
        age_days: i64,
        now: DateTime<Utc>,
    ) -> MemoryRecord {
        MemoryRecord {
            id: id.to_string(),
            text: text.to_string(),
            kind: Kind::Other,
            tags: vec![],
            origin: Origin::Told,
            importance,
            pinned,
            status: Status::Active,
            superseded_by: None,
            persona_name: None,
            sensitive: false,
            source: None,
            created_at: (now - Duration::days(age_days)).to_rfc3339(),
            updated_at: now.to_rfc3339(),
            last_used_at: None,
        }
    }

    fn ids(records: &[MemoryRecord]) -> Vec<&str> {
        records.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn pinned_memories_come_first_whatever_their_age_or_importance() {
        let now = Utc::now();
        let chosen = digest(
            vec![
                record("new_important", "a", 3, false, 0, now),
                record("old_pinned", "b", 1, true, 900, now),
            ],
            now,
            DIGEST_BUDGET_TOKENS,
        );
        assert_eq!(ids(&chosen), vec!["old_pinned", "new_important"]);
    }

    #[test]
    fn importance_beats_recency_within_reason_and_recency_breaks_close_calls() {
        let now = Utc::now();
        let chosen = digest(
            vec![
                record("minor_today", "a", 1, false, 0, now),
                record("major_last_year", "b", 3, false, 365, now),
                record("middle_today", "c", 2, false, 0, now),
            ],
            now,
            DIGEST_BUDGET_TOKENS,
        );
        assert_eq!(ids(&chosen), vec!["middle_today", "major_last_year", "minor_today"]);
    }

    #[test]
    fn the_recency_bonus_fades_with_age() {
        let now = Utc::now();
        let chosen = digest(
            vec![record("old", "a", 2, false, 400, now), record("fresh", "b", 2, false, 1, now)],
            now,
            DIGEST_BUDGET_TOKENS,
        );
        assert_eq!(ids(&chosen), vec!["fresh", "old"]);
    }

    #[test]
    fn the_budget_is_respected() {
        let now = Utc::now();
        let records: Vec<MemoryRecord> = (0..100)
            .map(|i| {
                record(
                    &format!("m{i}"),
                    &format!("a fact about the user number {i}"),
                    2,
                    false,
                    i,
                    now,
                )
            })
            .collect();
        let chosen = digest(records, now, 100);
        let used: usize = chosen.iter().map(|r| estimate_tokens(&render_line(r)) + 1).sum();
        assert!(used <= 100, "used {used}");
        assert!(!chosen.is_empty() && chosen.len() < 100);
    }

    #[test]
    fn a_memory_too_big_for_what_is_left_is_skipped_not_fatal() {
        let now = Utc::now();
        let chosen = digest(
            vec![
                record("huge", &"word ".repeat(500), 3, false, 0, now),
                record("small", "short fact", 1, false, 5, now),
            ],
            now,
            50,
        );
        assert_eq!(ids(&chosen), vec!["small"]);
    }

    #[test]
    fn an_unparseable_timestamp_does_not_break_ranking() {
        let now = Utc::now();
        let mut broken = record("broken", "a", 2, false, 0, now);
        broken.created_at = "not a date".to_string();
        assert_eq!(digest(vec![broken], now, DIGEST_BUDGET_TOKENS).len(), 1);
    }

    #[test]
    fn an_empty_store_gives_an_empty_digest() {
        assert!(digest(vec![], Utc::now(), DIGEST_BUDGET_TOKENS).is_empty());
    }
}
