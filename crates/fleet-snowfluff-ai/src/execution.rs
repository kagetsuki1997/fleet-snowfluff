//! `Execution`: the persisted record of one attempt to produce a reply
//! (`execution-log-and-context`'s `execution-log` capability) -- which
//! route and profile handled it, what working directory and external
//! session (if any) it used, how it ended, and what its tool calls did.
//! One per turn, uniformly, including an attempt that is discarded and
//! never reaches the user-visible chat transcript (a `mix`-mode local
//! attempt that escalates or fails).
//!
//! `ExecutionId` lives here rather than the app crate it originated in
//! (`session_domain.rs`, which still re-exports it) for the same reason
//! `ConversationId` already moved: `ContextManager`'s `ExecutionLog`
//! trait needs this type and must not depend on the app crate.
//!
//! Modeled as two event shapes sharing one id -- a `Start` (everything
//! known before the provider is called) and an `End` (everything known
//! after) -- because persistence is append-only (design.md's Decision
//! 3): an in-place "current state" record has nowhere to represent an
//! orphaned start from a crash. [`pair_events`] is the pure merge step
//! a reader uses to turn a flat event list back into whole
//! [`Execution`]s; the app crate's `execution_log_store` owns the file
//! I/O around it, not this module.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{agent_runtime::ToolInvocation, conversation::ConversationId, settings::ProfileKey};

/// Identifies one turn's execution. Generated fresh per call; now
/// persisted (unlike its original, typing-only introduction), so it
/// round-trips through JSON to pair a `Start` with its `End`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutionId(u64);

impl ExecutionId {
    /// A fresh, effectively-unique id for one execution. Uses the
    /// thread-local RNG already available via the `rand` crate rather
    /// than pulling in a UUID crate for a value that only needs to be
    /// unique within one conversation's execution log.
    pub fn new() -> Self { Self(rand::random()) }
}

impl Default for ExecutionId {
    fn default() -> Self { Self::new() }
}

/// Which of the chat flow's execution paths produced this attempt.
/// `MixLocal` is always the local Ollama attempt in `mix` mode, which
/// has no resumable-session concept of its own (`ToolCalling` is
/// likewise never CLI-backed -- `ProviderProfile::supports_tool_calling`
/// is never true for a CLI-backed profile); only `Direct` and
/// `Fallback` can ever carry an external session reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPath {
    Direct,
    MixLocal,
    Fallback,
    ToolCalling,
}

/// How an execution ended. `Escalated` is reserved for a `MixLocal`
/// attempt that handed off *by its own content decision* (the
/// `<<ESCALATE>>` marker) -- an infra-level failure that also causes a
/// `MixLocal` attempt to be discarded (the initial call failing, or a
/// mid-stream error) is `Errored`, not `Escalated`; the planning doc's
/// own task-router design draws this line on purpose ("Task Routing 與
/// Fallback 必須分離"), and conflating the two here would blur it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Completed,
    Errored,
    Cancelled,
    Escalated,
}

/// Everything known before the provider is called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionStart {
    pub id: ExecutionId,
    pub conversation_id: ConversationId,
    pub route: ExecutionPath,
    pub profile: ProfileKey,
    pub working_dir: PathBuf,
    /// RFC 3339, matching `LogEntry.timestamp`'s own convention.
    pub started_at: String,
}

/// Everything known once an execution concludes -- normally, with an
/// error, or (via the `ExecutionRecorder` guard's `Drop`, see design.md
/// Decision 5) because it was cancelled or the process exited before it
/// could be marked otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionEnd {
    pub id: ExecutionId,
    pub status: ExecutionStatus,
    /// A CLI-backed provider's own session/thread id, if one was
    /// captured -- a bare `String`, not the app crate's
    /// `ExternalSessionRef` wrapper, so this crate never needs to depend
    /// on the app crate for it.
    pub external_ref: Option<String>,
    /// How many transcript messages `external_ref`'s session is known to
    /// hold, carried over from `cli-session-continuity`'s own catch-up
    /// cursor -- meaningless without `external_ref`.
    pub seen_turns: Option<usize>,
    pub trace: Vec<ToolInvocation>,
    pub ended_at: String,
}

/// One line of the persisted execution log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ExecutionEvent {
    Start(ExecutionStart),
    End(ExecutionEnd),
}

impl ExecutionEvent {
    pub fn id(&self) -> ExecutionId {
        match self {
            ExecutionEvent::Start(s) => s.id,
            ExecutionEvent::End(e) => e.id,
        }
    }
}

/// A `Start` merged with its `End`, if one has been recorded yet. `end`
/// is `None` for an execution still in progress, and -- indistinguishably,
/// since nothing in the file says which -- for one whose process exited
/// before any guard had the chance to run at all (not the same as a
/// `Cancelled` status, which *is* a recorded `End`; this is the absence
/// of one). Readers that need "did this execution definitely finish"
/// should treat a `None` end the same as an in-progress one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execution {
    pub id: ExecutionId,
    pub conversation_id: ConversationId,
    pub route: ExecutionPath,
    pub profile: ProfileKey,
    pub working_dir: PathBuf,
    pub started_at: String,
    pub end: Option<ExecutionEnd>,
}

/// Serializes one event as a single JSONL line (no trailing newline --
/// the caller appends it), mirroring `log::serialize_entry`'s own split
/// between pure format and the app crate's file I/O.
pub fn serialize_event(event: &ExecutionEvent) -> String {
    serde_json::to_string(event).expect("ExecutionEvent serialization is infallible")
}

/// A line that fails to parse is skipped rather than aborting the whole
/// read, mirroring `log::parse_log`'s own tolerance for a single
/// corrupted line (e.g. a truncated write from a crash mid-append).
pub fn parse_events(contents: &str) -> Vec<ExecutionEvent> {
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Merges a flat sequence of events (as read from the log, in file
/// order) into whole `Execution`s, matched by id. An `End` with no
/// matching `Start` is dropped -- it cannot happen from this crate's own
/// writer (the guard always writes `Start` before anything can write an
/// `End`), so it is treated as corrupt data rather than guessed at.
/// Order among the returned `Execution`s follows each one's `Start`
/// event's position in `events`.
pub fn pair_events(events: Vec<ExecutionEvent>) -> Vec<Execution> {
    let mut executions: Vec<Execution> = Vec::new();
    let mut index_of: std::collections::HashMap<ExecutionId, usize> =
        std::collections::HashMap::new();
    for event in events {
        match event {
            ExecutionEvent::Start(start) => {
                index_of.insert(start.id, executions.len());
                executions.push(Execution {
                    id: start.id,
                    conversation_id: start.conversation_id,
                    route: start.route,
                    profile: start.profile,
                    working_dir: start.working_dir,
                    started_at: start.started_at,
                    end: None,
                });
            }
            ExecutionEvent::End(end) => {
                if let Some(&index) = index_of.get(&end.id) {
                    executions[index].end = Some(end);
                }
                // An End with no matching Start is dropped; see doc comment.
            }
        }
    }
    executions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agent_runtime::ToolOutcome, message::ProviderKind, settings::AuthMethod};

    fn profile() -> ProfileKey {
        ProfileKey { provider: ProviderKind::Ollama, auth_method: AuthMethod::Local }
    }

    fn conv() -> ConversationId {
        ConversationId::from_session_path(std::path::Path::new("/tmp/x.jsonl"))
    }

    #[test]
    fn execution_ids_generated_in_succession_are_distinct() {
        let ids: Vec<ExecutionId> = (0..100).map(|_| ExecutionId::new()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "100 freshly generated ids should not collide");
    }

    #[test]
    fn a_start_event_round_trips_through_json() {
        let start = ExecutionStart {
            id: ExecutionId::new(),
            conversation_id: conv(),
            route: ExecutionPath::Direct,
            profile: profile(),
            working_dir: PathBuf::from("/proj"),
            started_at: "2026-09-24T00:00:00Z".to_string(),
        };
        let event = ExecutionEvent::Start(start.clone());
        let json = serde_json::to_string(&event).unwrap();
        let back: ExecutionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ExecutionEvent::Start(start));
    }

    #[test]
    fn an_end_event_round_trips_through_json() {
        let end = ExecutionEnd {
            id: ExecutionId::new(),
            status: ExecutionStatus::Completed,
            external_ref: Some("sess-1".to_string()),
            seen_turns: Some(4),
            trace: vec![ToolInvocation {
                name: "read_file".to_string(),
                outcome: ToolOutcome::Executed { ok: true },
            }],
            ended_at: "2026-09-24T00:01:00Z".to_string(),
        };
        let event = ExecutionEvent::End(end.clone());
        let json = serde_json::to_string(&event).unwrap();
        let back: ExecutionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ExecutionEvent::End(end));
    }

    #[test]
    fn start_and_end_events_are_distinguishable_on_the_wire() {
        let id = ExecutionId::new();
        let start = ExecutionEvent::Start(ExecutionStart {
            id,
            conversation_id: conv(),
            route: ExecutionPath::ToolCalling,
            profile: profile(),
            working_dir: PathBuf::from("/proj"),
            started_at: "t0".to_string(),
        });
        let end = ExecutionEvent::End(ExecutionEnd {
            id,
            status: ExecutionStatus::Errored,
            external_ref: None,
            seen_turns: None,
            trace: vec![],
            ended_at: "t1".to_string(),
        });
        assert!(serde_json::to_string(&start).unwrap().contains("\"t\":\"start\""));
        assert!(serde_json::to_string(&end).unwrap().contains("\"t\":\"end\""));
    }

    // -- serialize_event / parse_events --

    #[test]
    fn a_serialized_event_parses_back_to_the_same_event() {
        let event = start_event(ExecutionId::new());
        let line = serialize_event(&event);
        assert!(!line.contains('\n'), "one JSONL line, no trailing newline");
        assert_eq!(parse_events(&line), vec![event]);
    }

    #[test]
    fn multiple_lines_parse_in_order() {
        let (a, b) = (
            start_event(ExecutionId::new()),
            end_event(ExecutionId::new(), ExecutionStatus::Completed),
        );
        let contents = format!("{}\n{}\n", serialize_event(&a), serialize_event(&b));
        assert_eq!(parse_events(&contents), vec![a, b]);
    }

    #[test]
    fn a_corrupt_line_is_skipped_without_discarding_the_rest() {
        let good = start_event(ExecutionId::new());
        let contents = format!("{{ not json\n{}\n", serialize_event(&good));
        assert_eq!(parse_events(&contents), vec![good]);
    }

    #[test]
    fn blank_lines_are_ignored() {
        assert!(parse_events("\n\n   \n").is_empty());
    }

    // -- pair_events --

    fn start_event(id: ExecutionId) -> ExecutionEvent {
        ExecutionEvent::Start(ExecutionStart {
            id,
            conversation_id: conv(),
            route: ExecutionPath::Direct,
            profile: profile(),
            working_dir: PathBuf::from("/proj"),
            started_at: "t0".to_string(),
        })
    }

    fn end_event(id: ExecutionId, status: ExecutionStatus) -> ExecutionEvent {
        ExecutionEvent::End(ExecutionEnd {
            id,
            status,
            external_ref: None,
            seen_turns: None,
            trace: vec![],
            ended_at: "t1".to_string(),
        })
    }

    #[test]
    fn a_start_with_a_matching_end_merges_into_one_execution() {
        let id = ExecutionId::new();
        let executions =
            pair_events(vec![start_event(id), end_event(id, ExecutionStatus::Completed)]);
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].id, id);
        assert_eq!(executions[0].end.as_ref().unwrap().status, ExecutionStatus::Completed);
    }

    #[test]
    fn a_start_with_no_end_yet_is_still_returned_with_end_none() {
        let id = ExecutionId::new();
        let executions = pair_events(vec![start_event(id)]);
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].end, None);
    }

    #[test]
    fn an_end_with_no_matching_start_is_dropped() {
        let executions =
            pair_events(vec![end_event(ExecutionId::new(), ExecutionStatus::Completed)]);
        assert!(executions.is_empty());
    }

    #[test]
    fn multiple_executions_are_returned_in_start_order() {
        let (id_a, id_b) = (ExecutionId::new(), ExecutionId::new());
        let executions = pair_events(vec![
            start_event(id_a),
            start_event(id_b),
            end_event(id_a, ExecutionStatus::Escalated),
            end_event(id_b, ExecutionStatus::Completed),
        ]);
        assert_eq!(executions.len(), 2);
        assert_eq!(executions[0].id, id_a);
        assert_eq!(executions[1].id, id_b);
    }
}
