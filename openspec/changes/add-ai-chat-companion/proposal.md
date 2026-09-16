## Why

Fleet Snowfluff currently has no AI capability at all. The feature-planning doc (`docs/fleet-snowfluff-feature-planning.md`) lays out a three-stage roadmap for adding one; this change implements Stage 1 — "打地基" (lay the foundation): a pluggable AI provider layer, a real chat experience, and a persona system — so that the core question ("is chatting with this pet actually fun?") can be validated with an alpha release before investing further in context-awareness, local-model-only privacy features, or long-term memory (Stages 2-3). Scope was widened from the doc's original phasing during design discussion: all three real providers (OpenAI, Anthropic/Claude, Ollama) plus a Mock/offline provider ship together in Stage 1 rather than deferring Ollama to Stage 2, since the trait abstraction is only genuinely validated once more than one real implementation exists behind it.

## What Changes

- Add a new `fleet-snowfluff-ai` crate: the `AiProvider` trait, `Message`/`Response`/`Persona` types, prompt assembly, and four concrete provider implementations (`OpenAiCompatible`, `Anthropic`, `Ollama`, `Mock`). `fleet-snowfluff-core` gains no new dependencies and stays AI-agnostic; the app crate is the sole integrator between `core` and `ai`.
- Add streaming chat completions (all four providers), delivered to the webview via Tauri's `Channel<T>` IPC primitive — a new IPC pattern for this codebase.
- Add three new on-disk config files alongside the existing `config.json`: `secrets.json` (API keys, restrictive permissions), `ai-config.json` (master enable switch, active provider, per-provider settings — all providers' configs stored simultaneously so switching is instant), and `personas/aemeath.yaml` (moved from the repo root, bundled as the default persona).
- Add a 4th "AI" tab to the existing settings window, following the existing tab/command pattern, including a one-time data-leaves-the-device disclosure the first time a cloud provider (OpenAI/Anthropic) is selected.
- Add a global chat window (text input, streamed replies, scrollable history, resumable via per-session JSONL logs under `chat-logs/<date>/<timestamp>_<session-id>.jsonl`) opened by double-clicking any pet instance.
- Add a small status-bubble webview anchored to the first pet instance (`pets[0]`) showing a thinking indicator, an unread-reply indicator, or a failure indicator — the sole Stage 1 use of the "ambient bubble" concept; the fuller rule-engine-driven ambient commentary from the planning doc's §4 remains Stage 2 scope.
- Add double-click gesture detection to the pet input-polling loop (tap-vs-drag disambiguation, same-target timing window), distinct from the existing single-press drag-start and right-click quick-menu gestures.
- Extend the existing global pause mechanism (`PetManager::set_paused`) to also hold while an AI request is pending in the background, restoring whatever pause state existed before the chat window was opened.
- **Clarify** (not change) existing pause/drag interaction: a paused pet remains manually draggable (an existing behavior, newly load-bearing since the status bubble must track `pets[0]`'s position during a drag).
- Impose Stage 1 guardrails: master AI-enabled switch defaulting to off, no provider configured by default, a fixed per-request output-token cap, a fixed recent-turn context window (not full session replay), plain-text-only responses (no structured emotion tag yet), at most one generation in flight globally, and no automatic retries on failure.

## Capabilities

### New Capabilities

- `ai-provider`: the provider abstraction (trait, streaming, four implementations), provider/model configuration, credential storage, and the master AI-enabled switch.
- `ai-chat`: the global chat window, the per-pet status bubble, the double-click gesture, session persistence, and the in-flight request lifecycle (pause interaction, cancellation, error handling).
- `ai-persona`: the persona file format, its bundled default, load/fallback/reload behavior, and parse-error reporting.

### Modified Capabilities

- `settings-ui`: adds a 4th "AI" tab (the spec currently states "three tabs — personalization, update, and about") plus the cloud-provider disclosure requirement.
- `pet-behavior`: modifies the "Pause mode" requirement to clarify that manual drag interaction takes precedence over it — a paused pet can still be dragged — since this change's status bubble now depends on that behavior rather than it being an unexercised edge case.

## Impact

- New crate: `crates/fleet-snowfluff-ai` (workspace member), adding `reqwest`-family dependencies and a maintained YAML-parsing crate (not `serde_yaml`, which is unmaintained) to the workspace.
- New app-crate modules: `secrets_store.rs`, `ai_config_store.rs`, `chat_window.rs`/equivalent, `status_bubble.rs`/equivalent, extensions to `manager.rs` (gesture detection, pause condition, drag-position sync to the bubble) and `settings_window.rs`/`commands.rs` (new AI tab commands).
- New repo-root directory: `personas/` (relocated from `persona-aemeath.yaml` at the repo root, already done during design discussion).
- New user-facing config-dir files: `secrets.json`, `ai-config.json`, `chat-logs/`, and a user-editable copy of `persona.yaml`.
- New locale entries required across all five `locales/*.json` files for every new UI surface.
- No changes to `pet-rendering`, `voice`, `localization`, `desktop-integration`, `auto-update`, `app-config`, or `release-pipeline` capabilities.
