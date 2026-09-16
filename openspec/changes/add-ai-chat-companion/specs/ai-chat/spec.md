## Purpose

Defines the two chat surfaces (the global chat window and the per-pet status bubble), how a conversation is opened, persisted, and resumed, and how an in-flight AI request behaves relative to the rest of the running application.

## ADDED Requirements

### Requirement: Global chat window

The application SHALL provide a single chat window, shared across all pet instances regardless of instance count, offering a text input and a scrollable, streamed view of the current session's conversation.

#### Scenario: Multiple instances, one chat window

- **WHEN** the instance count is greater than 1 and the user opens chat from any pet
- **THEN** the same single chat window opens or is focused, not a separate window per pet

### Requirement: Chat opened by double-click

Double-clicking a pet SHALL open the global chat window. This gesture SHALL be distinguished from a single press-and-hold (which starts a drag) and from a right-click (which opens the quick menu).

#### Scenario: Single click still drags

- **WHEN** the user presses and moves the cursor without releasing and re-pressing quickly
- **THEN** the pet is dragged as before, and the chat window does not open

#### Scenario: Double-click opens chat

- **WHEN** the user double-clicks a pet without dragging
- **THEN** the global chat window opens or is focused

### Requirement: Session persistence

Each conversation session SHALL be persisted as an append-only log, one file per session, organized by the date the session started. A new session file SHALL be created only by an explicit user action, never automatically by closing/reopening the window or by inactivity.

#### Scenario: Session spans midnight

- **WHEN** a session starts before midnight and continues after
- **THEN** all of its turns remain in the file created at session start, under that start date

#### Scenario: Reopening does not start a new session

- **WHEN** the user closes and later reopens the chat window without an explicit "new chat" action
- **THEN** the previous session continues in the same log file

### Requirement: Session resume with scrollback

Reopening the chat window SHALL resume the most recently active session and display its full history. Browsing older, already-closed sessions from within the application is not required in Stage 1.

#### Scenario: Reopen shows prior turns

- **WHEN** the user closes the chat window mid-conversation and reopens it later
- **THEN** the prior turns are visible in the reopened window

### Requirement: Pause during chat activity

Every pet instance SHALL be paused (per the pet-behavior pause mechanism) for as long as the chat window is open, or a generation is still pending, whichever is longer. Whatever pause state existed before the chat window was opened SHALL be restored once both conditions clear.

#### Scenario: Manual pause preserved

- **WHEN** the user had manually paused pets before opening the chat window
- **THEN** closing the chat window (with no pending generation) leaves the pets paused

#### Scenario: Pause persists through background generation

- **WHEN** the chat window is closed while a reply is still being generated
- **THEN** pets remain paused until that generation finishes or is cancelled

### Requirement: Single in-flight generation

At most one generation SHALL be in flight at any time, across the whole application. The chat input SHALL be disabled while a generation is pending, including after the window has been closed and reopened.

#### Scenario: Cannot send while pending

- **WHEN** a reply is still being generated and the user reopens the chat window
- **THEN** the input remains disabled until that generation resolves or is cancelled

#### Scenario: New chat cancels a pending generation

- **WHEN** the user starts a new chat session while a generation is still pending
- **THEN** the pending generation is cancelled and the new session starts immediately

### Requirement: Background completion, explicit cancellation

Closing the chat window SHALL NOT cancel an in-flight generation; it continues and is still recorded in the session log. Only an explicit stop action, or starting a new chat session, SHALL cancel a pending generation.

#### Scenario: Closing does not cancel

- **WHEN** the user closes the chat window while a reply is generating
- **THEN** the reply continues generating and is appended to the session log once complete

#### Scenario: Explicit stop cancels

- **WHEN** the user presses the stop control while a reply is generating
- **THEN** generation is cancelled and no further content is appended for that turn

### Requirement: Status bubble

A small status indicator SHALL be anchored to the first pet instance, showing: nothing when idle, `...` while a generation is pending, `Ciallo～(∠・ω< )⌒☆` once a reply completes while the chat window is not focused, or `(×_×)` if the pending generation failed. These are fixed literal glyphs, not localized text. The unread indicators SHALL clear when the chat window regains focus.

#### Scenario: Unread reply while window closed

- **WHEN** a reply finishes generating while the chat window is closed or unfocused
- **THEN** the status bubble shows `Ciallo～(∠・ω< )⌒☆` until the chat window is focused

#### Scenario: Failure indicator distinct from success

- **WHEN** a pending generation fails
- **THEN** the status bubble shows `(×_×)`, visually distinct from the `Ciallo～(∠・ω< )⌒☆` success indicator

### Requirement: Status bubble tracks manual drag

While the status bubble is visible, its on-screen position SHALL follow the first pet instance if that instance is manually dragged.

#### Scenario: Dragging the anchored pet

- **WHEN** the first pet instance is showing the status bubble and the user drags it to a new location
- **THEN** the status bubble moves with it

### Requirement: Inline error handling, no automatic retry

A failed generation SHALL be shown inline in the chat transcript with a human-readable reason, and recorded in the session log as excluded from future conversation context. The application SHALL NOT automatically retry a failed request; the input SHALL be re-enabled for the user to resend manually.

#### Scenario: Failure does not pollute future context

- **WHEN** a request fails and the user later sends a new message in the same session
- **THEN** the failed turn is not included in the context sent for the new request

#### Scenario: No silent retry

- **WHEN** a request fails
- **THEN** the application does not automatically resend it, and the input becomes available again
