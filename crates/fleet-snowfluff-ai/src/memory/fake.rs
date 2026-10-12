//! An in-memory [`MemoryService`] for tests of code that uses memory. It honors
//! the same contract as the SQLite store (audience filtering, supersession,
//! hard erase) with a plain token-overlap score instead of BM25, and is checked
//! against the same behavior suite.

use std::{collections::HashSet, sync::Mutex};

use async_trait::async_trait;

use super::{
    clamp_importance, new_id, now,
    tokenizer::{index_text, normalize, single_cjk_chars, tokens},
    Audience, MemoryEdit, MemoryError, MemoryRecord, MemoryService, NewMemory, ScoredMemory,
    Status,
};

#[derive(Default)]
pub struct InMemoryMemory {
    records: Mutex<Vec<MemoryRecord>>,
}

impl InMemoryMemory {
    pub fn new() -> Self { Self::default() }
}

fn visible(record: &MemoryRecord, audience: Audience) -> bool {
    audience == Audience::Local || !record.sensitive
}

fn record_from(new: NewMemory) -> MemoryRecord {
    let stamp = now();
    MemoryRecord {
        id: new_id(),
        text: new.text,
        kind: new.kind,
        tags: new.tags,
        origin: new.origin,
        importance: clamp_importance(new.importance),
        pinned: new.pinned,
        status: Status::Active,
        superseded_by: None,
        persona_name: new.persona_name,
        sensitive: new.sensitive,
        source: new.source,
        created_at: stamp.clone(),
        updated_at: stamp,
        last_used_at: None,
    }
}

#[async_trait]
impl MemoryService for InMemoryMemory {
    async fn add(&self, new: NewMemory) -> Result<MemoryRecord, MemoryError> {
        let record = record_from(new);
        self.records.lock().unwrap().push(record.clone());
        Ok(record)
    }

    async fn get(&self, id: &str, audience: Audience) -> Result<Option<MemoryRecord>, MemoryError> {
        Ok(self
            .records
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.id == id && visible(r, audience))
            .cloned())
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
        audience: Audience,
    ) -> Result<Vec<ScoredMemory>, MemoryError> {
        let wanted: HashSet<String> = tokens(query).into_iter().collect();
        let singles = single_cjk_chars(query);
        let records = self.records.lock().unwrap();
        let mut hits: Vec<ScoredMemory> = records
            .iter()
            .filter(|r| r.status == Status::Active && visible(r, audience))
            .filter_map(|r| {
                let have: HashSet<String> =
                    index_text(&r.text, &r.tags).split_whitespace().map(str::to_string).collect();
                let shared = wanted.intersection(&have).count();
                let normalized = normalize(&format!("{} {}", r.text, r.tags.join(" ")));
                let single_hits = singles.iter().filter(|c| normalized.contains(**c)).count();
                let score = shared as f64 * 2.0 + single_hits as f64 * 0.5;
                (score > 0.0).then(|| ScoredMemory { record: r.clone(), score })
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit);
        Ok(hits)
    }

    async fn active(&self, audience: Audience) -> Result<Vec<MemoryRecord>, MemoryError> {
        Ok(self
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.status == Status::Active && visible(r, audience))
            .cloned()
            .collect())
    }

    async fn list(&self, contains: Option<&str>) -> Result<Vec<MemoryRecord>, MemoryError> {
        let needle = contains.map(normalize);
        let mut out: Vec<MemoryRecord> = self
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.status == Status::Active)
            .filter(|r| {
                needle.as_ref().is_none_or(|n| {
                    normalize(&format!("{} {}", r.text, r.tags.join(" "))).contains(n.as_str())
                })
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    async fn history(&self) -> Result<Vec<MemoryRecord>, MemoryError> {
        let mut out: Vec<MemoryRecord> = self
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.status == Status::Superseded)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    async fn edit(&self, id: &str, edit: MemoryEdit) -> Result<Option<MemoryRecord>, MemoryError> {
        let mut records = self.records.lock().unwrap();
        let Some(record) = records.iter_mut().find(|r| r.id == id) else { return Ok(None) };
        if let Some(text) = edit.text {
            record.text = text;
        }
        if let Some(kind) = edit.kind {
            record.kind = kind;
        }
        if let Some(tags) = edit.tags {
            record.tags = tags;
        }
        if let Some(importance) = edit.importance {
            record.importance = clamp_importance(importance);
        }
        if let Some(pinned) = edit.pinned {
            record.pinned = pinned;
        }
        if let Some(sensitive) = edit.sensitive {
            record.sensitive = sensitive;
        }
        record.updated_at = now();
        Ok(Some(record.clone()))
    }

    async fn supersede(
        &self,
        old_id: &str,
        new: NewMemory,
    ) -> Result<Option<MemoryRecord>, MemoryError> {
        let mut records = self.records.lock().unwrap();
        let Some(position) =
            records.iter().position(|r| r.id == old_id && r.status == Status::Active)
        else {
            return Ok(None);
        };
        let replacement = record_from(new);
        records[position].status = Status::Superseded;
        records[position].superseded_by = Some(replacement.id.clone());
        records[position].updated_at = now();
        records.push(replacement.clone());
        Ok(Some(replacement))
    }

    async fn forget(&self, id: &str) -> Result<bool, MemoryError> {
        let mut records = self.records.lock().unwrap();
        let before = records.len();
        records.retain(|r| r.id != id);
        Ok(records.len() != before)
    }

    async fn erase_all(&self) -> Result<usize, MemoryError> {
        let mut records = self.records.lock().unwrap();
        let count = records.len();
        records.clear();
        Ok(count)
    }

    async fn mark_used(&self, ids: &[String]) -> Result<(), MemoryError> {
        let stamp = now();
        for record in self.records.lock().unwrap().iter_mut().filter(|r| ids.contains(&r.id)) {
            record.last_used_at = Some(stamp.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_in_memory_fake_satisfies_the_shared_behavior_suite() {
        super::super::suite::run(InMemoryMemory::new).await;
    }
}
