# Spec: agent-runtime

## Purpose

Defines Aemeath's own agent loop for tool-calling-capable provider profiles: what native tools exist, how permission for each tool call is decided and enforced, and how the user is asked to confirm a tool call when required.

## Requirements

### Requirement: Agent loop for tool-calling providers

For a provider profile that supports tool calling, the application SHALL run an agent loop: present the available native tools, allow the model to request one or more tool calls, execute permitted ones, return their results to the model, and repeat until the model produces a final answer or a limit is reached. For a provider profile that does not support tool calling, chat SHALL behave exactly as it does without an agent loop.

#### Scenario: A tool call result is used in the following reply

- **WHEN** the model requests a tool call and receives its result
- **THEN** the model's next response can incorporate that result before the user sees the final answer

#### Scenario: Iteration is bounded

- **WHEN** the model repeatedly requests tool calls without producing a final answer
- **THEN** the agent loop stops after a fixed maximum number of iterations rather than continuing indefinitely

### Requirement: Native tool registry

The application SHALL provide, for tool-calling-capable provider profiles, at least the following native tools: web search, reading a file, listing a directory, running a shell command, and reading basic system information (current date/time, OS, CPU/memory/uptime). Web search SHALL require no user-supplied credential. File and directory tools SHALL operate only within a user-configured project directory or with explicit per-request permission for a location outside it. Reading system information SHALL NOT include the active window title, user idle time, or clipboard content.

#### Scenario: Web search requires no credential

- **WHEN** the web search tool is used
- **THEN** it does not require the user to have configured any search-service credential

#### Scenario: File tools outside the configured project directory require permission

- **WHEN** the read-file or list-directory tool is asked to access a path outside the configured project directory
- **THEN** it does not access that path without the user's explicit permission for it

#### Scenario: Shell commands cannot run indefinitely

- **WHEN** the run-command tool is used
- **THEN** it is stopped and reported as timed out if it does not complete within a fixed time limit, and its output is bounded in size

### Requirement: Tool permission enforcement

Every native tool call SHALL be subject to a permission decision — automatically allowed, requiring the user's confirmation, or denied — before it executes. The decision for a file-access tool call SHALL depend on whether the requested path is within the configured project directory, not solely on which tool is being called. A denied or unconfirmed tool call SHALL NOT execute, and the model SHALL be informed that it did not run.

#### Scenario: A call requiring confirmation waits for the user

- **WHEN** a tool call requires confirmation
- **THEN** it does not execute until the user has responded, and the conversation is shown as still in progress while it waits

#### Scenario: A denied call is reported back to the model, not silently dropped

- **WHEN** the user denies a tool call
- **THEN** the model receives a result indicating the call was not permitted, rather than no result at all

### Requirement: Batched confirmation for one turn

When a single model turn requests more than one tool call requiring confirmation, the application SHALL present all of them together for the user to decide at once, rather than one at a time.

#### Scenario: Multiple calls in one turn are shown together

- **WHEN** the model's response in a single turn includes more than one tool call requiring confirmation
- **THEN** the user sees all of them together and can respond to each, rather than being interrupted once per call

### Requirement: Session-scoped trust for repeated tool use

The application SHALL allow the user to mark a tool call as trusted for the remainder of the current conversation when responding to a confirmation, so that future calls matching that trust do not require confirmation again for that conversation. This trust SHALL NOT be offered for tools capable of executing arbitrary commands or modifying files. This trust SHALL NOT be persisted beyond the current conversation.

#### Scenario: A remembered file-access choice does not grant unrelated access

- **WHEN** the user marks a specific out-of-project-directory path as trusted for the session
- **THEN** a later request for a different path outside the project directory still requires confirmation

#### Scenario: High-risk tools always require confirmation

- **WHEN** a tool capable of running arbitrary commands is used
- **THEN** the user is never offered a way to skip confirmation for it for the rest of the session
