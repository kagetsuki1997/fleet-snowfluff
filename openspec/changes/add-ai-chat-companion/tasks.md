## 1. Workspace & crate setup

- [x] 1.1 Create `crates/fleet-snowfluff-ai` crate and add it to the workspace `members` in the root `Cargo.toml`; verify `cargo check -p fleet-snowfluff-ai` succeeds.
- [x] 1.2 Add a streaming-capable HTTP client dependency and a maintained YAML-parsing crate (not `serde_yaml`, which is unmaintained) to `fleet-snowfluff-ai`; verify `cargo check` succeeds. Used `reqwest` 0.13.5 (json, stream features) and `serde-saphyr` 1.2.0 (chosen over `serde_yml`, which is itself now flagged unmaintained/unsound per RUSTSEC-2025-0068 — `serde-saphyr` fits our all-or-nothing `from_str::<Persona>` parsing need without a Value-DOM layer we don't use).
- [x] 1.3 Relocate the persona seed file to `personas/aemeath.yaml` at the repo root — already done during design; verify with `ls personas/aemeath.yaml` and confirm no stray `persona-aemeath.yaml` remains at the repo root.

## 2. `ai` crate: types, trait, prompt assembly

- [x] 2.1 Define `Message`, `Response`/`StreamChunk`, `Persona`, and `ProviderKind` types (with `Serialize`/`Deserialize` where needed for IPC); verify the crate compiles with unit tests for basic (de)serialization round-trips. (`Response` folded into `Message`/`StreamChunk`+the caller assembling the final text from chunks; no separate type needed.)
- [x] 2.2 Define the `AiProvider` trait with a streaming chat method; verify a trivial in-crate implementation compiles against it. Used `async-trait` to keep the trait object-safe (`Box<dyn AiProvider>`), `futures-core`/`futures-util` for the boxed `Stream` return type.
- [x] 2.3 Implement persona YAML loading with graceful fallback on parse failure, mirroring `fleet-snowfluff-core::config::sanitize`'s pure-function style; verify with unit tests covering the `ai-persona` spec's scenarios (no user file, malformed file, corrected file). Also verified the real, committed `personas/aemeath.yaml` parses successfully against the `Persona` struct.
- [x] 2.4 Implement prompt assembly (persona + a capped recent-turn history window + the new user message → final request messages), selecting few-shot examples by `response_language`; verify with unit tests covering `ai-provider`'s "Bounded conversation context" requirement. `Auto` response-language resolution is left to the caller (app crate, via `core`'s detected UI language) since resolving it is `core`'s concern, not this crate's — `assemble_messages` always takes an already-concrete `Language`.

## 3. `ai` crate: provider implementations

- [ ] 3.1 Implement `OpenAiCompatible` as a `build_request`/`parse_response`/`parse_chunk` pure-function split plus a thin HTTP-call glue function; verify with fixture-based unit tests (success, error, malformed body) and a manual `#[ignore]`d integration test gated on a real API key (never run in CI).
- [ ] 3.2 Implement `Anthropic` with the same split, using `x-api-key`/`anthropic-version` headers and its distinct request/response shape; verify with fixture-based unit tests and a manual `#[ignore]`d integration test.
- [ ] 3.3 Implement `Ollama` with the same split, no auth, targeting a local endpoint; verify with fixture-based unit tests and a manual `#[ignore]`d integration test requiring a locally running Ollama.
- [ ] 3.4 Implement `Mock`, echoing the input in artificially-delayed chunks to exercise the streaming path; verify with a unit test asserting the echoed content and that more than one chunk is produced.
- [ ] 3.5 Implement live model-listing for OpenAI (`GET /v1/models`), Anthropic (`GET /v1/models`, paginated), and Ollama (`GET /api/tags`); verify with fixture-based unit tests parsing sample list responses, per `ai-provider`'s "Live model listing" requirement.

## 4. App crate: config & secrets storage

- [ ] 4.1 Define `AiSettings` (master switch defaulting to `false`, `active_provider: Option<ProviderKind>` defaulting to `None`, per-provider settings blocks for openai/anthropic/ollama/mock) and its `ai-config.json` load/save in a new `ai_config_store.rs`, mirroring `config_store.rs`; verify with unit tests for missing/corrupt-file fallback, matching `ai-provider`'s "AI features disabled by default" and "No provider configured by default" scenarios.
- [ ] 4.2 Define `ProviderCredentials` and its `secrets.json` load/save in a new `secrets_store.rs`, applying restrictive file permissions where the OS supports it; verify the file is created with those permissions on unix and that `ai-config.json` never contains key material.
- [ ] 4.3 On first need, copy the bundled `personas/aemeath.yaml` into the user's config directory if no persona file exists there yet, then always read fresh per message afterward; verify against the `ai-persona` "Bundled default persona" and "Fresh reload on every message" scenarios.

## 5. App crate: settings UI — AI tab

- [ ] 5.1 Add `get_ai_settings`/apply-and-save commands for the AI tab (master switch, active provider, per-provider fields), following the existing `get_personalization` pattern in `commands.rs`; verify the tab reflects previously saved state on reopen.
- [ ] 5.2 Add a command to fetch live models for the currently selected provider and surface fetch failures inline in the settings form; verify against `ai-provider`'s "Invalid credentials surface at model-selection time" scenario.
- [ ] 5.3 Add the one-time cloud-provider disclosure flow for OpenAI/Anthropic, blocking activation until acknowledged, with no such flow for Ollama/Mock; verify against `settings-ui`'s "Cloud provider disclosure in AI tab" scenarios.
- [ ] 5.4 Add the AI tab to the settings webview's tab list; verify the settings window shows four tabs (personalization, AI, update, about) per the modified `settings-ui` "Settings window" requirement.
- [ ] 5.5 Add every new AI-tab string (master switch, provider labels, disclosure text) to all five `locales/*.json` files; verify `locale_dictionary` returns them for each `UiLanguage`.

## 6. App crate: chat window

- [ ] 6.1 Create the global chat webview window using the existing open-or-focus singleton pattern from `settings_window.rs`; verify only one instance ever exists no matter how many pets trigger it.
- [ ] 6.2 Implement session file management (JSONL append, `chat-logs/<date>/<timestamp>_<session-id>.jsonl`, folder fixed at session start, new session only on explicit "New Chat"); verify against `ai-chat`'s "Session persistence" scenarios (spans midnight, reopening does not start a new session).
- [ ] 6.3 Implement resume-with-scrollback on window open (load the latest session's JSONL and render its history); verify against the "Session resume with scrollback" scenario.
- [ ] 6.4 Implement the send-message command: enforce a single generation in flight globally, disable input while pending (including across a close/reopen), stream via `Channel<T>`, and append the final message to the session log as the source of truth regardless of window state; verify against `ai-chat`'s "Single in-flight generation" and "Background completion, explicit cancellation" scenarios.
- [ ] 6.5 Implement the stop control (explicit cancel) and "New Chat" (implicitly cancels any pending generation, then starts a fresh session); verify against the corresponding `ai-chat` scenarios.
- [ ] 6.6 Implement inline error display for failed requests (excluded from future context replay, no automatic retry, input re-enabled); verify against `ai-chat`'s error-handling scenarios.
- [ ] 6.7 Add every chat-window string (input placeholder, stop/new-chat labels, error message templates) to all five `locales/*.json` files; verify `locale_dictionary` returns them for each `UiLanguage`. (Status bubble states use fixed glyphs, not localized text, so no locale task is needed for it.)

## 7. App crate: pause integration & status bubble

- [ ] 7.1 Tie chat-window-open/generation-pending state to `PetManager::set_paused`, remembering and restoring whatever pause state preceded it; verify against `ai-chat`'s "Pause during chat activity" scenarios.
- [ ] 7.2 Add double-click gesture detection to the mouse-poll loop in `manager.rs` (movement threshold to distinguish a tap from a drag start, same-target timing window for the second tap), opening the chat window; verify against `ai-chat`'s "Chat opened by double-click" scenarios, tuning thresholds against real hardware per design.md's open question.
- [ ] 7.3 Create the status bubble webview anchored to `pets[0]`, with hidden/thinking/unread-reply/unread-failure states that clear on chat-window focus; verify against `ai-chat`'s "Status bubble" scenarios.
- [ ] 7.4 Sync the status bubble's position to `pets[0]` from the existing `apply_drag_position` call site during a manual drag; verify against `ai-chat`'s "Status bubble tracks manual drag" scenario and the modified `pet-behavior` "Dragging a paused pet" scenario.

## 8. Verification

- [ ] 8.1 Run `cargo test --workspace` and confirm all new unit tests pass with no regression to existing `core` tests (config, locale, motion).
- [ ] 8.2 Manually exercise the Mock provider end-to-end (send a message, observe streaming, unread bubble, resume after reopen, New Chat cancellation) as a smoke test of the full pipeline before enabling any real provider.
- [ ] 8.3 Manually verify each real provider's live model-fetch and a successful chat round-trip with valid credentials (opt-in, not part of CI).
- [ ] 8.4 Run `openspec validate add-ai-chat-companion --strict` and resolve any reported issues.
