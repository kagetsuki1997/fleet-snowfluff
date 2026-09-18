## MODIFIED Requirements

### Requirement: AI tab

The AI tab SHALL expose: the master AI-enabled switch, a list of enabled provider profiles (each a provider brand plus an auth method) with controls to enable/disable a profile and choose which one is the default, per-profile settings (credentials for API-key auth, endpoint where applicable, live-fetched model selection) for OpenAI, Anthropic, Ollama, and Mock, connection status for each enabled profile, and any persona-load warning per the ai-persona capability. Every control SHALL reflect current state on open and apply its change immediately, consistent with the personalization tab's existing live-apply behavior.

#### Scenario: AI tab reflects current state

- **WHEN** the AI tab is opened after a profile was previously configured and set as default
- **THEN** that profile, its auth method, its model, and the master switch's state are shown as currently set

#### Scenario: Multiple profiles can be enabled at once

- **WHEN** the user has enabled both an Ollama profile and an Anthropic-via-subscription profile
- **THEN** both appear in the AI tab as enabled, with the currently-default one visibly distinguished from the other

### Requirement: Cloud provider disclosure in AI tab

Enabling a (provider, auth method) pair involving OpenAI or Anthropic for the first time SHALL show the data-disclosure required by the ai-provider capability, worded for that specific auth method, before the profile becomes usable; enabling Ollama or Mock SHALL show no such disclosure.

#### Scenario: Disclosure blocks activation until acknowledged

- **WHEN** the user enables Anthropic via subscription for the first time and dismisses the disclosure without acknowledging it
- **THEN** that profile does not become usable

## ADDED Requirements

### Requirement: Provider status display

For each enabled profile, the AI tab SHALL show its current connection status using distinct wording for at least: not configured, configured but disclosure pending, connected, runtime unavailable (CLI missing), not logged in / subscription expired, and quota exhausted. Status SHALL be checked fresh when the AI tab is opened, not read from a value cached from a previous session.

#### Scenario: CLI missing is distinguishable from not logged in

- **WHEN** a subscription profile's CLI binary is not found on the system
- **THEN** the AI tab shows a runtime-unavailable state naming the missing CLI, distinct from the state shown when the CLI is present but not authenticated

### Requirement: Experimental provider indicator

The AI tab SHALL visually mark a profile using an experimental (per the ai-provider capability's "Experimental provider marking" requirement) provider implementation, distinctly from a verified one, wherever that profile is shown.

#### Scenario: Experimental label visible at selection time

- **WHEN** the user is choosing an auth method for OpenAI
- **THEN** the subscription option is shown labeled as experimental before it is enabled, not only after
