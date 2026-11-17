## Purpose

Lets the user have Aemeath do things on a schedule while the application is open: show a reminder, or run a prompt through the agent with a tool allow list the user approved in advance. This capability covers what a job is, when it runs, what it may do unattended, and how its results are recorded and shown.

## ADDED Requirements

### Requirement: Jobs, triggers and runs

The application SHALL model a scheduled job as a job with one or more independent triggers and a log of runs. A job SHALL be either a reminder (fixed text, no model call) or an agent job (a prompt run through the agent with a chosen provider profile and tool allow list). A trigger SHALL be a one-time moment, a repeating interval, or a recurring calendar schedule, each with an explicit time zone. Deleting a trigger SHALL leave the job and its other triggers in place. Deleting a job SHALL delete its triggers and run records but SHALL NOT delete the files the job wrote to its output folder.

#### Scenario: A job with two triggers

- **WHEN** a job has a weekday-morning trigger and a Saturday trigger
- **THEN** it runs at both, and removing one trigger leaves the other firing

#### Scenario: Deleting a job keeps its output files

- **WHEN** the user deletes a job that wrote files to its output folder
- **THEN** the job, triggers and run records are removed and the files remain on disk, with their location shown to the user

### Requirement: Schedule entry

The application SHALL let the user enter a schedule through a form offering one-time, every-N-minutes, hourly, daily, weekdays, weekly and monthly presets with a time and a time zone, and through an advanced field accepting a raw cron expression. Both entry paths SHALL produce the same stored schedule, and the application SHALL show every stored schedule as readable text in the user's language together with its next fire time. A schedule the application cannot parse SHALL be rejected with an explanation rather than saved.

#### Scenario: A preset and a cron expression give the same result

- **WHEN** the user picks "weekdays at 09:00" in one job and enters the equivalent cron expression in another
- **THEN** both are stored identically and shown with the same readable text and next fire time

#### Scenario: An invalid expression is rejected

- **WHEN** the user enters a cron expression that does not parse
- **THEN** nothing is saved and the user is told what is wrong

### Requirement: Time zone and daylight saving

Each trigger SHALL be evaluated in its own named time zone, independent of the machine's current zone. A local time that does not exist on a daylight-saving change day SHALL run at the next valid moment, and a local time that occurs twice SHALL run once.

#### Scenario: The machine changes time zone

- **WHEN** a trigger set for 09:00 in Asia/Taipei is evaluated on a machine now set to another zone
- **THEN** it fires at 09:00 Taipei time

#### Scenario: A repeated local hour runs once

- **WHEN** a daily trigger's local time occurs twice on a clock-change day
- **THEN** the job runs once that day

### Requirement: Jobs run only while the application is open

Jobs SHALL fire only while the application is running. A run whose fire time passed while the application was closed or the machine asleep SHALL be skipped and recorded as skipped, not caught up later. A run that is late by no more than sixty seconds because of scheduling delay SHALL still run.

#### Scenario: A missed run is skipped

- **WHEN** the application starts after a trigger's fire time passed while it was closed
- **THEN** no run starts for that missed time, and the log records it as skipped because the application was not running

#### Scenario: A slightly late run still happens

- **WHEN** a trigger's fire time passed a few seconds ago because the scheduler was busy
- **THEN** the job runs

### Requirement: A job fires once

A trigger's fire time SHALL start at most one run, even if the application is launched more than once or a second instance is started while the first is running.

#### Scenario: A second instance does not duplicate runs

- **WHEN** a second application instance is started while another is running
- **THEN** only one of them runs jobs, and each due trigger produces one run

### Requirement: Jobs are approved before they can run

A job SHALL NOT run until the user has approved it. A job's prompt or reminder text, provider profile and tool allow list SHALL be fixed at approval. Only the user, through the Settings Jobs tab, SHALL be able to change them, and saving such a change SHALL count as a new approval. Triggers SHALL be editable without changing the job's approval, and the user SHALL be able to duplicate a job with a different trigger, the duplicate being approved when the user saves it. A job created by the model in chat SHALL be approved through a confirmation that shows its text, profile, allow list and next fire times. A job, or a change to a job's triggers, requested by an external client SHALL stay pending and SHALL NOT take effect until the user approves it, and an unapproved request SHALL expire after seven days.

#### Scenario: Creating a job in chat

- **WHEN** the model asks to create a job from the user's request
- **THEN** the user sees the job's prompt, profile, tools and the next three fire times, and the job exists only if the user confirms

#### Scenario: The model cannot widen a job's tools

- **WHEN** the model tries to change an existing job's allow list or prompt
- **THEN** the request is refused, and only a Settings edit by the user can make that change

#### Scenario: A pending request expires

- **WHEN** a job requested by an external client is not approved within seven days
- **THEN** it is removed and never runs

#### Scenario: A requested trigger change waits for approval

- **WHEN** an external client asks to change an existing job's trigger
- **THEN** the job keeps its current trigger until the user approves the change

### Requirement: Unattended tool policy

An agent job SHALL be offered only the tools on its approved allow list; any other tool SHALL NOT be present for that run, so the model cannot call it. The default allow list SHALL contain only read-only tools. Tools that change things SHALL be added only by the user in Settings. A tool on the allow list SHALL run without a confirmation prompt. Shell command execution, job management, connecting MCP servers, delegating to sub-agents, and memory writes SHALL never be available to a job. A job's file writes SHALL be possible only through a tool that writes inside the job's own output folder, and any path that resolves outside that folder, including through links or parent references, SHALL be refused. The read-file and list-directory tools in a job run SHALL be limited to the configured project directory and the job's own output folder: a path outside them SHALL be refused, never allowed because the tool is on the allow list, and when no project directory is configured they SHALL reach only the job's output folder.

#### Scenario: A tool outside the allow list is not offered

- **WHEN** an agent job runs whose allow list excludes the web search tool
- **THEN** the model's available tools do not include web search

#### Scenario: Writes stay in the output folder

- **WHEN** a job's write tool is given a path that resolves outside the job's output folder, whether by a parent reference or a link
- **THEN** nothing is written and the call fails

#### Scenario: A listed read tool cannot leave the project directory

- **WHEN** a job whose allow list includes the read-file tool is asked to read a path outside the project directory and the job's output folder
- **THEN** the read is refused without any confirmation prompt

#### Scenario: No project directory configured

- **WHEN** a job with the read-file tool runs while no project directory is configured
- **THEN** the tool can read only files in the job's own output folder

#### Scenario: Forbidden tools cannot be allowed

- **WHEN** the user edits a job's allow list in Settings
- **THEN** the shell command tool and the job-management tools are not available to tick

#### Scenario: A job does not inherit chat context

- **WHEN** an agent job runs
- **THEN** it starts from a plain system prompt and its own prompt, with no persona, no chat history and no retrieved-memory digest, and nothing it does is written to memory or the chat history

### Requirement: Run limits and failure handling

Runs of one job SHALL NOT overlap; a trigger firing while the job's previous run is still going SHALL be skipped and logged. At most one agent job SHALL run at a time per provider backend and at most two overall, with further due jobs waiting in a queue of at most ten, a due job finding the queue full being skipped and logged as such; reminders SHALL NOT be queued behind agent jobs. A run on the local model backend SHALL wait while a chat reply is being generated on that same backend and SHALL otherwise run in parallel with chat. Every agent run SHALL stop at a time limit (default ten minutes, adjustable per job) and at a cap on model turns, recording partial output and a timed-out status. A failed run SHALL NOT be retried; the next trigger is the retry. After three consecutive failed runs the job's triggers SHALL be disabled and the user notified.

#### Scenario: A job does not overlap itself

- **WHEN** a job's trigger fires while its previous run is still running
- **THEN** the new run is skipped and logged as still running

#### Scenario: A job yields to chat on the local backend

- **WHEN** a job using the local model is due while a chat reply is being generated on the local model
- **THEN** the job starts after the reply ends

#### Scenario: A job runs in parallel with a cloud chat

- **WHEN** a job using the local model is due while a chat reply is being generated on a cloud profile
- **THEN** the job starts without waiting

#### Scenario: Repeated failures disable the job

- **WHEN** a job fails three runs in a row
- **THEN** its triggers are disabled and the user is notified

### Requirement: Provider profile and cloud disclosure

An agent job SHALL run only with the provider profile fixed at approval and SHALL NOT fall back to another profile. Only profiles whose tool use is controlled by the application's own tool registry (the local model and API-key profiles) SHALL be selectable for an agent job; profiles backed by an external command-line agent, whose tools the application cannot restrict to a registry, SHALL NOT be. A job SHALL be approvable with a cloud profile only if the user has already acknowledged that profile's data disclosure, and the approval SHALL state that the job will send data to that provider on a schedule without the user watching. If the profile is unavailable, or its acknowledgment was withdrawn or superseded, the run SHALL fail with that reason recorded and the user notified, and the job SHALL NOT be approved again automatically. A job requested by an external client SHALL default to the local profile when one is enabled, and otherwise stay pending until the user chooses a profile.

#### Scenario: A command-line agent profile is not offered

- **WHEN** the user chooses a provider profile for an agent job
- **THEN** profiles backed by an external command-line agent are not available

#### Scenario: No fallback to another profile

- **WHEN** a job's local model is not running at fire time
- **THEN** the run fails with "profile unavailable" and no other profile is used

#### Scenario: A withdrawn acknowledgment stops cloud runs

- **WHEN** the disclosure for a job's cloud profile is no longer acknowledged
- **THEN** the run fails with a needs-acknowledgment reason and the user is notified

### Requirement: Run output delivery, storage and retention

A reminder SHALL be shown to the user as a notification through the pet's status bubble and, when the application is in the background, an operating-system notification. An agent job's result SHALL be recorded in its run log and the user notified in the same way that it finished, with its name and status but not its output, which is read in the run log. Run results SHALL NOT be added to the chat history, to memory, or to any conversation summary. Each run record SHALL store time, status, reason, duration, provider profile, the audience (local or cloud) of that profile, the names of the tools called, and the output up to a size limit with truncation marked; it SHALL NOT store tool arguments or tool results. The application SHALL keep at most the last 100 runs per job and nothing older than 90 days, both adjustable, and SHALL provide a control that erases all job data.

#### Scenario: A reminder fires

- **WHEN** a reminder's trigger fires while the application is open
- **THEN** its text is shown through the status bubble, and the chat history is unchanged

#### Scenario: Tool arguments are not logged

- **WHEN** an agent job calls a search tool
- **THEN** the run record shows that the tool was called but not what it was asked or what it returned

#### Scenario: Old runs are pruned

- **WHEN** a job has more than 100 recorded runs
- **THEN** the oldest are removed

### Requirement: Job tools in chat

The model SHALL have tools to create a job with its triggers, list jobs, read one job's status, read a job's run history, read the full output of one run, change a job's trigger, and delete a job. Creating, changing a trigger and deleting SHALL require the user's confirmation. Creation SHALL take a structured schedule rather than free text, and the confirmation SHALL show the readable schedule and the next fire times. Status SHALL include enabled and approved state, profile, triggers with next fire times, last run result and consecutive failures. History SHALL include each run's time, duration, status and reason and a short excerpt of its output; full output SHALL be returned only by the separate output tool and bounded in size. Output returned to the model SHALL be marked as untrusted data. A cloud-profile chat, and any external client, SHALL receive output only for runs whose own profile was cloud, and SHALL receive metadata only for the rest; a local-profile chat SHALL receive all.

#### Scenario: Asking how a job went

- **WHEN** the user asks in chat how their morning job did
- **THEN** the model can read its status and recent runs and answer from them

#### Scenario: Local output is withheld from a cloud chat

- **WHEN** a cloud-profile chat reads the history of a job that ran on the local model
- **THEN** it receives the run times and statuses but not the output

#### Scenario: Job output is marked untrusted

- **WHEN** the model receives a run's output excerpt
- **THEN** the excerpt is wrapped as untrusted data and any markup in it is neutralized
