## MODIFIED Requirements

### Requirement: Settings window

The application SHALL provide a settings window (webview) with four tabs — personalization, AI, update, and about — openable from the tray menu and the quick menu; only one settings window SHALL exist at a time.

#### Scenario: Single instance

- **WHEN** the settings window is open and the user selects settings from the tray again
- **THEN** the existing window is focused rather than a second one opening

## ADDED Requirements

### Requirement: AI tab

The AI tab SHALL expose: the master AI-enabled switch, the active-provider selector, per-provider settings (credentials, endpoint where applicable, live-fetched model selection) for OpenAI, Anthropic, Ollama, and Mock, and any persona-load warning per the ai-persona capability. Every control SHALL reflect current state on open and apply its change immediately, consistent with the personalization tab's existing live-apply behavior.

#### Scenario: AI tab reflects current state

- **WHEN** the AI tab is opened after a provider was previously configured
- **THEN** the previously configured provider, its model, and the master switch's state are shown as currently set

### Requirement: Cloud provider disclosure in AI tab

Selecting OpenAI or Anthropic as the active provider for the first time SHALL show the data-disclosure required by the ai-provider capability before the selection takes effect; selecting Ollama or Mock SHALL show no such disclosure.

#### Scenario: Disclosure blocks activation until acknowledged

- **WHEN** the user selects Anthropic as the active provider for the first time and dismisses the disclosure without acknowledging it
- **THEN** Anthropic does not become the active provider
