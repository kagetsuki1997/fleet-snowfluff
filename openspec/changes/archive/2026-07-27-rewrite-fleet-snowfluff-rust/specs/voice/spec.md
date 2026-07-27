# Spec: voice

## ADDED Requirements

### Requirement: Voice playback on interaction

When voice is enabled, the pet SHALL play a randomly selected voice clip from the active language pack when a drag begins; starting a new clip stops any currently playing clip.

#### Scenario: Voice on drag start

- **WHEN** the user starts dragging a pet with voice enabled
- **THEN** one clip from the active language pack plays

### Requirement: Anti-repeat selection

Random clip selection SHALL NOT play the same clip four or more times consecutively when more than one clip exists in the active pack (up to three consecutive plays of the same clip is allowed; the fourth is what gets avoided, matching legacy's own "no more than three in a row" rule).

#### Scenario: Fourth repeat avoided

- **WHEN** the same clip has just played three times in a row
- **THEN** the next selection is drawn from the other clips

### Requirement: Voice enable toggle and volume

The application SHALL provide a voice on/off toggle and a volume setting from 0 to 150 percent (values above 100 amplify), both applied immediately to all instances and persisted in config.

#### Scenario: Disabling stops playback

- **WHEN** the user disables voice while a clip is playing
- **THEN** playback stops and no further clips play until re-enabled

#### Scenario: Amplified volume

- **WHEN** voice volume is set to 150
- **THEN** clips play amplified to 1.5× their source amplitude

### Requirement: Per-language voice packs

Voice assets SHALL be organized as per-language folders (zh, ja, en, ko), each with a manifest listing its clips; the active pack is selected by the voice-language setting, which is independent of the UI language.

#### Scenario: Language pack switch

- **WHEN** the user switches voice language from zh to a language with assets
- **THEN** subsequent playback draws only from that language's manifest

### Requirement: Empty languages are not selectable

The voice-language picker SHALL show all four languages but disable those whose pack contains no clips; if the configured voice language resolves to an empty pack at load time, the setting SHALL fall back to zh.

#### Scenario: Empty language disabled in picker

- **WHEN** the user opens the voice-language picker and only zh has assets
- **THEN** ja, en, and ko appear disabled and cannot be selected

#### Scenario: Invalid config value snaps back

- **WHEN** the config file specifies voice language ko and the ko pack is empty
- **THEN** the application loads with voice language zh
