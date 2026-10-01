## Purpose

Defines the persisted record of what actually happened during each turn of a conversation — which runtime handled it, what tool calls it made and with what outcome, and how it ended — independent of and in addition to the user-visible chat transcript, so that a later turn (possibly on a different runtime) can be informed by what a prior turn's tools did, and so that the full sequence of attempts within one turn (including ones never shown to the user) is recoverable.

## ADDED Requirements

### Requirement: One Execution record per attempt, uniformly

The application SHALL record one Execution for every attempt to produce a reply, including an attempt that is discarded and never reaches the user-visible chat transcript (a `mix`-mode local attempt that escalates or fails). This SHALL hold regardless of whether the attempt used tools, a CLI-backed runtime, or neither. An Execution record SHALL identify the conversation it belongs to, the provider profile that handled it, and the route taken (a direct attempt, a `mix`-mode local attempt, a fallback after escalation or failure, or a tool-calling attempt).

#### Scenario: A plain chat turn is still recorded

- **WHEN** a message is answered directly by a profile with no tool calls and no CLI session involved
- **THEN** an Execution record exists for that turn

#### Scenario: A discarded local attempt is recorded alongside the fallback that replaced it

- **WHEN** a `mix`-mode local attempt escalates or fails and the same message is retried against the default profile
- **THEN** both the discarded local attempt and the winning fallback each have their own Execution record, even though only the fallback's reply appears in the chat transcript

### Requirement: Execution lifecycle is start-then-end, and an incomplete end means cancelled or crashed

An Execution SHALL be recorded as it begins, before the provider is called, and again when it concludes. If the application records an Execution beginning but is unable to record how it concluded — because the generation was explicitly stopped, or the application exited or crashed mid-turn — the Execution SHALL be considered cancelled, distinguishable on inspection from one that completed or errored normally.

#### Scenario: A started Execution is visible even before it finishes

- **WHEN** a generation is still in progress
- **THEN** its Execution is already recorded as started, before any reply content exists

#### Scenario: Stopping a generation records it as cancelled

- **WHEN** the user stops an in-progress generation
- **THEN** its Execution is recorded as cancelled, not simply left with no outcome recorded at all

### Requirement: Tool call outcomes survive the turn that produced them

For a tool-calling Execution, the application SHALL retain, after the turn concludes, which tools were called and the outcome of each: executed successfully, executed and failed, denied by standing permission policy, declined by the user for that call, or rejected for an unknown tool name or malformed arguments. For a rejected call only, the arguments that caused the rejection SHALL also be retained, bounded in size, since the call never reached execution and the argument itself is the only available diagnostic. Arguments and results for every other outcome SHALL NOT be retained beyond the turn itself.

#### Scenario: A successful tool call's outcome is retained

- **WHEN** a tool-calling turn executes a tool successfully
- **THEN** that tool's name and successful outcome are still available after the turn ends

#### Scenario: Malformed arguments are retained for diagnosis

- **WHEN** a tool call is rejected because its arguments do not match what the tool expects
- **THEN** the rejected arguments are retained, up to a fixed size, alongside the rejection

#### Scenario: A successful tool call's result content is not retained

- **WHEN** a tool call executes successfully and returns its result
- **THEN** only the fact that it succeeded is retained after the turn, not the result content itself

### Requirement: A turn's tool activity informs the next turn's context

When assembling the context for a new turn, the application SHALL include a short note describing the prior turn's tool activity, if the immediately preceding Execution for the same conversation made any tool calls, regardless of which profile or runtime answers the new turn.

#### Scenario: A plain follow-up turn is told what the previous tool-calling turn did

- **WHEN** a tool-calling turn completes and the next message in the same conversation is answered by a profile with no tools of its own
- **THEN** that next turn's context includes a note about what the prior turn's tools did

#### Scenario: No note is added when the prior turn made no tool calls

- **WHEN** the immediately preceding turn made no tool calls
- **THEN** no tool-activity note is added to the next turn's context
