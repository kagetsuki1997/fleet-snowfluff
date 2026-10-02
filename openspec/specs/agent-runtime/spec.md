# Spec: agent-runtime

## Purpose

Defines Aemeath's own agent loop for tool-calling-capable provider profiles: what native tools exist, how permission for each tool call is decided and enforced, and how the user is asked to confirm a tool call when required.

## Requirements

### Requirement: Agent loop for tool-calling providers

For a provider profile that supports tool calling, the application SHALL run an agent loop: present the available native tools, allow the model to request one or more tool calls, execute permitted ones, return their results to the model, and repeat until the model produces a final answer or a limit is reached. For a provider profile that does not support tool calling, chat SHALL behave exactly as it does without an agent loop. The outcome of every tool call attempted during the loop SHALL remain available to the caller once the loop ends, not only the model's final reply text (see the `execution-log` capability for how that outcome is persisted and reused). When a single model turn requests more than one tool call permitted to run automatically, the application SHALL execute them concurrently rather than one at a time, bounded to a fixed maximum number in flight at once; each tool's result SHALL still be returned to the model matched to the call it answers, regardless of which call completes first.

#### Scenario: A tool call result is used in the following reply

- **WHEN** the model requests a tool call and receives its result
- **THEN** the model's next response can incorporate that result before the user sees the final answer

#### Scenario: Iteration is bounded

- **WHEN** the model repeatedly requests tool calls without producing a final answer
- **THEN** the agent loop stops after a fixed maximum number of iterations rather than continuing indefinitely

#### Scenario: Tool call outcomes are available after the loop ends

- **WHEN** the agent loop produces a final answer after making one or more tool calls
- **THEN** the caller can determine which tools were called and whether each succeeded, failed, or was not permitted to run, without needing anything beyond what the loop itself returns

#### Scenario: Concurrent calls within one turn are each matched to their own result

- **WHEN** the model requests multiple tool calls permitted to run automatically within one turn
- **THEN** each call's result is paired with that same call once it completes, regardless of the order in which the calls finish

#### Scenario: Concurrency is bounded, not unlimited

- **WHEN** the model requests more automatically-permitted tool calls in one turn than the fixed concurrency limit
- **THEN** the excess calls still execute, starting as earlier ones finish, rather than being rejected or dropped

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

### Requirement: Tool permission enforcement

Every native tool call SHALL be subject to a permission decision — automatically allowed, requiring the user's confirmation, or denied — before it executes. The decision for a file-access tool call SHALL depend on whether the requested path is within the configured project directory, not solely on which tool is being called. A denied or unconfirmed tool call SHALL NOT execute, and the model SHALL be informed that it did not run.

#### Scenario: A call requiring confirmation waits for the user

- **WHEN** a tool call requires confirmation
- **THEN** it does not execute until the user has responded, and the conversation is shown as still in progress while it waits

#### Scenario: A denied call is reported back to the model, not silently dropped

- **WHEN** the user denies a tool call
- **THEN** the model receives a result indicating the call was not permitted, rather than no result at all

### Requirement: Batched confirmation for one turn

When a single model turn requests more than one tool call requiring confirmation, the application SHALL present all of them together for the user to decide at once, rather than one at a time. If a new batch of tool calls requiring confirmation arises while an earlier batch is still awaiting the user's decision, the application SHALL queue the new batch rather than discarding the earlier one; every batch SHALL eventually be presented to the user, in the order it arose.

#### Scenario: Multiple calls in one turn are shown together

- **WHEN** the model's response in a single turn includes more than one tool call requiring confirmation
- **THEN** the user sees all of them together and can respond to each, rather than being interrupted once per call

#### Scenario: A second batch does not discard the first

- **WHEN** a new confirmation-requiring batch arises while an earlier batch is still unresolved
- **THEN** the earlier batch remains intact and is still presented to the user, rather than being silently treated as denied

### Requirement: Bounded sub-task delegation

The application SHALL offer a delegation capability to every tool-calling-capable provider profile, letting the model hand off a described sub-task to a new, independent agent loop and receive that sub-task's final answer as the result. A delegated sub-task SHALL NOT itself be offered the delegation capability, so delegation SHALL NOT recurse beyond one level. A delegated sub-task's own tool calls SHALL be subject to the exact same permission decisions the delegating turn's own tool calls would receive, never more permissive. A delegated sub-task's intermediate activity — its own narration and tool calls — SHALL NOT be shown to the user while it runs; only the delegation call's own failure or final result SHALL be observable, exactly as for any other tool call.

#### Scenario: A delegated sub-task's result is returned to the model

- **WHEN** the model delegates a sub-task and it completes
- **THEN** the model receives that sub-task's final answer as the result of the delegation call, before producing its own reply to the user

#### Scenario: Delegation cannot recurse

- **WHEN** a delegated sub-task itself attempts to delegate a further sub-task
- **THEN** no delegation capability is available to it, and the attempt is rejected the same way a call to an unknown tool is

#### Scenario: A delegated sub-task is invisible while in progress

- **WHEN** a sub-task is delegated
- **THEN** neither its own narration nor its own tool calls are shown to the user while it runs

#### Scenario: A delegated sub-task cannot exceed its parent's own permissions

- **WHEN** a delegated sub-task calls a native tool
- **THEN** that call is subject to the exact same permission decision the delegating turn's own equivalent call would receive

### Requirement: Session-scoped trust for repeated tool use

The application SHALL allow the user to mark a tool call as trusted for the remainder of the current conversation when responding to a confirmation, so that future calls matching that trust do not require confirmation again for that conversation. This trust SHALL NOT be offered for tools capable of executing arbitrary commands or modifying files. This trust SHALL NOT be persisted beyond the current conversation.

#### Scenario: A remembered file-access choice does not grant unrelated access

- **WHEN** the user marks a specific out-of-project-directory path as trusted for the session
- **THEN** a later request for a different path outside the project directory still requires confirmation

#### Scenario: High-risk tools always require confirmation

- **WHEN** a tool capable of running arbitrary commands is used
- **THEN** the user is never offered a way to skip confirmation for it for the rest of the session
