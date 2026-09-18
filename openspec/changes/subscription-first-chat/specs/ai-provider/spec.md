## MODIFIED Requirements

### Requirement: Provider abstraction

The application SHALL support chat completions through a common provider interface with at least four selectable provider brands: an OpenAI-compatible cloud provider, an Anthropic (Claude) cloud provider, a local Ollama provider, and an offline Mock provider that echoes the input without any network access. For OpenAI and Anthropic, each brand SHALL support two auth methods that select between distinct implementations behind the same interface: API key (direct HTTP request signed with a user-supplied key) and subscription (reusing the credential/session already established by that provider's own official CLI). Ollama and Mock have no auth method beyond their existing single mode.

#### Scenario: Switching providers requires no code change

- **WHEN** a new OpenAI-compatible-style endpoint needs to be supported
- **THEN** it is usable by configuring the existing OpenAI-compatible provider with a different endpoint, without adding a new implementation

#### Scenario: Same brand, different auth method, same interface

- **WHEN** a profile's auth method is switched from API key to subscription for the same provider brand
- **THEN** chat continues to work through the same `AiProvider` interface, with no change visible to callers other than which credential source is used

### Requirement: Live model listing

For each provider brand using API-key or local auth (OpenAI, Anthropic, Ollama), the set of selectable models SHALL be fetched live from that provider rather than hardcoded, and a failure to fetch SHALL be shown as a configuration error at the point of selection. For a subscription auth method backed by a CLI with no live model-listing capability of its own, the set of selectable models SHALL instead be a fixed set of that CLI's own documented model identifiers/aliases (not an arbitrary guess at values the CLI's own documentation doesn't confirm), and leaving the model unset SHALL be valid and SHALL mean "use that CLI's own default."

#### Scenario: Invalid credentials surface at model-selection time

- **WHEN** the user enters an invalid API key and opens the model selector for that provider
- **THEN** the model list fails to load and the failure reason is shown inline, without requiring a chat message to be sent first

#### Scenario: A CLI-backed subscription profile still offers a model choice

- **WHEN** the user opens the model selector for a subscription auth method backed by a CLI with no live listing capability
- **THEN** the selector shows that CLI's own documented model identifiers/aliases rather than an empty list, and leaving no model selected is a valid choice

### Requirement: Streaming responses

Chat responses SHALL be delivered to the requesting surface incrementally, for every provider including Mock. For a provider whose underlying transport genuinely generates and delivers text incrementally, chunks SHALL reflect real generation progress. For a provider whose underlying transport only ever delivers one complete response with no incremental delivery of its own, the application SHALL still deliver it to the requesting surface as a paced sequence of chunks rather than a single block, so the user-visible behavior stays consistent across providers even though the underlying generation was not observed incrementally.

#### Scenario: Partial text visible before completion

- **WHEN** a provider is generating a multi-sentence reply
- **THEN** earlier portions of the reply are visible before the full reply has finished generating

#### Scenario: A provider with no incremental transport still paces its output

- **WHEN** a provider's underlying transport delivers only one complete response with no incremental events of its own
- **THEN** the requesting surface still receives that response as multiple chunks over time, not as a single instantaneous block

### Requirement: No provider configured by default

On first run, no provider profile SHALL be enabled, including no default selection of a cloud provider or auth method.

#### Scenario: Fresh install

- **WHEN** the application starts for the first time with no prior configuration
- **THEN** attempting to use chat shows that no provider is configured, and no request is sent to any provider

### Requirement: Cloud provider data disclosure

The first time a (provider, auth method) pair involving a cloud provider (OpenAI or Anthropic, via either API key or subscription) is enabled, the settings UI SHALL show a one-time disclosure describing what will happen for that specific auth method — naming the provider and, for subscription auth, that the CLI's already-logged-in account will be used to send messages, not a newly entered API key — and that pair SHALL NOT become usable until the disclosure is acknowledged. This acknowledgment SHALL be persisted per (provider, auth method) pair, so enabling a different auth method for a provider whose other auth method was already acknowledged SHALL show its own disclosure. Selecting Ollama or Mock SHALL require no such disclosure.

#### Scenario: First cloud selection

- **WHEN** the user enables OpenAI via API key for the first time
- **THEN** a disclosure naming OpenAI and describing API-key usage is shown, and that profile does not become usable until acknowledged

#### Scenario: Disclosure does not repeat once acknowledged

- **WHEN** the user has previously acknowledged OpenAI via API key's disclosure, and later re-enables OpenAI via API key after disabling and re-enabling it
- **THEN** no disclosure is shown for that (provider, auth method) pair the second time

#### Scenario: Switching auth method re-triggers disclosure

- **WHEN** the user has acknowledged Anthropic via API key's disclosure, then enables Anthropic via subscription for the first time
- **THEN** a separate disclosure describing subscription usage is shown, even though Anthropic via API key was already acknowledged

#### Scenario: Local provider needs no disclosure

- **WHEN** the user selects Ollama as a provider
- **THEN** no data-disclosure prompt is shown

### Requirement: Independent per-provider configuration

Settings (credentials or auth method, endpoint, selected model) for every enabled provider profile SHALL be stored independently and simultaneously, with a single default-profile pointer indicating which one currently handles chat requests. Enabling an additional profile or changing the default SHALL NOT require re-entering a previously configured profile's settings.

#### Scenario: Switching back to a previously configured provider

- **WHEN** the user enables OpenAI via API key, enables Ollama, sets Ollama as the default, then sets OpenAI via API key back as the default
- **THEN** OpenAI's previously entered settings are still present and usable without re-entry

#### Scenario: Existing single-provider config migrates to one profile

- **WHEN** a Stage 1 install with `active_provider: Some(Anthropic)` and previously configured Anthropic settings starts for the first time after this change
- **THEN** it has exactly one enabled profile (Anthropic, API key auth, its previous model and disclosure state carried over) set as the default profile, with no re-entry or re-disclosure required

## ADDED Requirements

### Requirement: Subscription auth via the provider's own CLI

For a provider profile using subscription auth, the application SHALL detect and use an already-authenticated installation of that provider's official CLI (`claude` for Anthropic, `codex` for OpenAI) rather than implementing its own OAuth flow. The application SHALL NOT require the user to enter an OAuth token manually.

#### Scenario: CLI already logged in

- **WHEN** the user enables Anthropic via subscription and the `claude` CLI is installed and already authenticated
- **THEN** chat requests succeed using that CLI's credential, with no additional login step shown

#### Scenario: CLI not installed

- **WHEN** the user enables a subscription auth method and the corresponding CLI binary cannot be found
- **THEN** the profile shows a runtime-unavailable state naming the missing CLI, and no chat request is attempted

#### Scenario: CLI installed but not logged in

- **WHEN** the user enables a subscription auth method and the CLI is installed but not authenticated
- **THEN** the application attempts to trigger that CLI's own login flow (which opens a browser); if the login flow cannot be completed this way, the settings UI instead instructs the user to run the CLI's login command themselves and then re-check status

### Requirement: No persisted credential for subscription auth

The application SHALL NOT write any subscription-auth token, session, or login status to disk. Status and credentials for a subscription profile SHALL be re-derived from the provider's CLI at the time they are needed, not cached across requests or app restarts.

#### Scenario: Subscription status is not cached

- **WHEN** the user logs out of the `claude` CLI in a terminal while Fleet is running
- **THEN** the next chat request or settings-tab open reflects the logged-out state, without Fleet needing to be told about the change

### Requirement: Distinct provider/runtime failure states

Provider failures SHALL be distinguishable, at minimum, as: network failure, invalid/rejected credential, rate limited, invalid response, cancelled, runtime unavailable (the provider's CLI is missing or not executable), subscription expired (the CLI reports it is no longer authenticated), and quota exhausted (authenticated, but the current subscription period's usage allowance is used up). Rate limited and quota exhausted SHALL be presented distinctly, since one implies retrying shortly will help and the other does not.

#### Scenario: Quota exhausted is not shown as a transient rate limit

- **WHEN** a subscription profile's usage cap for the current period has been reached
- **THEN** the failure is shown as quota-exhausted, not as a transient rate limit inviting an immediate retry

### Requirement: Experimental provider marking

A provider implementation that has not been manually verified end-to-end SHALL be marked experimental, and that marking SHALL be visible wherever the provider can be selected or its status inspected, not only in documentation.

#### Scenario: Unverified subscription provider is labeled

- **WHEN** the OpenAI subscription auth method is enabled
- **THEN** the settings UI shows it as experimental/unverified, distinctly from a provider that has been verified

### Requirement: Session continuity for CLI-backed subscription providers

Where a subscription-auth provider's underlying CLI supports resuming a prior session, the application SHALL reuse that session across consecutive messages within the same chat session rather than starting a new one for every message, to avoid repeatedly resending full conversation context. If a resume attempt fails (the underlying session is no longer valid), the application SHALL fall back to starting a fresh session automatically rather than failing the request.

#### Scenario: Consecutive messages reuse the underlying session

- **WHEN** the user sends a second chat message in the same session, using a subscription profile whose CLI supports session resumption
- **THEN** the request reuses the CLI session established by the first message rather than starting a new one

#### Scenario: A stale session falls back to a fresh one automatically

- **WHEN** a resume attempt fails because the underlying CLI session is no longer valid
- **THEN** the application starts a fresh session for that message automatically, without surfacing this as a failure to the user beyond the normal loss of that session's conversational memory
