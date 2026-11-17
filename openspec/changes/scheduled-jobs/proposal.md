## Why

Aemeath only acts when the user is typing to it. Reminders ("stretch in two hours") and recurring
work ("summarize the news every weekday morning") have nowhere to live, and the Stage 8 roadmap
entry for automation is still a bare bullet. The agent loop, permission tiers, memory audience
filtering and the MCP client all exist now, so a scheduler can be built on them instead of beside
them. The open design problem is what an agent is allowed to do when nobody is there to click a
Confirm popup; this change answers it by making the user approve a job's profile and tool allow
list once, up front.

## What Changes

- Add **scheduled jobs**: a `Job` (what to do, which profile, which tools) with one or more
  `Trigger`s (when) and a log of `Run`s. Two kinds of job: a **reminder** (fixed text, no model)
  and an **agent job** (a prompt run through the full agent loop).
- Jobs are created in chat through tools, in the new Settings **Jobs** tab, or (opt-in) by an
  external MCP client. A job's prompt, profile and tool allow list are frozen at approval; only the
  user can change them, in Settings, and saving is the new approval. Triggers can be edited
  separately, and a job can be duplicated with a different trigger.
- Unattended policy: pre-authorized and read-only by default. The allow list is chosen per job
  before it ever runs; a tool outside it is not offered to the model at all. Writes are confined
  to a job-owned output folder, and file reads are limited to the project directory plus that
  folder. `run_command` and all job-management tools are never offered to a job. Agent jobs run
  only on profiles whose tools the application controls itself (local model, API-key), not on
  Claude Code or Codex profiles.
- Run only while the app is open; a run missed while the app was closed or asleep is skipped, not
  caught up. Runs never overlap for one job, agent jobs are limited per backend, a local backend
  yields to an in-flight chat reply, and a job auto-disables after three consecutive failures.
- Schedules are entered through a friendly form (presets plus a time picker and time zone) with an
  advanced raw cron field; both produce one stored schedule, shown back in readable text.
- A run's output goes to the run log and a notification (status bubble, OS notification), never
  into the chat history, memory, or compaction. Run records are kept with retention limits.
- Chat tools let the model list jobs and read their status and history, with output excerpts
  wrapped as untrusted text and filtered by the Local/Cloud audience of the run.
- Add an **opt-in, localhost-only MCP server** (Streamable HTTP, bearer token) so Claude Code and
  other MCP clients can read jobs and request new ones. Requests from it are pending until the
  user approves them in Settings.
- Rename the compaction scheduler (`scheduler.rs`, `SchedulerConfig`, `Trigger`) to
  compaction-specific names and move the shared `Clock` to its own module. No behavior change.

## Capabilities

### New Capabilities
- `scheduled-jobs`: jobs, triggers and runs; schedule evaluation and the missed-run rule; the
  unattended permission and output-folder policy; run limits, failure handling and cloud
  disclosure; run output delivery, retention and audience; the chat tools for managing and
  inspecting jobs.
- `jobs-mcp-server`: the opt-in local MCP server for jobs, its binding, authentication and token
  handling, the tools it exposes, and the pending-request approval flow.

### Modified Capabilities
- `agent-runtime`: the native tool registry gains the job tools and a per-run filtered registry
  built from a job's allow list.
- `settings-ui`: a new Jobs tab (jobs, triggers, allow list editor, run log, pending requests, MCP
  server controls).

## Impact

- `crates/fleet-snowfluff-ai`: new `jobs/` module (store, schedule, dispatcher, run
  orchestration, tools); `scheduler.rs` renamed to `compaction_scheduler.rs`; new `clock.rs`.
- `crates/fleet-snowfluff`: job runtime and commands, notification delivery, MCP server
  hosting, `secrets_store` use for the server token.
- `ui/` and `locales/*.json`: Jobs tab and the friendly schedule form in five locales.
- New dependencies: a cron/time-zone library (`croner` and `chrono-tz` are the candidates), an
  HTTP server for the MCP endpoint, and `cronstrue` for readable schedule text. Each is confirmed
  by a spike in the first task group.
- Docs: `AI_FEATURES.md`, the READMEs, and Stage 8 in `docs/fleet-snowfluff-feature-planning.md`.
- Out of scope: agent jobs on Claude Code or Codex profiles, browser automation, catch-up of missed runs, persona-voiced reminders, writing to
  memory or chat from a job, shell access for jobs, an MCP server tool to delete jobs or edit an
  allow list.
