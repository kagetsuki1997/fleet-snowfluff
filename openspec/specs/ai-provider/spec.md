# Spec: ai-provider

## Purpose

Defines the pluggable AI chat-provider layer: which providers exist, how they are configured and switched between, and the guardrails (master switch, disclosure, cost/latency caps) that apply regardless of which provider is active.

## Requirements

### Requirement: Provider abstraction

The application SHALL support chat completions through a common provider interface with at least four selectable implementations in Stage 1: an OpenAI-compatible cloud provider, an Anthropic (Claude) cloud provider, a local Ollama provider, and an offline Mock provider that echoes the input without any network access.

#### Scenario: Switching providers requires no code change

- **WHEN** a new OpenAI-compatible-style endpoint needs to be supported
- **THEN** it is usable by configuring the existing OpenAI-compatible provider with a different endpoint, without adding a new implementation

### Requirement: AI features disabled by default

AI-dependent features (chat window, status bubble) SHALL be gated by a single master switch that defaults to off. Even when a provider is fully configured, no AI-dependent feature SHALL run while the switch is off.

#### Scenario: Configured but disabled

- **WHEN** a provider and its credentials are fully configured but the master switch is off
- **THEN** opening the chat window shows that AI features are disabled rather than sending any request

### Requirement: No provider configured by default

On first run, no provider SHALL be preselected as active, including no default selection of a cloud provider.

#### Scenario: Fresh install

- **WHEN** the application starts for the first time with no prior configuration
- **THEN** attempting to use chat shows that no provider is configured, and no request is sent to any provider

### Requirement: Cloud provider data disclosure

The first time a cloud provider (OpenAI or Anthropic) is selected as active, the settings UI SHALL show a one-time disclosure that messages will be sent to that provider's servers, and the provider SHALL NOT become active until the disclosure is acknowledged. This acknowledgment SHALL be persisted per provider, so switching away and back to an already-acknowledged provider SHALL NOT show the disclosure again. Selecting Ollama or Mock SHALL require no such disclosure.

#### Scenario: First cloud selection

- **WHEN** the user selects OpenAI as the active provider for the first time
- **THEN** a disclosure naming OpenAI is shown, and OpenAI does not become the active provider until acknowledged

#### Scenario: Disclosure does not repeat once acknowledged

- **WHEN** the user has previously acknowledged OpenAI's disclosure, switches to Ollama, then switches back to OpenAI
- **THEN** no disclosure is shown for OpenAI the second time

#### Scenario: Local provider needs no disclosure

- **WHEN** the user selects Ollama as the active provider
- **THEN** no data-disclosure prompt is shown

### Requirement: Independent per-provider configuration

Settings (credentials, endpoint, selected model) for every provider SHALL be stored independently and simultaneously, with a single pointer indicating which one is currently active. Switching the active provider SHALL NOT require re-entering a previously configured provider's settings.

#### Scenario: Switching back to a previously configured provider

- **WHEN** the user configures OpenAI, switches to Ollama, then switches back to OpenAI
- **THEN** OpenAI's previously entered settings are still present and usable without re-entry

### Requirement: Live model listing

For each real provider (OpenAI, Anthropic, Ollama), the set of selectable models SHALL be fetched live from that provider rather than hardcoded, and a failure to fetch SHALL be shown as a configuration error at the point of selection.

#### Scenario: Invalid credentials surface at model-selection time

- **WHEN** the user enters an invalid API key and opens the model selector for that provider
- **THEN** the model list fails to load and the failure reason is shown inline, without requiring a chat message to be sent first

### Requirement: Streaming responses

Chat responses SHALL be delivered to the requesting surface incrementally as they are generated, for every provider including Mock.

#### Scenario: Partial text visible before completion

- **WHEN** a provider is generating a multi-sentence reply
- **THEN** earlier portions of the reply are visible before the full reply has finished generating

### Requirement: Bounded response length

Every request SHALL apply a fixed maximum output length regardless of provider, not configurable through the settings UI.

#### Scenario: Model ignores persona brevity guidance

- **WHEN** a provider would otherwise generate a very long reply
- **THEN** generation stops once the fixed maximum output length is reached

### Requirement: Bounded conversation context

Each request SHALL include only a fixed-size window of the most recent conversation turns as context, not the full session history.

#### Scenario: Long-running session

- **WHEN** a session has accumulated far more turns than the fixed context window
- **THEN** only the most recent turns within that window are sent as context on the next request

### Requirement: Plain-text responses

Provider responses SHALL be treated as plain text in Stage 1; no structured emotion or metadata field is requested or parsed from any provider.

#### Scenario: Response contains no machine-readable emotion tag

- **WHEN** a reply is received from any provider
- **THEN** it is displayed and logged as plain text only

### Requirement: Separate credential storage

Provider credentials (API keys) SHALL be stored separately from non-secret provider settings (endpoint, selected model, active-provider pointer), with restrictive file permissions applied where the operating system supports it.

#### Scenario: Non-secret settings readable without exposing keys

- **WHEN** the non-secret provider configuration file is inspected
- **THEN** it contains no API key material
