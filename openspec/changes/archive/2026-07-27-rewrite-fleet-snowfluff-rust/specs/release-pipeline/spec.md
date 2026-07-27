# Spec: release-pipeline

## ADDED Requirements

### Requirement: Quality gates on push and PR

CI on GitHub-hosted runners SHALL run `cargo fmt --check`, `cargo clippy` with warnings denied, and `cargo test` for every pull request and every push to `develop` and `main`; failures block merge. Core-crate tests SHALL run headless (no display server or GPU required).

#### Scenario: Lint failure blocks

- **WHEN** a pull request contains a clippy warning
- **THEN** the quality workflow fails and the PR cannot merge until fixed

### Requirement: Tag-triggered release builds

Pushing a `v*` tag SHALL trigger a matrix build producing: Windows NSIS `.exe`, Linux `.deb` and AppImage, and macOS universal-binary `.dmg` plus the updater `.app.tar.gz` — each with its minisign signature — and an updater `latest.json`, all uploaded to a **draft** GitHub Release for that tag.

#### Scenario: Tag produces draft release

- **WHEN** the maintainer pushes tag `v0.1.0`
- **THEN** a draft release exists containing installers for all three platforms, signatures, and `latest.json`

### Requirement: Manual publish gate

Releases SHALL remain drafts until manually published after the per-platform smoke checklist (versioned in the repository) passes; running updaters only see a release once published.

#### Scenario: Draft invisible to updaters

- **WHEN** a draft release for a newer version exists but is unpublished
- **THEN** running applications' update checks report no update available
