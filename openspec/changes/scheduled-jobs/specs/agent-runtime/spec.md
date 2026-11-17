## ADDED Requirements

### Requirement: Per-run registry for scheduled jobs

For a scheduled agent job the application SHALL build a tool registry containing only the tools on that job's approved allow list, plus the job's output-folder write tool when it is on the list, and SHALL NOT include job-management tools, shell command execution, delegation, MCP connection, or memory write tools whatever the allow list says. Tools sourced from a connected MCP server SHALL be present only if the user ticked those specific tools for the job. The file read and directory listing tools in that registry SHALL be confined to the project directory and the job's output folder. The registry SHALL NOT change during a run, and it SHALL be built only for profiles whose tool use the application controls through its own registry.

#### Scenario: An MCP tool is available only when ticked

- **WHEN** a connected MCP server offers a tool the job's allow list does not name
- **THEN** the job's run cannot see or call that tool

#### Scenario: The allow list cannot add a forbidden tool

- **WHEN** a job's stored allow list names a tool that is never available to jobs
- **THEN** that tool is absent from the run's registry
