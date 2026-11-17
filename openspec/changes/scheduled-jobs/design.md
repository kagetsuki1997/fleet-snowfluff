## Context

See proposal.md for motivation. The pieces this builds on:

- The agent loop (`AgentRuntime::run`) takes an immutable `ToolRegistry` per call. `for_child()` already builds a filtered copy for delegated sub-tasks, and `delegate_task` runs a child with a plain system prompt and no parent context.
- Tool permission is Auto, Confirm or Deny; Confirm assumes a person is present. Unattended runs have nobody to click.
- The memory store (`memory/store.rs`) is the pattern for a local SQLite file (WAL, `secure_delete`), and `Audience::{Local,Cloud}` already filters what a cloud profile may see.
- `scheduler.rs` is the compaction trigger logic, not a general scheduler. It supplies a `Clock` trait and the habit of testing time with a synthetic clock.
- `file_tools.rs` confines paths by canonicalizing and checking `starts_with` the root. There is **no write-file tool today**: only `read_file`, `list_directory` and `run_command`.
- `read_file` and `list_directory` are Auto only inside the configured project directory and Confirm anywhere else (and always Confirm when no project directory is set). `claude_code_cli.rs` constrains Claude Code with `--allowedTools`/`--disallowedTools` built from that CLI's own tool names, not from Aemeath's registry; Codex is similar.
- The application has no single-instance guard.
- The application has no MCP server and no inbound listener except the short-lived OAuth redirect listener. `claude_code_cli.rs` passes `--strict-mcp-config`, so sessions the app spawns ignore all MCP config.

## Goals / Non-Goals

**Goals:**
- One job service that owns the data and the rules, used by the chat tools, the Settings commands and the MCP server alike, so the three surfaces cannot disagree.
- An unattended policy that can be stated and tested: a run can use exactly the tools the user ticked and nothing else.
- Time handling correct enough for daily use across zones and daylight saving.

**Non-Goals:**
- Catching up missed runs, running while the app is closed, or a system service.
- Browser automation, persona-voiced reminders, jobs that write memory or chat.
- Defending against prompt injection by filtering text. The defense is capability limits (below).

## Decisions

### 1. The application is the single owner; MCP is a thin HTTP front

The application process holds all job state and fires all jobs. The MCP server is an HTTP listener inside the application calling the same job service. Alternative considered: a separate stdio MCP binary writing to a shared SQLite file, which would work with the app closed. Rejected because only the application can fire jobs anyway, a second writer needs its own migration and locking story, and the user preferred one owner. Cost accepted: a listening port, and no job requests while the app is closed.

### 2. Separate Job, Trigger and Run

A job owns what to do and what is allowed; triggers own when. Triggers can be edited, added and removed without re-approving the job, and a job can be duplicated with a different trigger. Runs reference both ids. Alternative: one record per schedule with the prompt inline, rejected because "same job, different schedule" would then mean copying frozen scope by hand.

### 3. Approval freezes prompt, profile and allow list

`approved_hash` over (text, profile, sorted allow list) is stored with the job. A job whose stored fields no longer match its hash does not run. The chat and MCP surfaces have no operation that edits those fields; only the Settings command does, and it recomputes the hash because the save is the approval. This makes "the model cannot widen a job" a property of the code path rather than a prompt instruction.

### 4. Allow list is a registry filter, not a runtime denial

The run builds a registry from the allow list (reusing the `for_child()` filtering idea) and passes it to `run()`. A tool not on the list is absent, so the model never spends turns on it, and `run()` is untouched. A hard-excluded set (shell, job tools, delegation, MCP connect, memory writes) is removed after the list is applied, so a hand-edited database row cannot reintroduce them. Confirm-tier tools on the list run without a popup because ticking them was the approval. That approval covers the tool, not a wider reach than the tool normally has, so the read and list tools are rebuilt for jobs with their root fixed to the project directory plus the job's output folder and no outside-path escape: a path outside is refused outright instead of falling back to Confirm (which would otherwise become an automatic allow). With no project directory configured they reach only the output folder. `run()` consults the permission handler only for Confirm-tier calls, so the job policy handler returns allow for those and nothing else relies on it to deny; the registry filter is the control.

### 5. Write access is a new output-folder tool

Because no write tool exists, jobs get `write_job_output`, which writes only under `<app data>/jobs/<job id>/out/`. It reuses the canonicalize-and-prefix check from `file_tools.rs`, rejects links that escape the folder, and bounds file size and count. Read access for jobs uses the existing read tools under their normal project-directory rules. Alternative: letting jobs use a general write tool inside the project directory; rejected for v1 as too much reach for unattended runs.

### 6. Prompt-injection stance

A job may hold both a web-reading tool and a write tool; the user chose not to forbid the combination. The mitigations are structural: no shell, writes confined to one folder, reads confined to the project directory and that folder, results delivered only to the log and a notification, no memory or chat writes, a frozen allow list, and a per-run time and turn cap. The output folder is not the only outbound channel: `web_search` sends its query to a public search service and any ticked MCP tool can contact its own server, so hostile page text could try to smuggle data from a file the job read into a query. This is accepted and bounded by the read confinement above (a job can only leak what is in the project directory and its own folder), not eliminated. Run output returned to chat is wrapped as untrusted and audience-filtered. Alternatives not built in v1 and recorded for later: a quarantined reader sub-task, and removing write tools after the first untrusted read (needs a two-phase run because the registry is immutable per call).

### 7. Schedule representation and time zones

A trigger stores a structured schedule (kind, fields) plus an IANA zone, and derives a cron expression for evaluation; raw cron entered in the advanced field is stored as the cron kind. Next-fire computation is a pure function of (schedule, zone, now), tested with fixed instants. The chat tool takes the structured form, never free text, to avoid model cron mistakes. Candidates are `croner` with `chrono-tz` (spiked in task group 0). The friendly form is hand-written in the existing vanilla TypeScript UI, with `cronstrue` for readable text, because the common cron-builder widgets are React or Vue components.

### 8. Dispatcher, separate from compaction

`JobDispatcher` ticks on the shared `Clock`, finds due triggers, applies the grace window and the skip rules, and hands runs to an executor. It is a new module; the compaction scheduler is renamed, not extended. Limits live in the dispatcher: one run per job, one agent job per backend and two overall, a bounded queue, reminders bypassing the queue, and a local-backend run waiting while a chat reply on that backend is in flight. The in-flight check reuses the signal the compaction scheduler already consumes, scoped by backend.

### 9. Audience on runs and on reads

Each run stores the audience of the profile it ran under. Read paths (chat tools, MCP) filter output by the reader's audience exactly as memory does: cloud readers get output only for cloud runs and metadata otherwise; MCP clients count as cloud.

### 10. Cloud disclosure and profile pinning

Approval records the profile and, for cloud profiles, requires an existing acknowledgment. At run time the acknowledgment and disclosure revision are checked again; failure ends the run with a reason and a notification, never a fallback and never a silent re-approval.

### 11. MCP server hosting

The listener binds loopback only, validates Host and Origin, requires a bearer token from `secrets_store`, and exposes six tools implemented over the job service. Whether plain JSON responses suffice for Streamable HTTP, or the endpoint must also support event streams, is settled in the spike. Requests it creates are pending rows with the read-only default allow list.

### 12. Storage

`jobs.db` SQLite beside the memory store, same pragmas. Tool arguments and results are never stored. Output is stored up to 32 KB per run with truncation marked. Retention is enforced on write and at startup.

### 13. Agent jobs run only on registry-controlled profiles

The allow list is enforced by building the run's registry, which only means something when the application's own loop executes the tools. That is true for the local model and API-key profiles. Claude Code and Codex run their own tool loops under `--allowedTools` names, so the same guarantee would need a second mapping from our tool names to theirs, with its own gaps. For v1 agent jobs are limited to the first group, and the profile picker, the MCP default profile and the approval check all exclude CLI-backed profiles. Reminders need no profile. Mapping an allow list onto a CLI's tool flags is a possible later change.

### 14. A job fires once

Because there is no single-instance guard, the dispatcher takes an exclusive lease on a lock file next to `jobs.db` before it starts ticking, and holds it for the process lifetime; a second instance runs without dispatching (and says so in the Jobs tab). Alternative: adding a single-instance plugin, which changes startup behavior for the whole app and is a larger decision than this change.

### 15. Pending changes, numbers and notifications

A pending request is a row with a kind: a new job (with its triggers) or a trigger change for an existing job, each applying only when approved. Concrete values: grace window 60 seconds, queue of at most 10 waiting agent jobs, a full queue skipping the new run with a logged reason. Notifications never carry an agent job's output, only its name and status; a reminder shows its own text.

### 16. Rename first

The first code group renames `scheduler.rs` to `compaction_scheduler.rs`, `SchedulerConfig` to `CompactionSchedulerConfig`, compaction's `Trigger` to `CompactionTrigger` (it would clash with the job Trigger), and moves `Clock` and `SystemClock` to `clock.rs`. The existing tests must pass unchanged apart from names.

## Risks / Trade-offs

- [Prompt injection reaching a write-capable job] → capability limits in decision 6, including read confinement; the output folder, search queries and ticked MCP tools are the outbound paths, and a note in the job detail view tells the user the folder's contents may include text from untrusted pages.
- [Listening port on the user's machine] → opt-in, loopback only, Host and Origin checks, token; reviewed in the MCP server task group before shipping.
- [Time zone and daylight-saving bugs] → pure next-fire function with fixed-instant tests across a spring-forward and a fall-back day, and an explicit zone per trigger.
- [A cloud job runs unattended and spends quota] → acknowledgment required at approval, per-run caps, three-failure auto-disable, no retries.
- [Two application instances double-firing a job] → the dispatcher lease in decision 14; only one instance dispatches.
- [Skip-missed hides that nothing ran] → the log records skipped runs with a reason, and a notification summarizes skips after startup.
- [CJK copy unreviewed] → written by the assistant, flagged for native review as with the memory change.
- [Local models are poor at tool use] → the allow list keeps their tool set small; agent jobs on local models are expected to be simple.

## Migration Plan

Additive. A new SQLite file is created lazily on first use, new settings default to off (MCP server) or empty (no jobs), and nothing changes for existing users. The rename in group 1 is internal. Rollback is removing the feature's files; no existing data is altered.

## Open Questions

- Which HTTP server crate to use for the MCP endpoint, and the default port number, are decided in the spike.
- Whether `croner` meets the project's minimum Rust version, and whether `cronstrue` ships a Traditional Chinese locale, are checked in the spike; the fallback for either is a small hand-written equivalent.
