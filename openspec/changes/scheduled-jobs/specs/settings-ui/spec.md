## ADDED Requirements

### Requirement: Jobs tab

The Settings window SHALL provide a Jobs tab, localized in every supported language, that lists pending requests from external clients above the user's jobs; shows for each job its kind, profile, readable triggers with next fire time and last result; lets the user create, edit, duplicate, enable, disable and delete jobs and edit their triggers through the friendly schedule form or a raw cron field; lets the user edit a job's tool allow list with read-only tools ticked by default and tools that change things clearly marked; shows each job's run log; and holds the retention settings, the erase-all control and the controls for the jobs MCP server. Editing a job's prompt, profile or allow list SHALL make saving an explicit approval and SHALL say what the job will be allowed to do.

#### Scenario: Reviewing a pending request

- **WHEN** an external client has requested a job
- **THEN** the tab shows who asked, what it would run and its proposed tools, with approve, edit-then-approve and reject actions

#### Scenario: Ticking a tool that changes things

- **WHEN** the user ticks a tool that can change files or data
- **THEN** it is visibly marked as able to change things, and the job's detail view lists it

#### Scenario: Reading the run log

- **WHEN** the user opens a job's run log
- **THEN** each run shows time, duration, status, reason and output, with truncation marked
