## MODIFIED Requirements

### Requirement: Session persistence

Each conversation session SHALL be persisted as an append-only log, one file per session, organized by the date the session started. A new session file SHALL be created only by an explicit user action, never automatically by closing/reopening the window or by inactivity. Which CLI-backed provider session, if any, is current for a given profile within a conversation SHALL be derivable from that conversation's persisted execution record (see the `execution-log` capability), so that reopening the application resumes those sessions where they are still valid, and so that turns a session missed remain identifiable after a restart.

#### Scenario: Session spans midnight

- **WHEN** a session starts before midnight and continues after
- **THEN** all of its turns remain in the file created at session start, under that start date

#### Scenario: Reopening does not start a new session

- **WHEN** the user closes and later reopens the chat window without an explicit "new chat" action
- **THEN** the previous session continues in the same log file

#### Scenario: CLI session survives an application restart

- **WHEN** the application is restarted and the most recent chat session is resumed, and the next message is sent to a CLI-backed profile that had a valid session in that conversation
- **THEN** the message resumes that CLI session rather than starting a cold one

#### Scenario: Missed turns are still identified after a restart

- **WHEN** the application is restarted, and a CLI session resumed from the previous run had missed turns another provider answered before the restart
- **THEN** those turns are still supplied to the CLI on resume, rather than being treated as already seen

#### Scenario: A new chat does not inherit CLI sessions

- **WHEN** the user starts a new chat session
- **THEN** no CLI session reference from the previous conversation is used for it

### Requirement: Session resume with scrollback

Reopening the chat window SHALL resume the most recently active session and display its full history. The user SHALL also be able to browse past conversations and reopen any of them, switching the chat window to that conversation's history; opening a past conversation SHALL cancel any pending generation first, the same as starting a new chat session already does.

#### Scenario: Reopen shows prior turns

- **WHEN** the user closes the chat window mid-conversation and reopens it later
- **THEN** the prior turns are visible in the reopened window

#### Scenario: A past conversation can be reopened

- **WHEN** the user selects a past conversation from the list of conversations
- **THEN** the chat window switches to that conversation's history

#### Scenario: Reopening a past conversation cancels a pending generation

- **WHEN** a generation is pending in the current conversation and the user reopens a different, past conversation
- **THEN** the pending generation is cancelled before the switch

### Requirement: Background completion, explicit cancellation

Closing the chat window SHALL NOT cancel an in-flight generation; it continues and is still recorded in the session log. Only an explicit stop action, or starting a new chat session, SHALL cancel a pending generation. Cancelling a generation SHALL also terminate any CLI process that is producing it, rather than leaving it running in the background. A cancelled generation, and one that the application was unable to complete recording due to an unexpected exit, SHALL be recorded as such rather than leaving no trace of the attempt.

#### Scenario: Closing does not cancel

- **WHEN** the user closes the chat window while a reply is generating
- **THEN** the reply continues generating and is appended to the session log once complete

#### Scenario: Explicit stop cancels

- **WHEN** the user presses the stop control while a reply is generating
- **THEN** generation is cancelled and no further content is appended for that turn

#### Scenario: Stopping ends the CLI process

- **WHEN** the user stops a generation that is being produced by a CLI-backed profile
- **THEN** the underlying CLI process is terminated and does not continue running after the stop

#### Scenario: A stopped generation is recorded as cancelled

- **WHEN** the user stops a generation
- **THEN** that attempt is recorded as cancelled, not as if it had never been attempted

## ADDED Requirements

### Requirement: Deleting a conversation

The user SHALL be able to delete a past conversation, removing its persisted transcript and execution record together. Deleting the conversation that is currently open in the chat window SHALL be refused; the user SHALL switch to a different conversation or start a new one first. Deleting a conversation SHALL NOT attempt to remove or modify any session state owned by an external CLI runtime (such as Claude Code's or Codex's own session store) — only Aemeath's own reference to having used such a session is removed.

#### Scenario: A past conversation can be deleted

- **WHEN** the user deletes a conversation that is not the one currently open
- **THEN** that conversation's transcript and execution record are both removed, and it no longer appears in the conversation list

#### Scenario: The currently open conversation cannot be deleted

- **WHEN** the user attempts to delete the conversation that is currently open in the chat window
- **THEN** the deletion is refused

#### Scenario: Deleting a conversation does not reach into an external runtime's own session store

- **WHEN** a deleted conversation had referenced a CLI-backed provider's session
- **THEN** that CLI's own session store is left untouched; only Aemeath's reference to it is removed
