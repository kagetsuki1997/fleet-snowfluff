## MODIFIED Requirements

### Requirement: AI tab

The AI tab SHALL expose: the master AI-enabled switch, a list of enabled provider profiles (each a provider brand plus an auth method) with controls to enable/disable a profile and choose which one is the default, per-profile settings (credentials for API-key auth, endpoint where applicable, live-fetched model selection) for OpenAI, Anthropic, Ollama, and Mock, connection status for each enabled profile, a task-routing mode control (single, or mixed local-then-default routing), a project directory picker used to scope file- and command-related tool access, per-tool native access controls for a subscription profile backed by a CLI with its own per-tool allow/deny list (currently Claude Code), a list of connected MCP servers with a control to add one by filling in its connection details directly and a control to remove any connected one, a Memory section listing the memories the assistant has saved about the user (see the `memory` capability) with controls to review and manage them, and any persona-load warning per the ai-persona capability. Every control SHALL reflect current state on open and apply its change immediately, consistent with the personalization tab's existing live-apply behavior.

#### Scenario: AI tab reflects current state

- **WHEN** the AI tab is opened after a profile was previously configured and set as default
- **THEN** that profile, its auth method, its model, and the master switch's state are shown as currently set

#### Scenario: Multiple profiles can be enabled at once

- **WHEN** the user has enabled both an Ollama profile and an Anthropic-via-subscription profile
- **THEN** both appear in the AI tab as enabled, with the currently-default one visibly distinguished from the other

#### Scenario: Task routing mode is visible and changeable

- **WHEN** the user opens the AI tab
- **THEN** the current task-routing mode (single or mixed) is shown and can be changed, and the change applies starting with the next message sent

#### Scenario: No project directory configured is a valid, visible state

- **WHEN** no project directory has been configured
- **THEN** the AI tab shows this plainly rather than silently assuming a default location

#### Scenario: Native tool access controls are visible for a CLI-backed profile that has them

- **WHEN** a subscription profile backed by a CLI with its own per-tool native access control (currently Claude Code) is enabled
- **THEN** the AI tab shows a control for each of that CLI's native tools, reflecting its current allow/deny state and changeable individually

#### Scenario: Connected MCP servers are listed with their status

- **WHEN** one or more MCP servers have been connected
- **THEN** the AI tab lists each one with its current status (ready to use, or still pending authorization) and a way to remove it

#### Scenario: A connected server's discovered tools can be inspected

- **WHEN** a user expands a connected MCP server's entry in the AI tab
- **THEN** the tools that server exposes (as discovered when it was connected) are shown, with a way to refresh that list on demand

#### Scenario: A server can be added directly from the settings window

- **WHEN** a user fills in an MCP server's connection details directly in the AI tab, instead of asking for it in chat
- **THEN** the same confirmation and connection process applies as it would for a chat-requested connection

#### Scenario: Memories are listed with where they came from

- **WHEN** the user opens the Memory section after memories exist
- **THEN** every active memory is listed with its text and whether the user asked for it or it was derived automatically, and a way to search the list

#### Scenario: A memory can be edited, pinned, marked sensitive, or deleted

- **WHEN** the user edits, pins, marks sensitive, or deletes a memory in the Memory section
- **THEN** the change applies immediately, and a deletion erases the memory entirely

#### Scenario: Superseded memories can be viewed

- **WHEN** a memory was replaced by a correction
- **THEN** the Memory section can show the superseded version as history, distinct from active memories

#### Scenario: The Memory section is available in every supported language

- **WHEN** the application language is English, Traditional Chinese, Simplified Chinese, Japanese, or Korean
- **THEN** the Memory section's labels and messages are shown in that language

#### Scenario: No memories yet is a valid state

- **WHEN** no memory has been saved
- **THEN** the Memory section says so plainly rather than showing an error or an empty control

#### Scenario: Memory can be switched off and erased from the Memory section

- **WHEN** the user opens the Memory section
- **THEN** it offers a switch that turns memory on or off and an action to erase all memories, and a change to the switch applies starting with the next message sent
