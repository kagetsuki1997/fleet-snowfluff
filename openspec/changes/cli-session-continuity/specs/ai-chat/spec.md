## MODIFIED Requirements

### Requirement: Session persistence

Each conversation session SHALL be persisted as an append-only log, one file per session, organized by the date the session started. A new session file SHALL be created only by an explicit user action, never automatically by closing/reopening the window or by inactivity. The references to any CLI-backed provider sessions used within a conversation SHALL be persisted alongside that conversation's log, with the same lifetime as the log, so that reopening the application resumes those sessions where they are still valid. The record SHALL include how much of the conversation each session is known to have seen, so that turns it missed remain identifiable after a restart.

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

### Requirement: Background completion, explicit cancellation

Closing the chat window SHALL NOT cancel an in-flight generation; it continues and is still recorded in the session log. Only an explicit stop action, or starting a new chat session, SHALL cancel a pending generation. Cancelling a generation SHALL also terminate any CLI process that is producing it, rather than leaving it running in the background.

#### Scenario: Closing does not cancel

- **WHEN** the user closes the chat window while a reply is generating
- **THEN** the reply continues generating and is appended to the session log once complete

#### Scenario: Explicit stop cancels

- **WHEN** the user presses the stop control while a reply is generating
- **THEN** generation is cancelled and no further content is appended for that turn

#### Scenario: Stopping ends the CLI process

- **WHEN** the user stops a generation that is being produced by a CLI-backed profile
- **THEN** the underlying CLI process is terminated and does not continue running after the stop
