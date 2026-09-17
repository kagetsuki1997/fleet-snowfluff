# Spec: ai-persona

## Purpose

Defines how the pet's conversational personality is sourced, edited, and recovered from when broken — a bundled default plus a user-editable file, never requiring a restart or in-app editor.

## Requirements

### Requirement: Bundled default persona

The application SHALL ship a default persona bundled into the binary, used whenever no user persona file exists, or the user's persona file fails to parse.

#### Scenario: No user persona file present

- **WHEN** the application runs with no persona file in the user config directory
- **THEN** the bundled default persona is used for chat

### Requirement: User-editable persona file

The active persona SHALL be sourced from a plain-text file in the user's config directory, editable with an external text editor without any in-application editing UI.

#### Scenario: Externally edited file takes effect

- **WHEN** the user edits the persona file with an external editor and saves it
- **THEN** the edited content is used for chat without needing an in-app save action

### Requirement: Fresh reload on every message

The persona file SHALL be re-read at the time each message is sent, not cached across messages, and no application restart SHALL be required for an edit to take effect.

#### Scenario: Edit while application is running

- **WHEN** the user edits and saves the persona file while the application is running, without restarting it
- **THEN** the next message sent uses the edited persona

### Requirement: Graceful fallback on parse failure

A persona file that fails to parse SHALL NOT crash or disable chat; the bundled default persona SHALL be used for that and subsequent messages until the file is fixed, and a warning naming the parse failure SHALL be visible in the AI settings tab.

#### Scenario: Malformed edit

- **WHEN** the user saves a persona file with invalid syntax
- **THEN** chat continues to work using the bundled default persona, and the AI settings tab shows a warning describing the parse failure

#### Scenario: Fixed file recovers automatically

- **WHEN** the user corrects the persona file after a previous parse failure
- **THEN** the next message uses the corrected persona and the warning no longer appears
