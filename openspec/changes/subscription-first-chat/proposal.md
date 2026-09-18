## Why

Stage 1 shipped chat, but every provider requires the user to obtain and paste in their own API key. Most people who would want Fleet to feel like "their own desktop AI" already pay for ChatGPT or Claude — asking them to additionally set up separate, usage-billed API access before Fleet's chat does anything is the single biggest adoption barrier standing between "installed the app" and "actually uses the AI features." Reusing a subscription the user already has removes that barrier without asking Fleet to reimplement anyone's OAuth.

## What Changes

- Add a subscription auth method for Anthropic: obtains a bearer token from the Claude Code CLI (`claude setup-token`) and sends it to Anthropic's Messages API directly (`Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20`), reusing the existing `Anthropic` provider's request/response handling — no separate CLI dependency beyond `claude` itself.
- Add a subscription auth method for OpenAI: a new provider implementation that wraps `codex exec --json` as a subprocess, parsing its newline-delimited JSON event stream into the same `ChatStream` shape every other provider produces. Shipped as **experimental** — implemented against documentation only, not manually verified, and surfaced as such in the settings UI (no ChatGPT/Codex subscription was available to test against during this change).
- Detect an already-authenticated CLI (`claude auth status`, and the Codex equivalent) and use it directly; do not build custom OAuth/login UI. Triggering the CLI's own `login` flow from inside Fleet is attempted but not guaranteed — if headless (no-TTY) login misbehaves, the settings UI tells the user to run the CLI's login command themselves in a terminal and then refresh.
- **BREAKING**: Replace `AiSettings.active_provider: Option<ProviderKind>` (a single flat pointer) with a list of enabled provider profiles (`provider` + `auth_method` + `model`) plus a single manually-chosen default profile. This is only the data model and settings UI for having more than one profile configured at once — no automatic routing or fallback between profiles ships in this change (that's a later, separate change). Existing installs auto-migrate their single configured provider into one API-key profile; no re-entry required.
- Disclosure acknowledgment (already required once per cloud provider before Stage 1) becomes per (provider, auth method) pair: acknowledging Anthropic via API key does not silently cover Anthropic via subscription, since a materially different credential/account is now involved.
- Add `ProviderError` variants — `RuntimeUnavailable`, `SubscriptionExpired`, `QuotaExhausted` — so the settings UI can distinguish "CLI not installed," "was logged in, now isn't," and "authenticated but this period's usage cap is hit" from each other and from the existing `Auth`/`RateLimited` cases.
- No new credential is persisted for subscription auth. `ProviderCredentials`/`secrets.json` gains nothing new for it — the CLI already owns login/refresh, and Fleet re-derives status/tokens from it at request time rather than caching a copy that could drift.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `ai-provider`: adds a per-provider auth-method dimension (API key vs. subscription) alongside the existing provider dimension, adds the two subscription-capable implementations, replaces the single active-provider pointer with an enabled-profiles list + default profile, extends per-provider disclosure to be per (provider, auth method), and adds new `ProviderError` states for CLI/subscription-specific failures.
- `settings-ui`: the AI tab's active-provider selector becomes a profile list (add/enable a provider+auth-method combination, pick one as default) instead of a single radio choice; a provider's connection status must be able to show CLI-specific and subscription-specific states (not installed, not logged in, expired, quota exhausted) in addition to the existing configured/unconfigured/disclosure states; the Codex/OpenAI subscription option is visibly marked experimental.

## Impact

- `crates/fleet-snowfluff-ai/src/settings.rs`: `AiSettings` shape change (`active_provider` → `enabled_profiles` + `default_profile`), migration of existing single-provider configs.
- `crates/fleet-snowfluff-ai/src/message.rs`: new `ProviderError` variants.
- `crates/fleet-snowfluff-ai/src/provider.rs`, `providers/anthropic.rs`, new `providers/codex.rs` (or similar): subscription-auth request paths.
- `crates/fleet-snowfluff-ai/src/credentials.rs`: unchanged in shape (no new persisted secret), but its "what Fleet stores" boundary becomes an explicit design point.
- `crates/fleet-snowfluff/src/ai_commands.rs` (`build_provider`, `set_active_provider`, `acknowledge_provider_disclosure`) and `crates/fleet-snowfluff/src/chat_commands.rs` (active-provider read at line ~197): re-worked to resolve a default profile instead of a single active provider.
- Settings window AI tab (webview side): profile list UI, experimental-provider labeling, new status states.
- New runtime dependency: shelling out to the `claude` CLI (already required for the Anthropic subscription path) and, for the experimental OpenAI path, the `codex` CLI.
- Out of scope: Task Routing and Fallback logic between profiles (deferred to a later change) — this change ships the data model and UI for multiple configured profiles, not automatic selection between them.
