## MODIFIED Requirements

### Requirement: Native tool registry

The application SHALL provide, for tool-calling-capable provider profiles, at least the following native tools: web search, reading a file, listing a directory, running a shell command, and reading basic system information (current date/time, OS, CPU/memory/uptime). Web search SHALL require no user-supplied credential. File and directory tools SHALL operate only within a user-configured project directory or with explicit per-request permission for a location outside it. The read-file tool SHALL bound the size of the content it returns and SHALL indicate when content was cut off. Reading system information SHALL NOT include the active window title, user idle time, or clipboard content.

#### Scenario: Web search requires no credential

- **WHEN** the web search tool is used
- **THEN** it does not require the user to have configured any search-service credential

#### Scenario: File tools outside the configured project directory require permission

- **WHEN** the read-file or list-directory tool is asked to access a path outside the configured project directory
- **THEN** it does not access that path without the user's explicit permission for it

#### Scenario: Shell commands cannot run indefinitely

- **WHEN** the run-command tool is used
- **THEN** it is stopped and reported as timed out if it does not complete within a fixed time limit, and its output is bounded in size

#### Scenario: A large file is truncated

- **WHEN** the read-file tool is asked to read a file larger than its size bound
- **THEN** it returns only the first portion up to the bound, followed by a marker stating the content was truncated, rather than the whole file

#### Scenario: A file within the bound is returned unchanged

- **WHEN** the read-file tool is asked to read a file no larger than its size bound
- **THEN** it returns the complete content with no truncation marker
