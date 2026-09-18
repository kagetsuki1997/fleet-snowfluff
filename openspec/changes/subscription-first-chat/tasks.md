## 1. Data model: profiles, migration, new error states

- [x] 1.1 Add `AuthMethod` enum (`ApiKey`, `Subscription`) and `ProviderProfile` struct (provider, auth_method, model, disclosure_acknowledged, plus provider-specific fields like base_url for API-key auth) to `fleet-snowfluff-ai/src/settings.rs`; verify with unit tests that a profile round-trips through JSON.
- [x] 1.2 Replace `AiSettings.active_provider: Option<ProviderKind>` with `enabled_profiles: Vec<ProviderProfile>` and `default_profile: Option<ProfileId>`; verify `AiSettings::default()` yields an empty profile list and no default, matching the existing "no provider configured by default" test.
- [x] 1.3 Update `AiSettings::sanitize` to detect the old shape (`active_provider` present, no `enabled_profiles`) and convert it into exactly one API-key profile set as default, carrying over that provider's model and `disclosure_acknowledged`; verify with a unit test that feeds a literal Stage-1-shaped JSON fixture and asserts the resulting profile list and default.
- [x] 1.4 Verify a fresh/corrupt config (neither old nor new shape) still yields zero profiles and no default, per the existing corrupt-JSON-yields-defaults test pattern in `settings.rs`.
- [x] 1.5 Add `RuntimeUnavailable`, `SubscriptionExpired`, `QuotaExhausted` variants to `ProviderError` in `fleet-snowfluff-ai/src/message.rs`, each carrying a `String`; verify `Display` impls and add a test asserting `RateLimited` and `QuotaExhausted` produce visibly different messages.

## 2. Anthropic subscription auth

- [ ] 2.1 Add a function that shells out to `claude setup-token` (or `claude auth status` first, if a status check is needed to distinguish "not logged in" from "token retrieval failed") and returns a bearer token or a mapped `ProviderError` (`RuntimeUnavailable` if the binary isn't found, `SubscriptionExpired`/`Auth` if the CLI reports not-authenticated); verify with tests that stub the subprocess call by extracting the parsing logic into a pure function over literal CLI-output fixtures (success JSON, not-logged-in JSON, missing-binary error), matching this codebase's existing "parse over literal strings" testing precedent.
- [ ] 2.2 Extend `providers/anthropic.rs`'s request-building to accept a subscription credential (`Authorization: Bearer <token>` + `anthropic-beta: oauth-2025-04-20` header) as an alternative to the existing `x-api-key` path; verify with a unit test asserting the two header shapes for the same request body.
- [ ] 2.3 Wire profile auth_method selection into whichever code path constructs the Anthropic provider, so a `Subscription`-auth profile fetches a token before each request rather than at profile-enable time (per design.md's "nothing cached" decision); verify manually against a real logged-in `claude` CLI that a chat message round-trips successfully end to end.
- [ ] 2.4 Map subprocess/CLI failures (binary missing, not logged in, token fetch failed) to the new `ProviderError` variants; verify with tests over literal fixture outputs.

## 3. OpenAI/Codex subscription auth (experimental)

- [ ] 3.1 Add a new provider implementation (e.g. `providers/codex.rs`) that spawns `codex exec --json <prompt>` and parses its newline-delimited JSON event stream, extracting the assistant's text deltas into `StreamChunk`s; verify by parsing literal fixture event streams (captured from Codex's documented event shapes, since no live subscription is available) covering a normal multi-event reply, an error event, and a truncated/interrupted stream.
- [ ] 3.2 Add CLI detection and status mapping (`codex` binary present/absent, logged in/not, per whatever status subcommand it exposes) to the same `ProviderError` variants as the Anthropic path; verify with tests over literal fixture outputs, same style as 2.4.
- [ ] 3.3 Investigate whether `codex exec` supports disabling file/shell tool access for a plain-chat use case (see design.md Open Questions); document the finding as a code comment at the call site regardless of outcome, since it determines whether the experimental caveat needs a tool-access-specific warning.
- [ ] 3.4 Add an `is_experimental()` (or equivalent) marker on the provider/profile so the settings UI can render it per the new "Experimental provider marking" requirement; verify with a test asserting the Codex-subscription profile reports experimental and every other profile does not.

## 4. App-crate integration

- [ ] 4.1 Update `build_provider()` (`fleet-snowfluff/src/ai_commands.rs`) to take a `ProviderProfile` (or profile ID resolved against `enabled_profiles`) instead of a bare `ProviderKind`, dispatching to the API-key or subscription implementation per its `auth_method`; verify existing `build_provider` tests still pass with the new signature.
- [ ] 4.2 Replace `set_active_provider` with commands to enable/disable a profile and set the default profile; verify `disclosure_ok` (renamed/adapted to key on provider+auth_method) still refuses activation until that pair's disclosure is acknowledged, per the existing test pattern in `ai_commands.rs`.
- [ ] 4.3 Update `acknowledge_provider_disclosure` to record acknowledgment per (provider, auth_method) instead of per provider; verify with a test that acknowledging one auth method for a provider does not mark the other as acknowledged.
- [ ] 4.4 Update `chat_commands.rs`'s active-provider read (~line 197) to resolve the default profile instead of `active_provider`; verify the existing chat-flow tests (or add one) covering "no default profile configured" behaving the same as today's "no active provider" case.
- [ ] 4.5 Add a Tauri command (or extend `get_ai_settings`) that reports fresh per-profile status (calls the relevant CLI, maps to the new `ProviderError`-derived states) for the settings UI to display, per the "Provider status display" requirement's "checked fresh, not cached" rule.

## 5. Settings UI (AI tab)

- [ ] 5.1 Replace the AI tab's single active-provider radio with a profile list: enable/disable a provider+auth-method combination, mark one as default; verify by opening the settings window and confirming multiple profiles can be enabled simultaneously with one visibly marked default.
- [ ] 5.2 Update the cloud-provider disclosure dialog to show auth-method-specific wording (naming the CLI/account for subscription, the key for API-key) and gate profile activation on it, per the modified `settings-ui` disclosure requirement; verify by enabling each of the four cloud (provider, auth_method) combinations and confirming each shows its own disclosure once.
- [ ] 5.3 Add status rendering for the new states (runtime unavailable, subscription expired, quota exhausted) distinct from existing states, refreshed on tab open; verify manually with the Anthropic path (log out of `claude` CLI, confirm the tab reflects it without restarting Fleet).
- [ ] 5.4 Add the experimental-provider visual marking on the OpenAI-subscription option, visible before it is enabled as well as after; verify by inspecting the auth-method picker for OpenAI.

## 6. Verification

- [ ] 6.1 Manually verify the full Anthropic subscription path end-to-end on this machine (real `claude` CLI, real Pro subscription): enable the profile, acknowledge disclosure, send a chat message, confirm a persona-consistent streamed reply; confirm log-out is reflected as `SubscriptionExpired`/not-logged-in on the next status check.
- [ ] 6.2 Confirm existing Stage-1 provider tests (OpenAI/Anthropic API-key paths, Ollama, Mock) still pass unmodified in behavior, only in call shape, after the profile-model migration.
- [ ] 6.3 Run the full existing test suite (`cargo test` across the workspace) and confirm no regressions.
- [ ] 6.4 Document in the settings UI (or a linked help text) that OpenAI/Codex subscription support is experimental and unverified, and file a follow-up note (issue or TODO) to remove that marking once someone with a ChatGPT/Codex subscription confirms it works end-to-end.
