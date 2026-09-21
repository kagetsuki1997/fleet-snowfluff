## MODIFIED Requirements

### Requirement: AI tab

The AI tab SHALL expose: the master AI-enabled switch, a list of enabled provider profiles (each a provider brand plus an auth method) with controls to enable/disable a profile and choose which one is the default, per-profile settings (credentials for API-key auth, endpoint where applicable, live-fetched model selection) for OpenAI, Anthropic, Ollama, and Mock, connection status for each enabled profile, a task-routing mode control (single, or mixed local-then-default routing), a project directory picker used to scope file- and command-related tool access, and any persona-load warning per the ai-persona capability. Every control SHALL reflect current state on open and apply its change immediately, consistent with the personalization tab's existing live-apply behavior.

#### Scenario: AI tab reflects current state

- **WHEN** the AI tab is opened after a profile was previously configured and set as default
- **THEN** that profile, its auth method, its model, and the master switch's state are shown as currently set

#### Scenario: Multiple profiles can be enabled at once

- **WHEN** the user has enabled both an Ollama profile and an Anthropic-via-subscription profile
- **THEN** both appear in the AI tab as enabled, with the currently-default one visibly distinguished from the other

#### Scenario: Task routing mode is visible and changeable

- **WHEN** the user opens the AI tab
- **THEN** the current task-routing mode (single or mixed) is shown and can be changed, and the change applies starting with the next message sent

#### Scenario: No project directory configured is a valid, visible state

- **WHEN** no project directory has been configured
- **THEN** the AI tab shows this plainly rather than silently assuming a default location
