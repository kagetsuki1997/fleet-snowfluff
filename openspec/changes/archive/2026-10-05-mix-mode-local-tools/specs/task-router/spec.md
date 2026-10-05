## MODIFIED Requirements

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
