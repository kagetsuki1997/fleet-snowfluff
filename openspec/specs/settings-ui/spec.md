# Spec: settings-ui

## Purpose

TBD

## Requirements

### Requirement: Settings window

The application SHALL provide a settings window (webview) with four tabs — personalization, AI, update, and about — openable from the tray menu and the quick menu; only one settings window SHALL exist at a time.

#### Scenario: Single instance

- **WHEN** the settings window is open and the user selects settings from the tray again
- **THEN** the existing window is focused rather than a second one opening

### Requirement: Personalization controls

The personalization tab SHALL expose: scale step, opacity step, display priority mode, wander stay mode, monitor selection (all screens or a specific monitor), window snap, instance count, autostart, UI language, voice on/off, voice volume (0–150), and voice language. Every control SHALL reflect current state on open and apply its change immediately to running pets and to config. The instance-count control SHALL show a hint warning that setting it too high can crash or freeze the app (including the tray and settings menu), naming the platform-specific `config.json` path so a user who hits it can manually recover by editing the value back down.

#### Scenario: Live apply

- **WHEN** the user moves the voice volume slider to 80
- **THEN** the next voice clip plays at 80% and the value persists across restart

#### Scenario: Instance-count hint shows the recovery path

- **WHEN** the personalization tab renders
- **THEN** the hint under the instance-count control names the actual `config.json` path for the current platform

### Requirement: About tab attribution

The about tab SHALL display the app version, credits for the original Ameath project and `-fugu-` (with link), the Rust rewrite authorship, license notice, and the statement that all GIF and voice asset copyrights belong to Wuthering Waves / Kuro Games and will be removed promptly upon infringement complaint.

#### Scenario: Attribution visible

- **WHEN** the user opens the about tab in any UI language
- **THEN** the credits and the Kuro Games asset disclaimer are shown

### Requirement: Update tab

The update tab SHALL offer manual check-for-update, show current and latest versions, offer install when an update exists, and expose the skip-this-version and skip-all-updates options.

#### Scenario: Manual check finds update

- **WHEN** the user clicks check-for-update and a newer release exists
- **THEN** the tab shows the new version with an install action and a skip-this-version action

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

### Requirement: Settings window opacity

The settings window's overall opacity SHALL follow the personalization tab's existing opacity setting, the same value applied to pet windows, applied immediately when changed while the settings window is open and applied on open to whatever value is currently configured.

#### Scenario: Opacity applies live

- **WHEN** the settings window is open and the user changes the opacity slider on the personalization tab
- **THEN** the settings window's own transparency updates immediately, not only the pets'

#### Scenario: Opacity applies on open

- **WHEN** the user opens the settings window while a non-default opacity is already configured
- **THEN** the settings window opens already at that opacity, not fully opaque until the next change
