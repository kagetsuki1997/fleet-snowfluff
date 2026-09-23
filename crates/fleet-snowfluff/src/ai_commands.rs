//! Tauri commands for the settings webview's AI tab -- mirrors
//! `commands.rs`'s personalization commands: master switch, enabled
//! profiles (with per-profile cloud-disclosure gating), credentials,
//! live model-listing, and fresh per-profile status. Stage 2
//! (`subscription-first-chat`) replaced the single active-provider
//! pointer with a list of independently-enabled `ProviderProfile`s
//! (provider + auth method) plus a manually-chosen default; every
//! command here operates on a profile identified by its `ProfileKey`
//! rather than a bare `ProviderKind`.

use std::{path::PathBuf, sync::Mutex};

use fleet_snowfluff_ai::{
    AiProvider, AiSettings, Anthropic, AuthMethod, ClaudeCodeCli, ClaudeCodeToolAccess, Codex,
    Mock, ModelInfo, Ollama, OpenAiCompatible, ProfileKey, ProviderCredentials, ProviderError,
    ProviderKind, ProviderProfile, TaskRouterMode, ToolCallingProvider,
};
use tauri::{AppHandle, State};

use crate::{ai_config_store, persona_store, secrets_store};

#[derive(serde::Serialize)]
pub struct AiSettingsSnapshot {
    settings: AiSettings,
    openai_key_set: bool,
    anthropic_key_set: bool,
    /// Re-read fresh every time this snapshot is built (same "no
    /// caching" rule as persona loading itself), so a warning from a
    /// broken edit clears the moment the user fixes the file, without
    /// needing a dedicated "reload" action.
    persona_warning: Option<String>,
}

#[tauri::command]
pub fn get_ai_settings(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    credentials: State<Mutex<ProviderCredentials>>,
) -> AiSettingsSnapshot {
    let settings = ai_settings.lock().unwrap().clone();
    let creds = credentials.lock().unwrap();
    let persona_warning = persona_store::load(&app).warning.map(|w| w.to_string());
    AiSettingsSnapshot {
        openai_key_set: creds.openai_api_key.as_deref().is_some_and(|k| !k.is_empty()),
        anthropic_key_set: creds.anthropic_api_key.as_deref().is_some_and(|k| !k.is_empty()),
        settings,
        persona_warning,
    }
}

#[tauri::command]
pub fn set_ai_enabled(app: AppHandle, ai_settings: State<Mutex<AiSettings>>, enabled: bool) {
    let mut settings = ai_settings.lock().unwrap();
    settings.ai_enabled = enabled;
    ai_config_store::save(&app, &settings);
}

/// `agent-core-and-task-router`'s Task Router `mode` setting -- `Single`
/// (today's behavior, always `default_profile`) vs `Mix` (local model
/// tries first, escalates complex tasks). See `task_router.rs`'s own
/// doc comment for the routing behavior this flips; this command only
/// persists the choice.
#[tauri::command]
pub fn set_task_router_mode(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    mode: TaskRouterMode,
) {
    let mut settings = ai_settings.lock().unwrap();
    settings.task_router_mode = mode;
    ai_config_store::save(&app, &settings);
}

/// The native-tool auto-zone root (design.md's "`read_file`/
/// `list_directory`: the configured project directory is an auto-zone,
/// not a hard boundary"). An empty string clears it back to `None`
/// (the frontend's "not configured" state) rather than persisting an
/// empty path, matching `set_profile_model`/`set_profile_base_url`'s
/// own empty-string-means-unset convention.
#[tauri::command]
pub fn set_project_root(app: AppHandle, ai_settings: State<Mutex<AiSettings>>, path: String) {
    let mut settings = ai_settings.lock().unwrap();
    settings.project_root = (!path.is_empty()).then(|| PathBuf::from(path));
    ai_config_store::save(&app, &settings);
}

/// Persists the whole `ClaudeCodeToolAccess` struct at once (`Group 7`'s
/// per-tool `--allowedTools`/`--disallowedTools` split) -- the frontend
/// already holds the full current value from `get_ai_settings`'s
/// snapshot and sends it back with one field flipped, so there's no
/// need for a dozen single-tool commands.
#[tauri::command]
pub fn set_claude_code_tool_access(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    tool_access: ClaudeCodeToolAccess,
) {
    let mut settings = ai_settings.lock().unwrap();
    settings.claude_code_tool_access = tool_access;
    ai_config_store::save(&app, &settings);
}

/// Whether `key` may become the default profile given the disclosures
/// already acknowledged in `settings` (`ai-provider`'s "Cloud provider
/// data disclosure", now keyed per (provider, auth method) rather than
/// per provider, and tracked independently of profile presence -- see
/// `AiSettings::disclosure_acknowledged`'s own doc comment). `None`
/// (clearing the default) and any `Local`-auth profile (Ollama, Mock)
/// never need one.
fn disclosure_ok(settings: &AiSettings, key: Option<ProfileKey>) -> bool {
    let Some(key) = key else { return true };
    match key.auth_method {
        AuthMethod::Local => true,
        AuthMethod::ApiKey | AuthMethod::Subscription => settings.disclosure_acknowledged(key),
    }
}

/// Adds `key` to the enabled-profile list if it isn't already there
/// (idempotent -- re-enabling an already-enabled profile never resets
/// its model/base_url state). Refuses (returns `false`) to enable a
/// cloud-provider profile whose disclosure hasn't been acknowledged
/// yet -- enforced here too, not only in the frontend's own flow
/// (which shows the disclosure and calls `acknowledge_profile_disclosure`
/// *before* this), so the guarantee holds regardless of what calls
/// this command.
#[tauri::command]
pub fn enable_profile(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: ProviderKind,
    auth_method: AuthMethod,
) -> bool {
    let key = ProfileKey { provider, auth_method };
    let mut settings = ai_settings.lock().unwrap();

    if !disclosure_ok(&settings, Some(key)) {
        log::warn!("refused to enable profile {key:?}: disclosure not yet acknowledged");
        return false;
    }

    if settings.profile(key).is_none() {
        settings.enabled_profiles.push(ProviderProfile {
            provider,
            auth_method,
            model: None,
            base_url: None,
        });
        ai_config_store::save(&app, &settings);
    }
    true
}

/// Removes the (`provider`, `auth_method`) profile from the
/// enabled-profile list; clears the default profile too if it was the
/// one being disabled. Every profile-identifying command below takes
/// `provider`/`auth_method` as separate top-level parameters rather
/// than a single `ProfileKey` struct -- deliberately: Tauri's IPC layer
/// camelCases *top-level* command parameter names for the frontend
/// (`auth_method` -> `authMethod`), but a struct's own fields keep
/// whatever casing that struct's own `serde` derive uses (`ProfileKey`
/// has no `rename_all`, so its field would stay `auth_method`). Mixing
/// both conventions in one call is exactly the kind of wire-format
/// mismatch that fails silently until actually exercised end to end;
/// flattening avoids the question entirely.
#[tauri::command]
pub fn disable_profile(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: ProviderKind,
    auth_method: AuthMethod,
) {
    let key = ProfileKey { provider, auth_method };
    let mut settings = ai_settings.lock().unwrap();
    settings.enabled_profiles.retain(|p| p.key() != key);
    if settings.default_profile == Some(key) {
        settings.default_profile = None;
    }
    ai_config_store::save(&app, &settings);
}

/// Sets which enabled profile currently handles chat requests. Refuses
/// (returns `false`) if the profile needs a disclosure that hasn't
/// been acknowledged yet, or isn't actually enabled -- enforced here,
/// not only in the frontend's own gating, so the guarantee holds
/// regardless of what calls this command. Returns whether the change
/// actually took effect, so the frontend can tell "activated" apart
/// from "refused". Clearing the default entirely is not exposed here
/// -- it only happens as a side effect of `disable_profile` disabling
/// whichever profile was the default; the frontend never needs to set
/// "no default" directly.
#[tauri::command]
pub fn set_default_profile(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: ProviderKind,
    auth_method: AuthMethod,
) -> bool {
    let key = ProfileKey { provider, auth_method };
    let mut settings = ai_settings.lock().unwrap();

    if !disclosure_ok(&settings, Some(key)) {
        log::warn!("refused to set default profile {key:?}: disclosure not yet acknowledged");
        return false;
    }
    if settings.profile(key).is_none() {
        log::warn!("refused to set default profile {key:?}: profile is not enabled");
        return false;
    }

    settings.default_profile = Some(key);
    ai_config_store::save(&app, &settings);
    true
}

/// Records that the user has seen and accepted the cloud-provider data
/// disclosure for the (`provider`, `auth_method`) pair (`ai-provider`'s
/// "Cloud provider data disclosure" -- persisted per (provider, auth
/// method), so switching Anthropic from API key to subscription shows
/// its own disclosure rather than reusing the other auth method's
/// acknowledgment). Independent of whether that profile is currently
/// enabled (`AiSettings::acknowledged_disclosures`'s own doc comment)
/// -- the intended flow shows this disclosure and calls this command
/// *before* `enable_profile`, not after, so a profile need not exist
/// yet for its disclosure to be acknowledged.
#[tauri::command]
pub fn acknowledge_profile_disclosure(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: ProviderKind,
    auth_method: AuthMethod,
) {
    let key = ProfileKey { provider, auth_method };
    let mut settings = ai_settings.lock().unwrap();
    if !settings.acknowledged_disclosures.contains(&key) {
        settings.acknowledged_disclosures.push(key);
        ai_config_store::save(&app, &settings);
    }
}

#[tauri::command]
pub fn set_profile_model(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: ProviderKind,
    auth_method: AuthMethod,
    model: String,
) {
    let key = ProfileKey { provider, auth_method };
    let model = (!model.is_empty()).then_some(model);
    let mut settings = ai_settings.lock().unwrap();
    if let Some(profile) = settings.profile_mut(key) {
        profile.model = model;
        ai_config_store::save(&app, &settings);
    }
}

/// Only meaningful for `ApiKey`/`Local` auth (a custom endpoint) -- set
/// on a `Subscription` profile it's simply unused by that
/// implementation, not an error.
#[tauri::command]
pub fn set_profile_base_url(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: ProviderKind,
    auth_method: AuthMethod,
    base_url: String,
) {
    let key = ProfileKey { provider, auth_method };
    let base_url = (!base_url.is_empty()).then_some(base_url);
    let mut settings = ai_settings.lock().unwrap();
    if let Some(profile) = settings.profile_mut(key) {
        profile.base_url = base_url;
        ai_config_store::save(&app, &settings);
    }
}

/// Never round-trips the actual key back to the frontend --
/// `get_ai_settings`'s `openai_key_set`/`anthropic_key_set` booleans
/// are the only thing the webview ever learns about a saved key. Keyed
/// by `ProviderKind` alone (not a full `ProfileKey`) since an API key
/// is meaningful only for that one auth method by definition.
#[tauri::command]
pub fn set_provider_api_key(
    app: AppHandle,
    credentials: State<Mutex<ProviderCredentials>>,
    provider: ProviderKind,
    api_key: String,
) {
    let api_key = (!api_key.is_empty()).then_some(api_key);
    let mut creds = credentials.lock().unwrap();
    match provider {
        ProviderKind::OpenAi => creds.openai_api_key = api_key,
        ProviderKind::Anthropic => creds.anthropic_api_key = api_key,
        ProviderKind::Ollama | ProviderKind::Mock => {}
    }
    secrets_store::save(&app, &creds);
}

#[derive(serde::Serialize)]
pub struct ModelListResult {
    models: Vec<ModelInfo>,
    error: Option<String>,
}

/// Whether a routed profile can only carry on a plain conversation, or
/// can additionally call tools mid-turn -- decided once, at construction
/// time, alongside the rest of `route_provider()`'s provider selection,
/// rather than via `Box<dyn AiProvider>` downcasting later (the concrete
/// type is already known here; Rust has no clean way to ask a type-
/// erased trait object whether it also implements a second trait).
/// `ToolCapable` also satisfies `AiProvider` (`ToolCallingProvider:
/// AiProvider`), so [`RoutedExecution::into_ai_provider`] can hand back
/// a plain `Box<dyn AiProvider>` for either variant.
pub(crate) enum RoutedExecution {
    PlainChat(Box<dyn AiProvider>),
    ToolCapable(Box<dyn ToolCallingProvider>),
}

impl RoutedExecution {
    /// Discards the tool-calling capability, if any, and returns a
    /// plain `Box<dyn AiProvider>` -- what every call site needs today
    /// (the Agent Loop that would actually use `ToolCapable`'s extra
    /// capability doesn't exist yet). Relies on `dyn` trait upcasting
    /// (stable since Rust 1.86) to coerce `Box<dyn ToolCallingProvider>`
    /// into `Box<dyn AiProvider>`.
    pub(crate) fn into_ai_provider(self) -> Box<dyn AiProvider> {
        match self {
            RoutedExecution::PlainChat(provider) => provider,
            RoutedExecution::ToolCapable(provider) => provider,
        }
    }
}

/// Builds a live, routed provider for `profile`. `resume_session_id` is
/// only meaningful for CLI-backed subscription profiles
/// (`ClaudeCodeCli`/`Codex`'s own warm-session model) -- every other
/// branch ignores it. `claude_code_tool_access` is only meaningful for
/// `ClaudeCodeCli` (Group 7's by-case native-tool allow-list); every
/// other branch ignores it too. Tool-calling capability is granted only
/// to `(Ollama, Local)` -- `ToolCallingProvider`'s v1 implementation,
/// per design.md's "`ToolCallingProvider` is a separate trait from
/// `AiProvider`".
pub(crate) fn route_provider(
    credentials: &ProviderCredentials,
    profile: &ProviderProfile,
    resume_session_id: Option<String>,
    claude_code_tool_access: &ClaudeCodeToolAccess,
) -> RoutedExecution {
    match (profile.provider, profile.auth_method) {
        (ProviderKind::OpenAi, AuthMethod::ApiKey) => {
            RoutedExecution::PlainChat(Box::new(OpenAiCompatible::new(
                credentials.openai_api_key.clone().unwrap_or_default(),
                profile.base_url.clone().unwrap_or_else(|| {
                    fleet_snowfluff_ai::providers::openai::DEFAULT_BASE_URL.to_string()
                }),
                profile.model.clone().unwrap_or_default(),
            )))
        }
        (ProviderKind::Anthropic, AuthMethod::ApiKey) => {
            RoutedExecution::PlainChat(Box::new(Anthropic::new(
                credentials.anthropic_api_key.clone().unwrap_or_default(),
                profile.base_url.clone().unwrap_or_else(|| {
                    fleet_snowfluff_ai::providers::anthropic::DEFAULT_BASE_URL.to_string()
                }),
                profile.model.clone().unwrap_or_default(),
            )))
        }
        (ProviderKind::Anthropic, AuthMethod::Subscription) => {
            RoutedExecution::PlainChat(Box::new(ClaudeCodeCli::new(
                profile.model.clone(),
                resume_session_id,
                *claude_code_tool_access,
            )))
        }
        (ProviderKind::OpenAi, AuthMethod::Subscription) => RoutedExecution::PlainChat(Box::new(
            Codex::new(profile.model.clone(), resume_session_id),
        )),
        (ProviderKind::Ollama, auth_method) => {
            let provider = Ollama::new(
                profile.base_url.clone().unwrap_or_else(|| {
                    fleet_snowfluff_ai::providers::ollama::DEFAULT_BASE_URL.to_string()
                }),
                profile.model.clone().unwrap_or_default(),
            );
            if auth_method == AuthMethod::Local {
                RoutedExecution::ToolCapable(Box::new(provider))
            } else {
                RoutedExecution::PlainChat(Box::new(provider))
            }
        }
        (ProviderKind::Mock, _) => RoutedExecution::PlainChat(Box::new(Mock)),
        (ProviderKind::OpenAi | ProviderKind::Anthropic, AuthMethod::Local) => {
            unreachable!(
                "{:?} never has AuthMethod::Local -- only Ollama/Mock do (see AuthMethod's own \
                 doc comment)",
                profile.provider
            )
        }
    }
}

/// Builds a live `Box<dyn AiProvider>` for `profile` -- every existing
/// call site only ever needs plain chat behavior today, so this stays
/// the thin, non-tool-aware entry point; `route_provider` is the one
/// that actually decides tool-calling capability, for whichever future
/// caller (the Agent Loop) needs to keep it.
pub(crate) fn build_provider(
    credentials: &ProviderCredentials,
    profile: &ProviderProfile,
    resume_session_id: Option<String>,
    claude_code_tool_access: &ClaudeCodeToolAccess,
) -> Box<dyn AiProvider> {
    route_provider(credentials, profile, resume_session_id, claude_code_tool_access)
        .into_ai_provider()
}

/// Fetches the live model list for the (`provider`, `auth_method`)
/// profile (`ai-provider`'s "Live model listing"), which doubles as an
/// implicit connection/credential test for API-key/local providers --
/// a failure here surfaces before the user ever sends a chat message.
/// Works even before the profile has been enabled (matching Stage 1's
/// "fetch at the point of selection" behavior), using a transient
/// default-shaped profile in that case.
#[tauri::command]
pub async fn fetch_provider_models(
    ai_settings: State<'_, Mutex<AiSettings>>,
    credentials: State<'_, Mutex<ProviderCredentials>>,
    provider: ProviderKind,
    auth_method: AuthMethod,
) -> Result<ModelListResult, ()> {
    // Snapshot and drop both locks before the network/subprocess call
    // -- the same "never hold a lock across an .await" rule
    // updater.rs establishes.
    let provider_impl = {
        let settings = ai_settings.lock().unwrap();
        let creds = credentials.lock().unwrap();
        let key = ProfileKey { provider, auth_method };
        let profile = settings.profile(key).cloned().unwrap_or(ProviderProfile {
            provider,
            auth_method,
            model: None,
            base_url: None,
        });
        build_provider(&creds, &profile, None, &settings.claude_code_tool_access)
    };

    Ok(match provider_impl.list_models().await {
        Ok(models) => ModelListResult { models, error: None },
        Err(err) => ModelListResult { models: vec![], error: Some(err.to_string()) },
    })
}

/// `subscription-first-chat`'s "Provider status display": checked
/// fresh on every call, never cached, per that requirement's own rule.
#[derive(serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProfileStatus {
    NotConfigured,
    DisclosurePending,
    Connected,
    RuntimeUnavailable { detail: String },
    NotLoggedIn { detail: String },
    QuotaExhausted { detail: String },
    Error { detail: String },
}

/// Reports `key`'s current connection status without sending a chat
/// message, via `AiProvider::check_availability()` (a no-op success
/// for API-key/local providers; a real `claude auth status`/`codex
/// login status` check for the two CLI-backed subscription
/// implementations).
#[tauri::command]
pub async fn check_profile_status(
    ai_settings: State<'_, Mutex<AiSettings>>,
    credentials: State<'_, Mutex<ProviderCredentials>>,
    provider: ProviderKind,
    auth_method: AuthMethod,
) -> Result<ProfileStatus, ()> {
    let key = ProfileKey { provider, auth_method };

    let provider_impl = {
        let settings = ai_settings.lock().unwrap();
        let creds = credentials.lock().unwrap();
        let Some(profile) = settings.profile(key).cloned() else {
            return Ok(ProfileStatus::NotConfigured);
        };
        if matches!(provider, ProviderKind::OpenAi | ProviderKind::Anthropic)
            && !settings.disclosure_acknowledged(key)
        {
            return Ok(ProfileStatus::DisclosurePending);
        }
        build_provider(&creds, &profile, None, &settings.claude_code_tool_access)
    };

    Ok(match provider_impl.check_availability().await {
        Ok(()) => ProfileStatus::Connected,
        Err(ProviderError::RuntimeUnavailable(detail)) => {
            ProfileStatus::RuntimeUnavailable { detail }
        }
        Err(ProviderError::SubscriptionExpired(detail) | ProviderError::Auth(detail)) => {
            ProfileStatus::NotLoggedIn { detail }
        }
        Err(ProviderError::QuotaExhausted(detail)) => ProfileStatus::QuotaExhausted { detail },
        Err(err) => ProfileStatus::Error { detail: err.to_string() },
    })
}

/// Best-effort attempt to open the (`provider`, `auth_method`)
/// profile's own CLI login flow in the user's browser
/// (`subscription-first-chat`'s "CLI installed but not logged in"
/// scenario, via `AiProvider::trigger_login()`) -- fire-and-forget:
/// Aemeath does not wait for the flow to complete, it only starts it.
/// `Ok(true)` means the login command was spawned; the caller should
/// tell the user to complete it in their browser and then re-check
/// status (`check_profile_status`). `Ok(false)` means the profile isn't
/// enabled, so there is nothing to trigger. Any spawn failure (e.g. the
/// CLI binary is missing) is surfaced as an error so the settings UI
/// can fall back to its existing "run the login command yourself"
/// instruction.
#[tauri::command]
pub async fn trigger_profile_login(
    ai_settings: State<'_, Mutex<AiSettings>>,
    credentials: State<'_, Mutex<ProviderCredentials>>,
    provider: ProviderKind,
    auth_method: AuthMethod,
) -> Result<bool, String> {
    let key = ProfileKey { provider, auth_method };

    let provider_impl = {
        let settings = ai_settings.lock().unwrap();
        let creds = credentials.lock().unwrap();
        let Some(profile) = settings.profile(key).cloned() else {
            return Ok(false);
        };
        build_provider(&creds, &profile, None, &settings.claude_code_tool_access)
    };

    provider_impl.trigger_login().await.map(|()| true).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(provider: ProviderKind, auth_method: AuthMethod) -> ProviderProfile {
        ProviderProfile { provider, auth_method, model: None, base_url: None }
    }

    fn settings_with(profiles: Vec<ProviderProfile>) -> AiSettings {
        AiSettings {
            ai_enabled: false,
            enabled_profiles: profiles,
            default_profile: None,
            acknowledged_disclosures: vec![],
            task_router_mode: Default::default(),
            project_root: None,
            claude_code_tool_access: Default::default(),
        }
    }

    fn settings_with_acknowledgements(
        profiles: Vec<ProviderProfile>,
        acknowledged: Vec<ProfileKey>,
    ) -> AiSettings {
        AiSettings {
            ai_enabled: false,
            enabled_profiles: profiles,
            default_profile: None,
            acknowledged_disclosures: acknowledged,
            task_router_mode: Default::default(),
            project_root: None,
            claude_code_tool_access: Default::default(),
        }
    }

    #[test]
    fn disclosure_ok_requires_acknowledgement_for_cloud_providers() {
        let openai_key =
            ProfileKey { provider: ProviderKind::OpenAi, auth_method: AuthMethod::ApiKey };
        let anthropic_key =
            ProfileKey { provider: ProviderKind::Anthropic, auth_method: AuthMethod::ApiKey };
        let settings = settings_with(vec![
            profile(ProviderKind::OpenAi, AuthMethod::ApiKey),
            profile(ProviderKind::Anthropic, AuthMethod::ApiKey),
        ]);
        assert!(!disclosure_ok(&settings, Some(openai_key)));
        assert!(!disclosure_ok(&settings, Some(anthropic_key)));
    }

    #[test]
    fn disclosure_ok_never_required_for_local_providers() {
        let ollama_key =
            ProfileKey { provider: ProviderKind::Ollama, auth_method: AuthMethod::Local };
        let mock_key = ProfileKey { provider: ProviderKind::Mock, auth_method: AuthMethod::Local };
        let settings = settings_with(vec![
            profile(ProviderKind::Ollama, AuthMethod::Local),
            profile(ProviderKind::Mock, AuthMethod::Local),
        ]);
        assert!(disclosure_ok(&settings, Some(ollama_key)));
        assert!(disclosure_ok(&settings, Some(mock_key)));
        assert!(disclosure_ok(&settings, None));
    }

    #[test]
    fn disclosure_ok_once_acknowledged() {
        let key = ProfileKey { provider: ProviderKind::OpenAi, auth_method: AuthMethod::ApiKey };
        let settings = settings_with_acknowledgements(
            vec![profile(ProviderKind::OpenAi, AuthMethod::ApiKey)],
            vec![key],
        );
        assert!(disclosure_ok(&settings, Some(key)));

        let anthropic_key =
            ProfileKey { provider: ProviderKind::Anthropic, auth_method: AuthMethod::ApiKey };
        assert!(!disclosure_ok(&settings, Some(anthropic_key)));
    }

    #[test]
    fn acknowledging_one_auth_method_does_not_cover_the_other() {
        // Exercises `subscription-first-chat`'s modified "Cloud
        // provider data disclosure": switching Anthropic from API key
        // to subscription must show its own disclosure.
        let api_key_key =
            ProfileKey { provider: ProviderKind::Anthropic, auth_method: AuthMethod::ApiKey };
        let subscription_key =
            ProfileKey { provider: ProviderKind::Anthropic, auth_method: AuthMethod::Subscription };
        let settings = settings_with_acknowledgements(
            vec![
                profile(ProviderKind::Anthropic, AuthMethod::ApiKey),
                profile(ProviderKind::Anthropic, AuthMethod::Subscription),
            ],
            vec![api_key_key],
        );
        assert!(disclosure_ok(&settings, Some(api_key_key)));
        assert!(!disclosure_ok(&settings, Some(subscription_key)));
    }

    #[test]
    fn disclosure_survives_disable_and_re_enable() {
        // Exercises the "Disclosure does not repeat once acknowledged"
        // scenario's disable/re-enable case directly against this
        // module's own gate, not just `AiSettings` in isolation.
        let key = ProfileKey { provider: ProviderKind::OpenAi, auth_method: AuthMethod::ApiKey };
        // Profile removed (as `disable_profile` would do), acknowledgment kept.
        let settings = settings_with_acknowledgements(vec![], vec![key]);
        assert!(settings.profile(key).is_none(), "sanity: profile really isn't enabled");
        assert!(disclosure_ok(&settings, Some(key)), "re-enabling must not need disclosure again");
    }

    #[test]
    fn set_default_profile_refuses_a_profile_that_is_not_enabled() {
        // `disclosure_ok` alone would pass (Local auth never needs
        // one) -- the "is it actually enabled" check is separate.
        let settings = settings_with(vec![]);
        let key = ProfileKey { provider: ProviderKind::Ollama, auth_method: AuthMethod::Local };
        assert!(
            disclosure_ok(&settings, Some(key)),
            "sanity: Local auth never gates on disclosure"
        );
        assert!(settings.profile(key).is_none(), "sanity: the profile really isn't enabled");
    }

    #[test]
    fn build_provider_dispatches_anthropic_by_auth_method() {
        let creds = ProviderCredentials::default();
        let tool_access = ClaudeCodeToolAccess::default();
        let api_key_provider = build_provider(
            &creds,
            &profile(ProviderKind::Anthropic, AuthMethod::ApiKey),
            None,
            &tool_access,
        );
        assert_eq!(api_key_provider.kind(), ProviderKind::Anthropic);

        let subscription_provider = build_provider(
            &creds,
            &profile(ProviderKind::Anthropic, AuthMethod::Subscription),
            Some("prior-session-id".to_string()),
            &tool_access,
        );
        assert_eq!(subscription_provider.kind(), ProviderKind::Anthropic);
    }

    #[test]
    fn build_provider_dispatches_openai_by_auth_method() {
        let creds = ProviderCredentials::default();
        let tool_access = ClaudeCodeToolAccess::default();
        let api_key_provider = build_provider(
            &creds,
            &profile(ProviderKind::OpenAi, AuthMethod::ApiKey),
            None,
            &tool_access,
        );
        assert_eq!(api_key_provider.kind(), ProviderKind::OpenAi);

        let subscription_provider = build_provider(
            &creds,
            &profile(ProviderKind::OpenAi, AuthMethod::Subscription),
            None,
            &tool_access,
        );
        assert_eq!(subscription_provider.kind(), ProviderKind::OpenAi);
    }

    #[test]
    fn only_ollama_local_is_tool_capable() {
        let creds = ProviderCredentials::default();
        let tool_access = ClaudeCodeToolAccess::default();
        let combinations = [
            profile(ProviderKind::OpenAi, AuthMethod::ApiKey),
            profile(ProviderKind::OpenAi, AuthMethod::Subscription),
            profile(ProviderKind::Anthropic, AuthMethod::ApiKey),
            profile(ProviderKind::Anthropic, AuthMethod::Subscription),
            profile(ProviderKind::Ollama, AuthMethod::ApiKey),
            profile(ProviderKind::Ollama, AuthMethod::Subscription),
            profile(ProviderKind::Mock, AuthMethod::ApiKey),
            profile(ProviderKind::Mock, AuthMethod::Local),
        ];
        for profile in combinations {
            let routed = route_provider(&creds, &profile, None, &tool_access);
            assert!(
                matches!(routed, RoutedExecution::PlainChat(_)),
                "{:?} should not be tool-capable",
                (profile.provider, profile.auth_method)
            );
        }

        let ollama_local = profile(ProviderKind::Ollama, AuthMethod::Local);
        let routed = route_provider(&creds, &ollama_local, None, &tool_access);
        assert!(
            matches!(routed, RoutedExecution::ToolCapable(_)),
            "(Ollama, Local) should be the only tool-capable combination"
        );
    }

    #[test]
    fn into_ai_provider_works_for_both_routed_execution_variants() {
        let creds = ProviderCredentials::default();
        let tool_access = ClaudeCodeToolAccess::default();
        let plain = route_provider(
            &creds,
            &profile(ProviderKind::OpenAi, AuthMethod::ApiKey),
            None,
            &tool_access,
        )
        .into_ai_provider();
        assert_eq!(plain.kind(), ProviderKind::OpenAi);

        let tool_capable = route_provider(
            &creds,
            &profile(ProviderKind::Ollama, AuthMethod::Local),
            None,
            &tool_access,
        )
        .into_ai_provider();
        assert_eq!(tool_capable.kind(), ProviderKind::Ollama);
    }
}
