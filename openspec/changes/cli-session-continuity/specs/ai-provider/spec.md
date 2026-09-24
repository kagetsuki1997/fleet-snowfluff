## MODIFIED Requirements

### Requirement: Session continuity for CLI-backed subscription providers

Where a subscription-auth provider's underlying CLI supports resuming a prior session, the application SHALL reuse that session across consecutive messages within the same chat session rather than starting a new one for every message, to avoid repeatedly resending full conversation context. If a resume attempt fails (the underlying session is no longer valid), the application SHALL fall back to starting a fresh session automatically rather than failing the request, and that fresh session SHALL be seeded with the recent conversation history as described in "Fresh CLI sessions are seeded from the conversation transcript". A stored session reference SHALL be reused only when it was created under the same working directory the CLI is about to be run in.

#### Scenario: Consecutive messages reuse the underlying session

- **WHEN** the user sends a second chat message in the same session, using a subscription profile whose CLI supports session resumption
- **THEN** the request reuses the CLI session established by the first message rather than starting a new one

#### Scenario: A stale session falls back to a fresh one automatically

- **WHEN** a resume attempt fails because the underlying CLI session is no longer valid
- **THEN** the application starts a fresh session for that message automatically, without surfacing this as a failure to the user, and the new session is given the recent conversation history so the reply is not produced without prior context

#### Scenario: A session created under a different working directory is not resumed

- **WHEN** a stored session reference was created under a working directory that differs from the one the CLI is about to be run in (for example because the project directory setting changed)
- **THEN** the application does not attempt to resume it and starts a fresh, history-seeded session instead

## ADDED Requirements

### Requirement: Fresh CLI sessions are seeded from the conversation transcript

Whenever a CLI-backed subscription provider starts a request without a usable resumed session — the first message with that provider, a fallback after a failed resume, or a message escalated from another provider in the same chat session — the request SHALL include a compact rendering of the recent conversation history already recorded in the chat session, so that the CLI's reply can take earlier turns into account. When the CLI session is successfully resumed, the application SHALL NOT resend that history, since the CLI already holds it. When there is no prior history, nothing SHALL be added.

#### Scenario: Escalation from a local provider carries earlier turns

- **WHEN** earlier turns of the chat session were answered by a different provider and a later message is escalated to a CLI-backed profile with no existing session
- **THEN** the CLI request includes the recent earlier turns as context

#### Scenario: A resumed session is not sent history again

- **WHEN** a CLI session is successfully resumed for the next message
- **THEN** the request contains only the new message, not a re-rendered history

#### Scenario: A brand-new conversation adds no history

- **WHEN** the first message of a new chat session is sent to a CLI-backed profile
- **THEN** no history section is added to the request

### Requirement: CLI working directory

A CLI-backed subscription provider SHALL be run with an explicit working directory: the configured project directory when one is set, otherwise a fixed directory owned by the application. It SHALL NOT inherit the directory from which the application happened to be launched. The working directory is not an access boundary; which of the CLI's native tools may run remains governed by the native-tool access control setting.

#### Scenario: Project directory configured

- **WHEN** a project directory is configured and a message is sent to a CLI-backed profile
- **THEN** the CLI is run with that directory as its working directory

#### Scenario: No project directory configured

- **WHEN** no project directory is configured and a message is sent to a CLI-backed profile
- **THEN** the CLI is run with the application's own fixed directory as its working directory, regardless of where the application was launched from
