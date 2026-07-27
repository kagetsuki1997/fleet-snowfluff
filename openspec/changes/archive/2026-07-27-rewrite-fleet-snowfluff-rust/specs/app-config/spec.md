# Spec: app-config

## ADDED Requirements

### Requirement: Typed config with validation

Configuration SHALL be a typed schema persisted as JSON at the platform config directory derived from the app identifier (`fleet-snowfluff/config.json`). Out-of-range or malformed values SHALL be replaced by their defaults at load (mirroring legacy sanitization); a missing or unreadable file yields full defaults.

#### Scenario: Corrupt file recovers to defaults

- **WHEN** the config file contains invalid JSON
- **THEN** the application starts with default settings and rewrites a valid file on next save

#### Scenario: Out-of-range value clamped

- **WHEN** the config file contains an instance count of 999
- **THEN** the loaded config uses the default instance count

### Requirement: One-shot migration from Ameath

On startup with no Fleet Snowfluff config present, the application SHALL attempt to read the legacy `%APPDATA%/ameath_config.json` (Windows); if found, it SHALL carry over all settings except `music_enabled`/`music_volume`, add `ui_language` (from locale detection) and `voice_language` (zh), remove the legacy `DesktopPet` autostart registry value, register the new autostart if autostart is enabled, and write the new config. The legacy file SHALL be left unmodified. Migration SHALL NOT run again once a new config exists.

#### Scenario: Settings survive migration

- **WHEN** a user with legacy scale, transparency, and click-through settings launches Fleet Snowfluff for the first time
- **THEN** those values appear in the new config and the pets honor them; music keys are absent

#### Scenario: No legacy file

- **WHEN** no legacy config exists on first launch
- **THEN** the application starts with defaults and detection-based languages, without error

### Requirement: Per-OS autostart

The application SHALL support launch-at-login on all three platforms via the platform-appropriate mechanism, toggleable in settings and persisted in config.

#### Scenario: Autostart toggle

- **WHEN** the user enables autostart in settings
- **THEN** the OS-level autostart entry exists; disabling removes it
