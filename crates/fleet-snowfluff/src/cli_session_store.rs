//! Persisted CLI session references (`cli-session-continuity`): a
//! sidecar file next to a conversation's chat log,
//! `<ts>_<id>.sessions.json`, recording which CLI session (Claude's
//! `session_id`, Codex's `thread_id`) each provider profile last used
//! in that conversation and the working directory it was created under.
//!
//! The sidecar lives and dies with its log: a new chat gets a new log
//! and so a new (initially absent) sidecar, so nothing here ever needs
//! deleting. Best-effort like every other `*_store.rs`: a write failure
//! is logged, and a missing, corrupt, or unrecognised-version file
//! reads as "no stored sessions" -- the worst outcome is a cold CLI
//! session (seeded with history), never a failed chat.

use std::path::{Path, PathBuf};

use fleet_snowfluff_ai::ProfileKey;
use serde::{Deserialize, Serialize};

/// Bumped only for a change old readers could not understand; a file
/// with a version this build does not know reads as empty.
const FORMAT_VERSION: u32 = 1;

/// One stored session. A list of these (rather than a map keyed by
/// profile) because `ProfileKey` is a struct, which does not serialize
/// as a JSON object key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSession {
    pub profile: ProfileKey,
    pub session_id: String,
    /// The working directory the session was created under. A session
    /// may only be resumed from that same directory: as far as is known
    /// the CLIs scope their sessions by it, so an id may not resolve
    /// under another.
    pub cwd: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct SidecarFile {
    version: u32,
    sessions: Vec<StoredSession>,
}

/// `<dir>/<ts>_<id>.jsonl` -> `<dir>/<ts>_<id>.sessions.json`.
pub fn sidecar_path(session_path: &Path) -> PathBuf {
    let stem = session_path.file_stem().map(|s| s.to_string_lossy().into_owned());
    let name = format!("{}.sessions.json", stem.as_deref().unwrap_or("session"));
    session_path.with_file_name(name)
}

/// Every session stored for this conversation. Missing, unreadable,
/// corrupt, or unknown-version files all read as empty.
pub fn load(session_path: &Path) -> Vec<StoredSession> {
    let Ok(contents) = std::fs::read_to_string(sidecar_path(session_path)) else {
        return Vec::new();
    };
    match serde_json::from_str::<SidecarFile>(&contents) {
        Ok(file) if file.version == FORMAT_VERSION => file.sessions,
        _ => Vec::new(),
    }
}

/// Records `session` for its profile, replacing that profile's earlier
/// entry and leaving every other profile's untouched.
pub fn upsert(session_path: &Path, session: StoredSession) {
    let mut sessions = load(session_path);
    sessions.retain(|s| s.profile != session.profile);
    sessions.push(session);

    let path = sidecar_path(session_path);
    let file = SidecarFile { version: FORMAT_VERSION, sessions };
    let json = serde_json::to_string(&file).expect("SidecarFile serialization is infallible");
    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            log::warn!("could not create {}: {err}", dir.display());
            return;
        }
    }
    if let Err(err) = std::fs::write(&path, json) {
        log::warn!("could not write {}: {err}", path.display());
    }
}

/// The stored session to resume for `profile` when the CLI is about to
/// run in `cwd` -- `None` if there is none, or if the one stored was
/// created under a different directory (in which case the caller
/// starts a fresh, history-seeded session instead).
pub fn usable<'a>(
    stored: &'a [StoredSession],
    profile: ProfileKey,
    cwd: &Path,
) -> Option<&'a StoredSession> {
    stored.iter().find(|s| s.profile == profile && s.cwd == cwd)
}

#[cfg(test)]
mod tests {
    use fleet_snowfluff_ai::{AuthMethod, ProviderKind};

    use super::*;

    fn temp_log(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-cli-session-store-test-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("20260924T120000Z_abc123.jsonl")
    }

    fn claude() -> ProfileKey {
        ProfileKey { provider: ProviderKind::Anthropic, auth_method: AuthMethod::Subscription }
    }

    fn codex() -> ProfileKey {
        ProfileKey { provider: ProviderKind::OpenAi, auth_method: AuthMethod::Subscription }
    }

    fn stored(profile: ProfileKey, id: &str, cwd: &str) -> StoredSession {
        StoredSession { profile, session_id: id.to_string(), cwd: PathBuf::from(cwd) }
    }

    #[test]
    fn the_sidecar_sits_next_to_its_log_and_is_named_after_it() {
        let log = Path::new("chat-logs/2026-09-24/20260924T120000Z_abc123.jsonl");
        assert_eq!(
            sidecar_path(log),
            Path::new("chat-logs/2026-09-24/20260924T120000Z_abc123.sessions.json")
        );
    }

    #[test]
    fn a_stored_session_round_trips() {
        let log = temp_log("round-trip");
        upsert(&log, stored(claude(), "sess-1", "/proj"));
        assert_eq!(load(&log), vec![stored(claude(), "sess-1", "/proj")]);
    }

    #[test]
    fn a_missing_file_reads_as_empty() {
        assert!(load(&temp_log("missing")).is_empty());
    }

    #[test]
    fn a_corrupt_file_reads_as_empty() {
        let log = temp_log("corrupt");
        std::fs::write(sidecar_path(&log), "{ not json").unwrap();
        assert!(load(&log).is_empty());
    }

    #[test]
    fn an_unknown_version_reads_as_empty() {
        let log = temp_log("version");
        upsert(&log, stored(claude(), "sess-1", "/proj"));
        assert_eq!(load(&log).len(), 1, "a real sidecar is readable before its version is bumped");

        // Change only the version of a genuinely written file, so this
        // fails for the version and not for a malformed body.
        let path = sidecar_path(&log);
        let bumped = std::fs::read_to_string(&path)
            .unwrap()
            .replace(&format!("\"version\":{FORMAT_VERSION}"), "\"version\":99");
        assert!(bumped.contains("\"version\":99"));
        std::fs::write(&path, bumped).unwrap();

        assert!(load(&log).is_empty());
    }

    #[test]
    fn upserting_one_profile_replaces_only_its_own_entry() {
        let log = temp_log("upsert");
        upsert(&log, stored(claude(), "claude-1", "/proj"));
        upsert(&log, stored(codex(), "codex-1", "/proj"));
        upsert(&log, stored(claude(), "claude-2", "/other"));

        let all = load(&log);
        assert_eq!(all.len(), 2);
        assert!(all.contains(&stored(claude(), "claude-2", "/other")));
        assert!(all.contains(&stored(codex(), "codex-1", "/proj")));
    }

    #[test]
    fn a_session_is_usable_only_for_its_profile_and_working_directory() {
        let all = vec![stored(claude(), "claude-1", "/proj"), stored(codex(), "codex-1", "/proj")];
        assert_eq!(usable(&all, claude(), Path::new("/proj")).unwrap().session_id, "claude-1");
        assert_eq!(usable(&all, codex(), Path::new("/proj")).unwrap().session_id, "codex-1");
        assert!(usable(&all, claude(), Path::new("/elsewhere")).is_none(), "cwd differs");
        assert!(
            usable(
                &all,
                ProfileKey { provider: ProviderKind::Ollama, auth_method: AuthMethod::Local },
                Path::new("/proj")
            )
            .is_none(),
            "no entry for that profile"
        );
    }

    #[test]
    fn a_different_conversations_sidecar_is_never_consulted() {
        let old_log = temp_log("old-conversation");
        upsert(&old_log, stored(claude(), "old-session", "/proj"));

        // A new chat gets a new log path, so its sidecar starts absent.
        let new_log = old_log.with_file_name("20260924T130000Z_def456.jsonl");
        assert!(load(&new_log).is_empty());
    }
}
