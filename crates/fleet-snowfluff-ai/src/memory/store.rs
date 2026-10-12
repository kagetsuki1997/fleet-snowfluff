//! The SQLite backend for [`MemoryService`]: one database file, a keyword index
//! over text we tokenize ourselves (see `tokenizer`), and nothing that needs a
//! model.
//!
//! Details that matter:
//! - Opened **lazily** on first use, so a damaged or unreadable database never
//!   stops the app from starting; every call just returns an error and chat
//!   carries on without memory.
//! - **Erasing is real.** `secure_delete` overwrites deleted content and the
//!   write-ahead log is checkpointed after a delete, so a forgotten memory is
//!   not still sitting in the file.
//! - The relevance **floor scales down for a small store**. SQLite's BM25 gives
//!   near-zero scores when there are only a few documents (a term in one of one
//!   documents has no discriminating power), so a fixed cutoff tuned on thirty
//!   memories would discard every true match in a store of three.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::{
    clamp_importance, new_id, now,
    tokenizer::{index_text, normalize, single_cjk_chars, tokens},
    Audience, Kind, MemoryEdit, MemoryError, MemoryRecord, MemoryService, NewMemory, Origin,
    ScoredMemory, Status, SCORE_FLOOR,
};

/// The schema version this build understands.
const SCHEMA_VERSION: i64 = 1;

const COLUMNS: &str = "id, text, kind, tags, origin, importance, pinned, status, superseded_by, \
                       persona_name, sensitive, source, created_at, updated_at, last_used_at";

const SCHEMA_V1: &str = "
CREATE TABLE memories (
    id            TEXT PRIMARY KEY,
    text          TEXT NOT NULL,
    kind          TEXT NOT NULL,
    tags          TEXT NOT NULL,
    origin        TEXT NOT NULL,
    importance    INTEGER NOT NULL DEFAULT 2,
    pinned        INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL DEFAULT 'active',
    superseded_by TEXT,
    persona_name  TEXT,
    sensitive     INTEGER NOT NULL DEFAULT 0,
    source        TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    last_used_at  TEXT,
    norm_text     TEXT NOT NULL
);
CREATE VIRTUAL TABLE memories_fts USING fts5(tokens, tokenize = 'porter unicode61');
CREATE INDEX memories_status ON memories(status);
";

struct Inner {
    /// `None` is an in-memory database (tests).
    path: Option<PathBuf>,
    connection: Option<Connection>,
}

pub struct SqliteMemory {
    inner: Arc<Mutex<Inner>>,
}

impl SqliteMemory {
    /// A store at `path`, created on first use.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { inner: Arc::new(Mutex::new(Inner { path: Some(path.into()), connection: None })) }
    }

    /// A store that lives only in memory.
    pub fn in_memory() -> Self {
        Self { inner: Arc::new(Mutex::new(Inner { path: None, connection: None })) }
    }

    async fn with_connection<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, MemoryError> {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut guard =
                inner.lock().map_err(|_| MemoryError("the memory store is unavailable".into()))?;
            if guard.connection.is_none() {
                let opened = open(guard.path.as_deref()).map_err(|err| {
                    MemoryError(format!("could not open the memory store: {err}"))
                })?;
                guard.connection = Some(opened);
            }
            let connection = guard.connection.as_mut().expect("opened above");
            work(connection).map_err(|err| MemoryError(err.to_string()))
        })
        .await
        .map_err(|err| MemoryError(format!("the memory store task failed: {err}")))?
    }
}

fn open(path: Option<&Path>) -> Result<Connection, String> {
    let connection = match path {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
            }
            Connection::open(path).map_err(|err| err.to_string())?
        }
        None => Connection::open_in_memory().map_err(|err| err.to_string())?,
    };
    // Deleted content is overwritten, not just unlinked, so an erased memory does
    // not linger.
    connection.pragma_update(None, "secure_delete", "ON").map_err(|err| err.to_string())?;
    let _mode: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .map_err(|err| err.to_string())?;
    migrate(&connection)?;
    Ok(connection)
}

fn migrate(connection: &Connection) -> Result<(), String> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|err| err.to_string())?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "the memory database is version {version}, newer than this app understands"
        ));
    }
    if version < 1 {
        connection.execute_batch(SCHEMA_V1).map_err(|err| err.to_string())?;
        connection.pragma_update(None, "user_version", 1).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn record_from_row(row: &Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let tags: String = row.get(3)?;
    let origin: String = row.get(4)?;
    let status: String = row.get(7)?;
    Ok(MemoryRecord {
        id: row.get(0)?,
        text: row.get(1)?,
        kind: Kind::parse(&row.get::<_, String>(2)?),
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        origin: if origin == "auto" { Origin::Auto } else { Origin::Told },
        importance: row.get::<_, i64>(5)?.clamp(1, 3) as u8,
        pinned: row.get::<_, i64>(6)? != 0,
        status: if status == "superseded" { Status::Superseded } else { Status::Active },
        superseded_by: row.get(8)?,
        persona_name: row.get(9)?,
        sensitive: row.get::<_, i64>(10)? != 0,
        source: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        last_used_at: row.get(14)?,
    })
}

fn norm_text(text: &str, tags: &[String]) -> String {
    normalize(&format!("{text} {}", tags.join(" ")))
}

fn origin_str(origin: Origin) -> &'static str {
    match origin {
        Origin::Told => "told",
        Origin::Auto => "auto",
    }
}

/// Inserts a memory and its index entry. Returns the saved record.
fn insert(connection: &Connection, new: NewMemory) -> rusqlite::Result<MemoryRecord> {
    let id = new_id();
    let stamp = now();
    let importance = clamp_importance(new.importance);
    let tags_json = serde_json::to_string(&new.tags).unwrap_or_else(|_| "[]".into());
    connection.execute(
        "INSERT INTO memories (id, text, kind, tags, origin, importance, pinned, status, \
         persona_name, sensitive, source, created_at, updated_at, norm_text) VALUES (?1, ?2, ?3, \
         ?4, ?5, ?6, ?7, 'active', ?8, ?9, ?10, ?11, ?11, ?12)",
        params![
            id,
            new.text,
            new.kind.as_str(),
            tags_json,
            origin_str(new.origin),
            importance,
            new.pinned,
            new.persona_name,
            new.sensitive,
            new.source,
            stamp,
            norm_text(&new.text, &new.tags),
        ],
    )?;
    let rowid = connection.last_insert_rowid();
    connection.execute(
        "INSERT INTO memories_fts (rowid, tokens) VALUES (?1, ?2)",
        params![rowid, index_text(&new.text, &new.tags)],
    )?;
    Ok(MemoryRecord {
        id,
        text: new.text,
        kind: new.kind,
        tags: new.tags,
        origin: new.origin,
        importance,
        pinned: new.pinned,
        status: Status::Active,
        superseded_by: None,
        persona_name: new.persona_name,
        sensitive: new.sensitive,
        source: new.source,
        created_at: stamp.clone(),
        updated_at: stamp,
        last_used_at: None,
    })
}

fn select_one(connection: &Connection, id: &str) -> rusqlite::Result<Option<MemoryRecord>> {
    connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM memories WHERE id = ?1"),
            params![id],
            record_from_row,
        )
        .optional()
}

/// Escapes `%`, `_`, and `\` so user text is matched literally in a LIKE.
fn like_pattern(text: &str) -> String {
    let escaped: String = text
        .chars()
        .flat_map(|c| match c {
            '%' | '_' | '\\' => vec!['\\', c],
            other => vec![other],
        })
        .collect();
    format!("%{escaped}%")
}

/// The relevance floor for a store of `active` memories: [`SCORE_FLOOR`] for a
/// large store, lower for a small one. A term found in one of N memories scores
/// at most `ln((N - 0.5) / 1.5)` under BM25, so the floor is capped at 70% of
/// that, which a single true keyword match clears at any size.
fn effective_floor(active: i64) -> f64 {
    let single_match = ((active as f64 - 0.5) / 1.5).ln().max(0.0);
    SCORE_FLOOR.min(0.7 * single_match)
}

/// Wipes the write-ahead log into the main file so deleted content is not left
/// in it.
fn checkpoint(connection: &Connection) {
    let _ = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
}

#[async_trait]
impl MemoryService for SqliteMemory {
    async fn add(&self, new: NewMemory) -> Result<MemoryRecord, MemoryError> {
        self.with_connection(move |connection| {
            let transaction = connection.transaction()?;
            let record = insert(&transaction, new)?;
            transaction.commit()?;
            Ok(record)
        })
        .await
    }

    async fn get(&self, id: &str, audience: Audience) -> Result<Option<MemoryRecord>, MemoryError> {
        let id = id.to_string();
        self.with_connection(move |connection| {
            Ok(select_one(connection, &id)?
                .filter(|record| audience == Audience::Local || !record.sensitive))
        })
        .await
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
        audience: Audience,
    ) -> Result<Vec<ScoredMemory>, MemoryError> {
        let pieces = tokens(query);
        let singles = single_cjk_chars(query);
        let local = audience == Audience::Local;
        self.with_connection(move |connection| {
            let active: i64 = connection.query_row(
                "SELECT count(*) FROM memories WHERE status = 'active'",
                [],
                |row| row.get(0),
            )?;
            let floor = effective_floor(active);
            let mut hits: Vec<ScoredMemory> = Vec::new();

            if !pieces.is_empty() {
                let expression = pieces
                    .iter()
                    .map(|piece| format!("\"{}\"", piece.replace('"', "")))
                    .collect::<Vec<_>>()
                    .join(" OR ");
                let mut statement = connection.prepare(&format!(
                    "SELECT {}, bm25(memories_fts) FROM memories_fts JOIN memories m ON m.rowid = \
                     memories_fts.rowid WHERE memories_fts MATCH ?1 AND m.status = 'active' AND \
                     (?2 OR m.sensitive = 0) ORDER BY bm25(memories_fts) LIMIT ?3",
                    COLUMNS.split(", ").map(|c| format!("m.{c}")).collect::<Vec<_>>().join(", ")
                ))?;
                let rows = statement.query_map(
                    params![expression, local, i64::try_from(limit).unwrap_or(i64::MAX)],
                    |row| {
                        Ok(ScoredMemory {
                            record: record_from_row(row)?,
                            score: -row.get::<_, f64>(15)?,
                        })
                    },
                )?;
                for hit in rows {
                    let hit = hit?;
                    if hit.score >= floor {
                        hits.push(hit);
                    }
                }
            }

            // A lone CJK character has no bigram, so it is matched by scanning the
            // normalized text.
            for character in singles {
                if hits.len() >= limit {
                    break;
                }
                let mut statement = connection.prepare(&format!(
                    "SELECT {COLUMNS} FROM memories WHERE status = 'active' AND (?1 OR sensitive \
                     = 0) AND norm_text LIKE ?2 ESCAPE '\\'"
                ))?;
                let rows = statement.query_map(
                    params![local, like_pattern(&character.to_string())],
                    record_from_row,
                )?;
                for record in rows {
                    let record = record?;
                    if hits.len() < limit && !hits.iter().any(|hit| hit.record.id == record.id) {
                        hits.push(ScoredMemory { record, score: floor.max(0.1) });
                    }
                }
            }
            Ok(hits)
        })
        .await
    }

    async fn active(&self, audience: Audience) -> Result<Vec<MemoryRecord>, MemoryError> {
        let local = audience == Audience::Local;
        self.with_connection(move |connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {COLUMNS} FROM memories WHERE status = 'active' AND (?1 OR sensitive = 0)"
            ))?;
            let rows = statement.query_map(params![local], record_from_row)?;
            rows.collect()
        })
        .await
    }

    async fn list(&self, contains: Option<&str>) -> Result<Vec<MemoryRecord>, MemoryError> {
        let pattern = contains.map(|text| like_pattern(&normalize(text)));
        self.with_connection(move |connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {COLUMNS} FROM memories WHERE status = 'active' AND (?1 IS NULL OR \
                 norm_text LIKE ?1 ESCAPE '\\') ORDER BY created_at DESC"
            ))?;
            let rows = statement.query_map(params![pattern], record_from_row)?;
            rows.collect()
        })
        .await
    }

    async fn history(&self) -> Result<Vec<MemoryRecord>, MemoryError> {
        self.with_connection(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {COLUMNS} FROM memories WHERE status = 'superseded' ORDER BY updated_at \
                 DESC"
            ))?;
            let rows = statement.query_map([], record_from_row)?;
            rows.collect()
        })
        .await
    }

    async fn edit(&self, id: &str, edit: MemoryEdit) -> Result<Option<MemoryRecord>, MemoryError> {
        let id = id.to_string();
        self.with_connection(move |connection| {
            let Some(mut record) = select_one(connection, &id)? else { return Ok(None) };
            let reindex = edit.text.is_some() || edit.tags.is_some();
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

            let transaction = connection.transaction()?;
            transaction.execute(
                "UPDATE memories SET text = ?2, kind = ?3, tags = ?4, importance = ?5, pinned = \
                 ?6, sensitive = ?7, updated_at = ?8, norm_text = ?9 WHERE id = ?1",
                params![
                    record.id,
                    record.text,
                    record.kind.as_str(),
                    serde_json::to_string(&record.tags).unwrap_or_else(|_| "[]".into()),
                    record.importance,
                    record.pinned,
                    record.sensitive,
                    record.updated_at,
                    norm_text(&record.text, &record.tags),
                ],
            )?;
            if reindex && record.status == Status::Active {
                let rowid: i64 = transaction.query_row(
                    "SELECT rowid FROM memories WHERE id = ?1",
                    params![record.id],
                    |row| row.get(0),
                )?;
                transaction.execute("DELETE FROM memories_fts WHERE rowid = ?1", params![rowid])?;
                transaction.execute(
                    "INSERT INTO memories_fts (rowid, tokens) VALUES (?1, ?2)",
                    params![rowid, index_text(&record.text, &record.tags)],
                )?;
            }
            transaction.commit()?;
            Ok(Some(record))
        })
        .await
    }

    async fn supersede(
        &self,
        old_id: &str,
        new: NewMemory,
    ) -> Result<Option<MemoryRecord>, MemoryError> {
        let old_id = old_id.to_string();
        self.with_connection(move |connection| {
            let transaction = connection.transaction()?;
            let old: Option<i64> = transaction
                .query_row(
                    "SELECT rowid FROM memories WHERE id = ?1 AND status = 'active'",
                    params![old_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(old_rowid) = old else { return Ok(None) };
            let replacement = insert(&transaction, new)?;
            transaction.execute(
                "UPDATE memories SET status = 'superseded', superseded_by = ?2, updated_at = ?3 \
                 WHERE id = ?1",
                params![old_id, replacement.id, now()],
            )?;
            // The old memory leaves retrieval, so it leaves the keyword index too.
            transaction.execute("DELETE FROM memories_fts WHERE rowid = ?1", params![old_rowid])?;
            transaction.commit()?;
            Ok(Some(replacement))
        })
        .await
    }

    async fn forget(&self, id: &str) -> Result<bool, MemoryError> {
        let id = id.to_string();
        self.with_connection(move |connection| {
            let transaction = connection.transaction()?;
            let rowid: Option<i64> = transaction
                .query_row("SELECT rowid FROM memories WHERE id = ?1", params![id], |row| {
                    row.get(0)
                })
                .optional()?;
            let Some(rowid) = rowid else { return Ok(false) };
            transaction.execute("DELETE FROM memories_fts WHERE rowid = ?1", params![rowid])?;
            transaction.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
            transaction.commit()?;
            checkpoint(connection);
            Ok(true)
        })
        .await
    }

    async fn erase_all(&self) -> Result<usize, MemoryError> {
        self.with_connection(|connection| {
            let transaction = connection.transaction()?;
            let count: i64 =
                transaction.query_row("SELECT count(*) FROM memories", [], |row| row.get(0))?;
            transaction.execute("DELETE FROM memories_fts", [])?;
            transaction.execute("DELETE FROM memories", [])?;
            transaction.commit()?;
            checkpoint(connection);
            Ok(count as usize)
        })
        .await
    }

    async fn mark_used(&self, ids: &[String]) -> Result<(), MemoryError> {
        let ids = ids.to_vec();
        self.with_connection(move |connection| {
            let stamp = now();
            let transaction = connection.transaction()?;
            for id in &ids {
                transaction.execute(
                    "UPDATE memories SET last_used_at = ?2 WHERE id = ?1",
                    params![id, stamp],
                )?;
            }
            transaction.commit()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-memory-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("memory.db")
    }

    #[tokio::test]
    async fn the_sqlite_store_satisfies_the_shared_behavior_suite() {
        super::super::suite::run(SqliteMemory::in_memory).await;
    }

    #[tokio::test]
    async fn the_first_use_creates_the_database_and_a_reopen_finds_the_data() {
        let path = temp_db("reopen");
        {
            let store = SqliteMemory::at(&path);
            assert!(!path.exists(), "nothing is created until first use");
            store.add(NewMemory::told("Likes green tea")).await.unwrap();
        }
        assert!(path.exists());
        let reopened = SqliteMemory::at(&path);
        assert_eq!(reopened.list(None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_unreadable_path_returns_an_error_instead_of_panicking() {
        let blocker = temp_db("blocked").with_extension("blocker");
        std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        std::fs::write(&blocker, "a file where a directory should be").unwrap();
        let store = SqliteMemory::at(blocker.join("inside").join("memory.db"));
        let error = store.list(None).await.unwrap_err();
        assert!(error.0.contains("could not open the memory store"), "{error}");
        // And it keeps trying: the next call errors the same way instead of poisoning
        // the store.
        assert!(store.list(None).await.is_err());
    }

    #[tokio::test]
    async fn a_database_from_a_newer_version_is_refused_not_misread() {
        let path = temp_db("newer");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);
        let error = SqliteMemory::at(&path).list(None).await.unwrap_err();
        assert!(error.0.contains("newer than this app"), "{error}");
    }

    #[tokio::test]
    async fn the_score_floor_shrinks_for_a_small_store_and_is_full_for_a_large_one() {
        assert!(effective_floor(1) < 0.01);
        assert!(effective_floor(3) < 0.4);
        assert!(effective_floor(30) > 2.0);
        assert!((effective_floor(5_000) - SCORE_FLOOR).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_forgotten_memory_is_gone_from_the_file_not_just_the_listing() {
        let path = temp_db("secure-delete");
        let store = SqliteMemory::at(&path);
        let secret = "UNIQUE-SECRET-WORDS-zxqv9174";
        let memory = store.add(NewMemory::told(format!("remember {secret} please"))).await.unwrap();
        store.add(NewMemory::told("Likes green tea")).await.unwrap();
        assert!(store.forget(&memory.id).await.unwrap());

        let mut leftover = false;
        for file in std::fs::read_dir(path.parent().unwrap()).unwrap().flatten() {
            let bytes = std::fs::read(file.path()).unwrap();
            if bytes.windows(secret.len()).any(|window| window == secret.as_bytes()) {
                leftover = true;
            }
        }
        assert!(!leftover, "the forgotten text is still present in a database file");
    }

    #[tokio::test]
    async fn erasing_everything_leaves_no_text_in_the_files() {
        let path = temp_db("erase-all");
        let store = SqliteMemory::at(&path);
        let marker = "ANOTHER-UNIQUE-MARKER-qj4482";
        let old = store.add(NewMemory::told(format!("first {marker}"))).await.unwrap();
        store.supersede(&old.id, NewMemory::told(format!("second {marker}"))).await.unwrap();
        store.erase_all().await.unwrap();

        for file in std::fs::read_dir(path.parent().unwrap()).unwrap().flatten() {
            let bytes = std::fs::read(file.path()).unwrap();
            assert!(
                !bytes.windows(marker.len()).any(|window| window == marker.as_bytes()),
                "{:?} still contains erased text",
                file.path()
            );
        }
    }

    #[tokio::test]
    async fn text_with_sql_and_fts_syntax_is_treated_as_data() {
        let store = SqliteMemory::in_memory();
        store
            .add(NewMemory::told("Likes \"quoted\" things; DROP TABLE memories; --"))
            .await
            .unwrap();
        let hits = store.search("quoted \" OR NEAR( AND * ' --", 5, Audience::Local).await.unwrap();
        assert_eq!(hits.len(), 1, "the query was parsed as words, not as FTS syntax");
        assert_eq!(store.list(None).await.unwrap().len(), 1, "nothing was dropped");
        assert_eq!(store.list(Some("100%")).await.unwrap().len(), 0, "LIKE wildcards are literal");
    }
}
