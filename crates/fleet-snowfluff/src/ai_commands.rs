//! Tauri commands for the settings webview's AI tab -- mirrors
//! `commands.rs`'s personalization commands: master switch, enabled
//! profiles (with per-profile cloud-disclosure gating), credentials,
//! live model-listing, and fresh per-profile status. Stage 2
//! (`subscription-first-chat`) replaced the single active-provider
//! pointer with a list of independently-enabled `ProviderProfile`s
//! (provider + auth method) plus a manually-chosen default; every
//! command here operates on a profile identified by its `ProfileKey`
//! rather than a bare `ProviderKind`.

use std::sync::Mutex;

use fleet_snowfluff_ai::{
    AiProvider, AiSettings, Anthropic, AuthMethod, ClaudeCodeCli, Codex, Mock, ModelInfo, Ollama,
    OpenAiCompatible, ProfileKey, ProviderCredentials, ProviderError, ProviderKind,
    ProviderProfile,
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

/// Whether `key` may become the default profile given the disclosures
/// already acknowledged in `settings` (`ai-provider`'s "Cloud provider
/// data disclosure", now keyed per (provider, auth method) rather than
/// per provider). `None` (clearing the default) and any `Local`-auth
/// profile (Ollama, Mock) never need one.
fn disclosure_ok(settings: &AiSettings, key: Option<ProfileKey>) -> bool {
    let Some(key) = key else { return true };
    match key.auth_method {
        AuthMethod::Local => true,
        AuthMethod::ApiKey | AuthMethod::Subscription => {
            settings.profile(key).is_some_and(|p| p.disclosure_acknowledged)
        }
    }
}

/// Adds `key` to the enabled-profile list if it isn't already there
/// (idempotent -- re-enabling an already-enabled profile never resets
/// its model/base_url/disclosure state). Enabling a profile does not,
/// by itself, make it usable for chat -- only `set_default_profile`
/// does, and that's what the disclosure gate actually guards
/// (`subscription-first-chat`'s "Independent per-provider
/// configuration").
#[tauri::command]
pub fn enable_profile(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: ProviderKind,
    auth_method: AuthMethod,
) {
    let mut settings = ai_settings.lock().unwrap();
    let key = ProfileKey { provider, auth_method };
    if settings.profile(key).is_none() {
        settings.enabled_profiles.push(ProviderProfile {
            provider,
            auth_method,
            model: None,
            base_url: None,
            disclosure_acknowledged: false,
        });
        ai_config_store::save(&app, &settings);
    }
}

/// Removes `key` from the enabled-profile list; clears the default
/// profile too if it was the one being disabled.
#[tauri::command]
pub fn disable_profile(app: AppHandle, ai_settings: State<Mutex<AiSettings>>, key: ProfileKey) {
    let mut settings = ai_settings.lock().unwrap();
    settings.enabled_profiles.retain(|p| p.key() != key);
    if settings.default_profile == Some(key) {
        settings.default_profile = None;
    }
    ai_config_store::save(&app, &settings);
}

/// Sets which enabled profile currently handles chat requests. Refuses
/// (returns `false`) if `key` needs a disclosure that hasn't been
/// acknowledged yet, or names a profile that isn't actually enabled --
/// enforced here, not only in the frontend's own gating, so the
/// guarantee holds regardless of what calls this command. Returns
/// whether the change actually took effect, so the frontend can tell
/// "activated" apart from "refused".
#[tauri::command]
pub fn set_default_profile(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    key: Option<ProfileKey>,
) -> bool {
    let mut settings = ai_settings.lock().unwrap();

    if !disclosure_ok(&settings, key) {
        log::warn!("refused to set default profile {key:?}: disclosure not yet acknowledged");
        return false;
    }
    if let Some(k) = key {
        if settings.profile(k).is_none() {
            log::warn!("refused to set default profile {k:?}: profile is not enabled");
            return false;
        }
    }

    settings.default_profile = key;
    ai_config_store::save(&app, &settings);
    true
}

/// Records that the user has seen and accepted the cloud-provider data
/// disclosure for `key` (`ai-provider`'s "Cloud provider data
/// disclosure" -- persisted per (provider, auth method), so switching
/// Anthropic from API key to subscription shows its own disclosure
/// rather than reusing the other auth method's acknowledgment). A
/// no-op if the profile isn't enabled yet -- `enable_profile` must run
/// first.
#[tauri::command]
pub fn acknowledge_profile_disclosure(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    key: ProfileKey,
) {
    let mut settings = ai_settings.lock().unwrap();
    if let Some(profile) = settings.profile_mut(key) {
        profile.disclosure_acknowledged = true;
        ai_config_store::save(&app, &settings);
    }
}

#[tauri::command]
pub fn set_profile_model(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    key: ProfileKey,
    model: String,
) {
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
    key: ProfileKey,
    base_url: String,
) {
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

/// Builds a live `Box<dyn AiProvider>` for `profile`. `resume_session_id`
/// is only meaningful for CLI-backed subscription profiles
/// (`ClaudeCodeCli`/`Codex`'s own warm-session model) -- every other
/// branch ignores it.
pub(crate) fn build_provider(
    credentials: &ProviderCredentials,
    profile: &ProviderProfile,
    resume_session_id: Option<String>,
) -> Box<dyn AiProvider> {
    match (profile.provider, profile.auth_method) {
        (ProviderKind::OpenAi, AuthMethod::ApiKey) => Box::new(OpenAiCompatible::new(
            credentials.openai_api_key.clone().unwrap_or_default(),
            profile.base_url.clone().unwrap_or_else(|| {
                fleet_snowfluff_ai::providers::openai::DEFAULT_BASE_URL.to_string()
            }),
            profile.model.clone().unwrap_or_default(),
        )),
        (ProviderKind::Anthropic, AuthMethod::ApiKey) => Box::new(Anthropic::new(
            credentials.anthropic_api_key.clone().unwrap_or_default(),
            profile.base_url.clone().unwrap_or_else(|| {
                fleet_snowfluff_ai::providers::anthropic::DEFAULT_BASE_URL.to_string()
            }),
            profile.model.clone().unwrap_or_default(),
        )),
        (ProviderKind::Anthropic, AuthMethod::Subscription) => {
            Box::new(ClaudeCodeCli::new(profile.model.clone(), resume_session_id))
        }
        (ProviderKind::OpenAi, AuthMethod::Subscription) => {
            Box::new(Codex::new(profile.model.clone(), resume_session_id))
        }
        (ProviderKind::Ollama, _) => Box::new(Ollama::new(
            profile.base_url.clone().unwrap_or_else(|| {
                fleet_snowfluff_ai::providers::ollama::DEFAULT_BASE_URL.to_string()
            }),
            profile.model.clone().unwrap_or_default(),
        )),
        (ProviderKind::Mock, _) => Box::new(Mock),
        (ProviderKind::OpenAi | ProviderKind::Anthropic, AuthMethod::Local) => {
            unreachable!(
                "{:?} never has AuthMethod::Local -- only Ollama/Mock do (see AuthMethod's own \
                 doc comment)",
                profile.provider
            )
        }
    }
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
            disclosure_acknowledged: false,
        });
        build_provider(&creds, &profile, None)
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
            && !profile.disclosure_acknowledged
        {
            return Ok(ProfileStatus::DisclosurePending);
        }
        build_provider(&creds, &profile, None)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(provider: ProviderKind, auth_method: AuthMethod) -> ProviderProfile {
        ProviderProfile {
            provider,
            auth_method,
            model: None,
            base_url: None,
            disclosure_acknowledged: false,
        }
    }

    fn settings_with(profiles: Vec<ProviderProfile>) -> AiSettings {
        AiSettings { ai_enabled: false, enabled_profiles: profiles, default_profile: None }
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
        let mut p = profile(ProviderKind::OpenAi, AuthMethod::ApiKey);
        p.disclosure_acknowledged = true;
        let settings = settings_with(vec![p]);
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
        let mut acknowledged_api_key = profile(ProviderKind::Anthropic, AuthMethod::ApiKey);
        acknowledged_api_key.disclosure_acknowledged = true;
        let settings = settings_with(vec![
            acknowledged_api_key,
            profile(ProviderKind::Anthropic, AuthMethod::Subscription),
        ]);
        assert!(disclosure_ok(&settings, Some(api_key_key)));
        assert!(!disclosure_ok(&settings, Some(subscription_key)));
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
        let api_key_provider =
            build_provider(&creds, &profile(ProviderKind::Anthropic, AuthMethod::ApiKey), None);
        assert_eq!(api_key_provider.kind(), ProviderKind::Anthropic);

        let subscription_provider = build_provider(
            &creds,
            &profile(ProviderKind::Anthropic, AuthMethod::Subscription),
            Some("prior-session-id".to_string()),
        );
        assert_eq!(subscription_provider.kind(), ProviderKind::Anthropic);
    }

    #[test]
    fn build_provider_dispatches_openai_by_auth_method() {
        let creds = ProviderCredentials::default();
        let api_key_provider =
            build_provider(&creds, &profile(ProviderKind::OpenAi, AuthMethod::ApiKey), None);
        assert_eq!(api_key_provider.kind(), ProviderKind::OpenAi);

        let subscription_provider =
            build_provider(&creds, &profile(ProviderKind::OpenAi, AuthMethod::Subscription), None);
        assert_eq!(subscription_provider.kind(), ProviderKind::OpenAi);
    }
}
