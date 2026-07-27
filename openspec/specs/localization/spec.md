# Spec: localization

## Purpose

TBD

## Requirements

### Requirement: Five UI locales from single-source JSON

All user-facing UI strings (tray menu, quick menu, settings window, update dialogs, notifications) SHALL be provided in Traditional Chinese (zh-Hant), Simplified Chinese (zh-Hans), English (en), Japanese (ja), and Korean (ko), stored as one flat JSON dictionary per locale that serves as the single source of truth for both the Rust side and the webview UI. No user-facing string SHALL be hard-coded in code.

#### Scenario: Same locale everywhere

- **WHEN** the active UI language is Japanese
- **THEN** the tray menu, quick menu, and settings window all display Japanese strings from the ja dictionary

### Requirement: System locale detection with zh-Hant fallback

On first run (no override configured) the application SHALL resolve the UI language from the system locale: zh-TW/zh-HK/zh-MO map to zh-Hant; zh-CN/zh-SG map to zh-Hans; en, ja, and ko match by language prefix; any other locale falls back to zh-Hant.

#### Scenario: Unmapped locale falls back

- **WHEN** the system locale is fr-FR and no UI language override is set
- **THEN** the UI displays in zh-Hant

### Requirement: User language override

The settings window SHALL offer an explicit UI-language selection that overrides detection, persists in config, and applies without restart (or with an immediate, automatic UI refresh).

#### Scenario: Override beats system locale

- **WHEN** the system locale is en-US and the user selects Korean in settings
- **THEN** the UI displays in Korean on this and every subsequent launch

### Requirement: Placeholder interpolation

Dynamic strings SHALL use named `{placeholder}` tokens substituted at render time; translations SHALL NOT be built by concatenating string fragments.

#### Scenario: Version string interpolation

- **WHEN** the update tab shows the latest version 0.2.0
- **THEN** the display string is produced by substituting `{version}` in the locale template
