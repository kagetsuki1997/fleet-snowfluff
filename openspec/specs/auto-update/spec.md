# Spec: auto-update

## Purpose

TBD

## Requirements

### Requirement: Startup update check

Unless skip-all-updates is enabled, the application SHALL check GitHub Releases for a newer version in the background at startup; when one is found and its version is not the skipped version, the settings window SHALL open on the update tab.

#### Scenario: New version prompts

- **WHEN** the app starts and a newer, non-skipped release exists
- **THEN** the settings window opens on the update tab showing the new version

#### Scenario: Skipped version stays quiet

- **WHEN** the user previously chose skip-this-version for the available release
- **THEN** startup completes with no update prompt

### Requirement: Signed update installation

Updates SHALL be downloaded and installed via the Tauri updater with a valid minisign signature verified against the embedded public key; packages failing verification SHALL be rejected without installation.

#### Scenario: Valid update installs

- **WHEN** the user confirms installing an available update
- **THEN** the signed package is downloaded, verified, installed, and the application relaunches at the new version

#### Scenario: Bad signature rejected

- **WHEN** a downloaded package fails signature verification
- **THEN** installation aborts and an error is shown; the current version keeps running

### Requirement: Update opt-outs persist

Skip-this-version SHALL suppress prompts for exactly that version; skip-all-updates SHALL suppress automatic checks entirely; both persist in config, and manual checking from the update tab SHALL always remain possible.

#### Scenario: Skip-all silences startup checks

- **WHEN** skip-all-updates is enabled
- **THEN** no startup check occurs, but the manual check button in settings still works
