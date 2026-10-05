# Spec: task-router

## Purpose

Defines how Aemeath decides which provider profile handles a given chat message — the routing modes available, how a routing decision is made, and how it falls back when the chosen path is unavailable or unsuitable.

## Requirements

### Requirement: Task routing mode

The application SHALL support two task-routing modes: single, where every message is handled by the default profile, and mixed, where an eligible local profile is tried first for a message before falling back to the default profile. The mode SHALL default to single, so upgrading an existing installation does not change existing behavior.

#### Scenario: Single mode matches existing behavior

- **WHEN** task routing mode is single
- **THEN** every message is sent to the default profile, regardless of its content

#### Scenario: Mixed mode is opt-in

- **WHEN** an existing installation is upgraded to a version that supports task routing modes
- **THEN** task routing mode is single until the user explicitly changes it

### Requirement: Local-first classification in mixed mode

In mixed mode, the application SHALL first attempt a message against an eligible local profile with a maintained set of routing guidance made available to it, and SHALL determine from that attempt's own response whether the message should instead be handled by the default profile. This determination SHALL NOT rely on scoring the message's content against a fixed set of keywords, and SHALL NOT require a separate classification request beyond the local profile's own first response to the message. The local profile's attempt MAY use a fixed, read-only subset of the application's native tools; a tool call that would itself require confirmation or is denied, requested before the attempt has shown or executed anything else, SHALL be treated the same as a textual escalation signal. Once the attempt has shown any text or executed any tool, it SHALL run to completion against the local profile; the application SHALL NOT discard or retract anything already shown to the user for that message.

#### Scenario: A response judged straightforward is shown as-is

- **WHEN** the local profile responds to a message without indicating the message should be escalated
- **THEN** that response is shown to the user as the reply, and the default profile is never contacted for that message

#### Scenario: A response judged complex is retried against the default profile

- **WHEN** the local profile's response indicates the message should be escalated
- **THEN** that response is discarded, is not shown to the user, is not recorded in the conversation history, and the same message is sent to the default profile instead

#### Scenario: A tool call completes the response without escalating

- **WHEN** the local profile's attempt uses one of its available read-only tools and produces a final answer without indicating escalation
- **THEN** that answer is shown to the user as the reply, the default profile is never contacted for that message, and the tool calls made are recorded the same way any other tool-calling turn's are

#### Scenario: An early tool call needing confirmation is treated as escalation

- **WHEN** the local profile's first action, before anything else has been shown to the user or executed, is a tool call that would require confirmation or is denied
- **THEN** that attempt is discarded the same way a textual escalation signal is, and the same message is sent to the default profile instead

#### Scenario: A tool call needing confirmation after the attempt has already committed is denied, not escalated

- **WHEN** the local profile attempts a tool call that would require confirmation after it has already shown text or executed a prior tool call for the same message
- **THEN** that specific tool call is reported back to the local profile as not permitted, no confirmation is shown to the user, and the attempt continues or finishes against the local profile rather than being handed to the default profile

#### Scenario: A failure after the attempt has committed is shown as an error, not escalated

- **WHEN** the local profile's attempt fails after it has already shown text or executed a tool call for the same message
- **THEN** the failure is shown to the user as an error for that message, rather than the message being silently retried against the default profile

### Requirement: Fallback to the default profile

In mixed mode, if the local profile is unavailable, not enabled, or fails during a request, the application SHALL fall back to the default profile for that message automatically, the same way an escalated message is retried.

#### Scenario: Local profile unavailable falls back silently

- **WHEN** the local profile is not enabled or its runtime is unavailable
- **THEN** the message is sent to the default profile without the user needing to take any action

#### Scenario: Local profile error mid-response falls back

- **WHEN** the local profile fails partway through responding to a message
- **THEN** the message is retried against the default profile rather than the failure being shown to the user as the final result
