## MODIFIED Requirements

### Requirement: Native tool registry

The application SHALL provide, for tool-calling-capable provider profiles, at least the following native tools: web search, reading a file, listing a directory, running a shell command, and reading basic system information (current date/time, OS, CPU/memory/uptime). Web search SHALL require no user-supplied credential. File and directory tools SHALL operate only within a user-configured project directory or with explicit per-request permission for a location outside it, except that the read-file tool MAY read the application's own directory of spilled tool output without asking. The read-file tool SHALL bound the size of the content it returns and SHALL indicate when content was cut off. Reading system information SHALL NOT include the active window title, user idle time, or clipboard content. The same registry MAY also include tools sourced from a connected MCP server (see the `mcp` capability), each one subject to the exact same permission-tier enforcement as a native tool, with no distinction visible to the agent loop between a tool's native or MCP origin.

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

#### Scenario: An MCP-sourced tool is dispatched the same way a native tool is

- **WHEN** the model calls a tool that was discovered from a connected MCP server rather than implemented natively
- **THEN** the agent loop's own dispatch and permission-tier logic treats it exactly as it would a native tool, requiring no code specific to that tool's origin

#### Scenario: Spilled tool output can be read without a confirmation

- **WHEN** the read-file tool is asked to read a file inside the application's directory of spilled tool output
- **THEN** it reads the file without asking the user for permission, even when no project directory is configured, and a path outside that directory and the project directory still requires permission

## ADDED Requirements

### Requirement: Tool output is bounded within a run

Within a single agent run, the application SHALL bound the size of each tool result added to the conversation, whatever the tool's origin (native or from a connected MCP server). A result over the bound SHALL be cut off with a visible marker saying so, and the rest of the output SHALL be kept in the application's directory of spilled tool output, in pieces each small enough for the read-file tool to return whole, so that all of it can be read. The model SHALL be told the result was shortened and where the rest is.

#### Scenario: A very large tool result is shortened

- **WHEN** a tool returns more output than the bound
- **THEN** the model receives the first portion followed by a marker stating the result was cut off and naming where the rest is kept, not the whole output

#### Scenario: Everything that was cut off can be read back

- **WHEN** a tool returned far more than the bound
- **THEN** the shown portion plus the saved pieces together reproduce the original output exactly, and each saved piece can be read in full with the read-file tool

#### Scenario: A result within the bound is unchanged

- **WHEN** a tool returns output no larger than the bound
- **THEN** the model receives it complete with no marker

#### Scenario: An MCP-sourced result is bounded the same way

- **WHEN** a tool from a connected MCP server returns a very large page of text
- **THEN** it is shortened with the same visible marker as a native tool's result would be
