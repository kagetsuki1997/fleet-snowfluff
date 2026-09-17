## Context

See `proposal.md` - Why for motivation. Constraints that shape this design, established by reading the existing codebase during discovery:

- `fleet-snowfluff-core` is deliberately I/O-free (`rand`, `serde`, `serde_json`, `sys-locale` only) and describes itself as a "pure behavior engine" — it must stay AI-agnostic.
- Pet windows (`pet.rs`) are raw `tauri::window::Window` instances rendered via WGPU (`pet_quad.wgsl`), with no JS runtime at all; they receive no native click events, so `manager.rs` does its own hit-testing against a globally-polled mouse position (used today for drag-start and the right-click quick menu). Any new gesture has to plug into this same polling loop.
- The settings window (`settings_window.rs`) is the app's only existing webview, a singleton with three tabs, using a `get_*`/`apply_and_save`/`switch-tab` command pattern this change reuses for a fourth tab.
- Pet instances are managed as a `PetSwarm`/`Vec<PetWindow>` of behaviorally-identical clones (`swarm.rs`), not independent identities; `PetManager::set_instance_count` only ever pushes/pops from the end of that vector, so index 0 is stable across any live resize.
- `PetManager::set_paused` already exists, is already global, and is already tray-exposed; paused pets keep playing lively reaction animations (`PauseAnimationScheduler`) rather than freezing on a static frame — but `pet.rs::tick()` checks `if self.dragging { return; }` _before_ checking `self.paused`, so a paused pet remains fully draggable today. This was an unexercised edge case before this change and becomes load-bearing once the status bubble depends on `pets[0]`'s position.

## Goals / Non-Goals

**Goals:**

- Ship a working, multi-provider, streaming chat experience behind a pluggable trait, without compromising `core`'s existing purity.
- Reuse existing mechanisms (pause, the settings-window tab pattern, the mouse-poll hit-testing loop) instead of inventing parallel ones.
- Keep every new on-disk format (`secrets.json`, `ai-config.json`, `personas/*.yaml`, session logs) in the same "plain file a user could read or hand-edit" style the rest of the app already uses (`config.json`, the settings-ui path hint).

**Non-Goals:**

- Context-awareness / rule-engine-triggered ambient commentary (Stage 2's actual use of an "ambient bubble") — this change's status bubble is a narrower, Stage-1-scoped notification indicator, not that feature.
- Structured emotion-tag output or animation hookup (Stage 2 item 4).
- Long-term/RAG memory, affinity scoring, TTS/STT (Stage 2-3 items).
- Multi-persona switching UI (`personas/` is laid out to support it later; only one file is loaded in Stage 1).
- Browsing past chat sessions from within the app (files exist on disk; no in-app list/picker yet).

## Decisions

**Three-crate topology, one-way dependency.** New `fleet-snowfluff-ai` crate holds the `AiProvider` trait, types, prompt assembly, and all four provider implementations, depending on nothing project-specific. `fleet-snowfluff-core` gains no new dependency on it. `fleet-snowfluff` (the app) is the only crate that depends on both, and does the integration. _Alternative considered_: have `core` depend on `ai` directly (simpler for anything that needs both, e.g. mapping emotion tags to existing animation triggers) — rejected because it would make `core`'s "pure, AI-agnostic" claim false as a matter of its dependency graph, even before any animation-hookup work (Stage 2) exists to justify it.

**Config split into three files, not merged into `core::Config`.** `secrets.json` (keys only), `ai-config.json` (master switch, active provider, per-provider settings), and a user copy of `personas/aemeath.yaml`, all new, alongside the existing `config.json`. _Alternative considered_: add AI fields directly to `core::Config` so there's one file — rejected because `core::Config`'s type would then need to either import `ai`'s types (reopening the crate-boundary decision above) or duplicate a parallel, weakly-typed shadow of them (a drift risk: two sources of truth for e.g. "which provider is selected"). The user-facing goal of "one settings window" is preserved by adding a fourth tab, not by merging the files it reads from — UI unification and data-model unification are separate axes.

**Anthropic gets its own provider implementation, not `OpenAiCompatible` with a different URL.** Verified during design: Anthropic's Messages API is a distinct wire protocol (`x-api-key` + `anthropic-version` headers, system prompt as a top-level field, distinct request/response shape) — confirmed via Anthropic's own API docs, including that `GET /v1/models` exists and returns paginated `id`/`display_name`/capability data, which is what makes live model-fetch viable for it (see the "Live model listing" requirement in `ai-provider`).

**Streaming delivered via Tauri's `Channel<T>`, not the existing `emit_to` pattern.** The only existing IPC-push precedent in this codebase (`settings_window.rs`'s `switch-tab` event) is a one-shot signal to a fixed window label — not built for a rapid, potentially-overlapping sequence of token chunks. `Channel<T>` is scoped to one command invocation, so overlapping requests (a new message before a prior stream finishes, however unlikely given the single-in-flight-generation rule) can't cross-talk by construction, without hand-rolled request IDs. This is a new IPC pattern for the codebase either way; `Channel<T>` was chosen because the disambiguation it provides for free is exactly the kind of correctness problem this feature actually has (see "Single in-flight generation" in `ai-chat`). Confirmed this only concerns the chat window / status bubble webviews — it has nothing to do with the pet windows' platform-specific code (`platform/*.rs`), which exists solely for the pet's raw WGPU rendering and input polling and never touches Tauri's webview/IPC layer.

**All four providers' settings stored simultaneously, keyed independently, with one active-provider pointer.** _Alternative considered_: a single active `ProviderConfig` blob, overwritten on switch — rejected because supporting four providers from Stage 1 (rather than the source doc's original single-cloud-provider phase-in) only makes sense if switching between them doesn't require re-entering credentials each time.

**Global chat window and global status bubble, not per-pet-instance.** Since `PetSwarm` already treats every instance as a behaviorally-identical clone with no individual identity, one global chat window (regardless of instance count) and one status bubble (anchored only to `pets[0]`, confirmed stable per the Context section above) matches how the rest of the app already treats multi-instance, and avoids needing to track N bubble positions instead of one.

**Double-click gesture added to the existing mouse-poll loop.** Buffered press→release with a movement threshold (to distinguish a tap from a drag start, which today begins unconditionally on press) plus a same-pet timing window for a second tap — reusing the `bounds_contains` hit-test the right-click quick menu already uses. No new input-handling mechanism, an extension of the existing one.

**Pause reused as-is for chat, extended to also hold across background completion.** Opening the chat window calls the existing `PetManager::set_paused(true)`, remembering whatever pause state preceded it (so a user who had manually paused via the tray isn't un-paused by closing chat). Because closing the window does not cancel an in-flight generation (see below), the pause condition is "window open OR a generation is pending," not just "window open" — otherwise `pets[0]` could resume wandering mid-generation, reopening a position-tracking problem the design otherwise avoids.

**The drag-bypasses-pause behavior is resolved by following it, not disabling it.** Since `tick()`'s paused branch never invokes the wander state machine, the _only_ way `pets[0]` can move while the status bubble is visible is a manual drag — so the bubble only needs to sync its position inside the existing `apply_drag_position` call site (already called every tick during a drag), not on every tick generally. _Alternative considered_: disable dragging on `pets[0]` while a session is active — rejected as inconsistent (identical-looking pet instances would behave differently) and as giving up a working interaction for no real benefit once the actual fix is this cheap.

**In-flight generation decoupled from window lifecycle.** The background task's source of truth is the session's JSONL log (always appended to on completion, regardless of whether any window is open); pushing to the `Channel<T>` is best-effort only, for whichever window happens to be open and listening at that moment. This is what makes "closing the window doesn't cancel" safe to implement without the task needing to handle a vanished IPC receiver as an error case.

**Session logs are JSONL, one file per session, foldered by start date.** Chosen over a single JSON array per session to avoid rewriting the whole file on every append. Chosen over one giant log to make individual sessions independently discoverable on disk (`chat-logs/<date>/<timestamp>_<session-id>.jsonl`) even without in-app browsing (a Non-Goal for Stage 1). A session's folder is fixed at creation time regardless of how long it runs.

**Mock provider is a real, user-selectable implementation, not test-only.** Beyond serving as the fixture for integration-style tests of the full request → stream → log → bubble-state pipeline, it doubles as a zero-setup way for anyone (including end users) to see the chat feature work before configuring a real provider — directly serving the source doc's own "validate whether chatting is fun before investing further" strategy.

**Testing strategy mirrors `config.rs`'s existing style: pure functions over already-parsed data, not a mocking framework.** Each provider splits into a `build_request` and a `parse_response`/`parse_chunk` function, unit-tested against literal fixture strings (a success body, an error body, a malformed body) with no network involved — matching `config::sanitize`'s "works over string contents so it's testable without a filesystem" precedent. The actual `reqwest` call is a thin, deliberately untested-by-unit-tests glue function. No automatic network calls in CI, for any provider; real end-to-end verification is a manual, credential-gated, opt-in test only.

**Plain-text responses only in Stage 1.** Structured output support is uneven across the four providers — reliable on OpenAI/Anthropic, unreliable on small local Ollama models per the source doc's own §3.4/§4.1 concerns about local-model consistency — so emotion-tag extraction is deferred to Stage 2, when it can be designed against the actual animation-hookup use case instead of speculatively now.

**`personas/` is a root-level directory, mirroring `locales/`, not `rust-embed`.** A single small text file (today) fits the `include_str!` pattern `i18n.rs` already uses for the five locale JSON files better than `rust-embed`'s directory-of-binary-assets pattern (`assets.rs`'s use for GIFs/voice).

**Chat/settings window opacity is CSS-based, not a native per-platform window-alpha call.** Pet windows fade via a custom wgpu shader uniform (non-Windows) or raw `UpdateLayeredWindow` GDI compositing (Windows) — neither applies to a plain webview, and Tauri/tao expose no cross-platform "set window opacity" API at all (confirmed against the vendored source). The first design was three new per-platform functions (`NSWindow.alphaValue`, `SetLayeredWindowAttributes`, GTK `set_opacity`) mirroring how `platform/*.rs` already reaches around Tauri for other things. That hit a real verification wall: only macOS compiles natively on this dev machine; Windows cross-compilation failed on a pre-existing, unrelated toolchain issue (`cargo-xwin`'s nix-wrapped clang rejects `-fPIC` for the MSVC target while building `ring`'s C sources); Linux would need a `just build-linux` container run to compile-verify at all, per that module's own documented practice. Given the choice, we went with `.transparent(true)` (already used by pet windows) plus a plain CSS `opacity` style on each window's content root instead — one implementation, no per-platform code, no verification gap, at the cost of fading web content as a group rather than the native title bar/OS chrome along with it.

## Risks / Trade-offs

- **[Risk]** A background-completing generation could still fail to push a `Channel<T>` update if its window closed mid-stream. → **Mitigation**: the session log is the source of truth regardless; the chat window reads "is a generation still pending for this session" on open rather than relying solely on having received every chunk live.
- **[Risk]** Unbounded `chat-logs/` growth with no pruning in Stage 1. → **Mitigation**: accepted for Stage 1, consistent with the project's existing "point at the file path, let the user manage it" pattern (settings-ui's `config.json` hint); revisit if real usage makes this a problem.
- **[Risk]** `serde_yaml` (the obvious choice for parsing `persona.yaml`) is unmaintained/archived. → **Mitigation**: pick a maintained fork at implementation time; flagged as an explicit task rather than silently defaulting to it.
- **[Risk]** Cloud provider usage costs are the user's own responsibility with no spend cap beyond the fixed per-request output-token limit and the bounded context window. → **Mitigation**: out of scope to solve fully in Stage 1; the two caps bound the worst case per request, but this is not a budget/quota feature.
- **[Risk]** Double-click timing/movement thresholds may need real-hardware tuning across platforms, similar to the existing quick-menu popup's own noted diagnostic uncertainty about background-polling-driven input on Windows. → **Mitigation**: ship with reasonable defaults (~300-400 ms, small movement threshold) and treat as tunable, not a spec-level commitment.

## Migration Plan

All new files; no existing user data changes shape. `persona-aemeath.yaml` was already relocated from the repo root to `personas/aemeath.yaml` during design discovery (source-tree change only, not a user-facing migration). No rollback concerns beyond the usual "new feature behind a default-off switch" — `ai_enabled` defaulting to `false` means existing installs see no behavior change until a user opts in.

## Open Questions

- Exact double-click timing/movement thresholds — tunable during implementation and testing, does not affect the spec-level behavior ("double-click, not single-click or drag, opens chat").
- Which maintained `serde_yaml` alternative to depend on — a implementation-time library choice, does not affect the `ai-persona` capability's observable behavior.
