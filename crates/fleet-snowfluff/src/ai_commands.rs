//! Tauri commands for the settings webview's AI tab -- mirrors
//! `commands.rs`'s personalization commands: master switch, active
//! provider (with per-provider cloud-disclosure gating), credentials,
//! and live model-listing.

use std::sync::Mutex;

use fleet_snowfluff_ai::{
    AiProvider, AiSettings, Anthropic, Mock, ModelInfo, Ollama, OpenAiCompatible,
    ProviderCredentials, ProviderKind,
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

/// Whether `provider` may become active given the disclosures already
/// acknowledged in `settings` (`ai-provider`'s "Cloud provider data
/// disclosure": "SHALL NOT become active until the disclosure is
/// acknowledged"). Only OpenAI/Anthropic require one -- Ollama and Mock
/// never send data off-device.
fn disclosure_ok(settings: &AiSettings, provider: Option<ProviderKind>) -> bool {
    match provider {
        Some(ProviderKind::OpenAi) => settings.openai.disclosure_acknowledged,
        Some(ProviderKind::Anthropic) => settings.anthropic.disclosure_acknowledged,
        Some(ProviderKind::Ollama | ProviderKind::Mock) | None => true,
    }
}

/// Sets the active provider. Refuses to activate OpenAI/Anthropic
/// until that provider's disclosure has been acknowledged -- enforced
/// here, not only in the frontend's own gating, so the guarantee holds
/// regardless of what calls this command. The normal flow still calls
/// `acknowledge_provider_disclosure` first, as a separate step, only
/// when the user actually accepts the prompt; this is the backstop for
/// everything else. Returns whether the change actually took effect,
/// so the frontend can tell "activated" apart from "refused".
#[tauri::command]
pub fn set_active_provider(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: Option<ProviderKind>,
) -> bool {
    let mut settings = ai_settings.lock().unwrap();

    if !disclosure_ok(&settings, provider) {
        log::warn!("refused to activate {provider:?}: disclosure not yet acknowledged");
        return false;
    }

    settings.active_provider = provider;
    ai_config_store::save(&app, &settings);
    true
}

/// Records that the user has seen and accepted the cloud-provider data
/// disclosure for `provider` (`ai-provider`'s "Cloud provider data
/// disclosure" -- persisted per provider so it never repeats once
/// acknowledged). A no-op for Ollama/Mock, which never show one.
#[tauri::command]
pub fn acknowledge_provider_disclosure(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: ProviderKind,
) {
    let mut settings = ai_settings.lock().unwrap();
    match provider {
        ProviderKind::OpenAi => settings.openai.disclosure_acknowledged = true,
        ProviderKind::Anthropic => settings.anthropic.disclosure_acknowledged = true,
        ProviderKind::Ollama | ProviderKind::Mock => {}
    }
    ai_config_store::save(&app, &settings);
}

#[tauri::command]
pub fn set_provider_model(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: ProviderKind,
    model: String,
) {
    let model = (!model.is_empty()).then_some(model);
    let mut settings = ai_settings.lock().unwrap();
    match provider {
        ProviderKind::OpenAi => settings.openai.model = model,
        ProviderKind::Anthropic => settings.anthropic.model = model,
        ProviderKind::Ollama => settings.ollama.model = model,
        ProviderKind::Mock => {}
    }
    ai_config_store::save(&app, &settings);
}

#[tauri::command]
pub fn set_provider_base_url(
    app: AppHandle,
    ai_settings: State<Mutex<AiSettings>>,
    provider: ProviderKind,
    base_url: String,
) {
    let base_url = (!base_url.is_empty()).then_some(base_url);
    let mut settings = ai_settings.lock().unwrap();
    match provider {
        ProviderKind::OpenAi => settings.openai.base_url = base_url,
        ProviderKind::Anthropic => settings.anthropic.base_url = base_url,
        ProviderKind::Ollama => settings.ollama.base_url = base_url,
        ProviderKind::Mock => {}
    }
    ai_config_store::save(&app, &settings);
}

/// Never round-trips the actual key back to the frontend --
/// `get_ai_settings`'s `openai_key_set`/`anthropic_key_set` booleans
/// are the only thing the webview ever learns about a saved key.
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

pub(crate) fn build_provider(
    settings: &AiSettings,
    credentials: &ProviderCredentials,
    provider: ProviderKind,
) -> Box<dyn AiProvider> {
    match provider {
        ProviderKind::OpenAi => Box::new(OpenAiCompatible::new(
            credentials.openai_api_key.clone().unwrap_or_default(),
            settings.openai.base_url.clone().unwrap_or_else(|| {
                fleet_snowfluff_ai::providers::openai::DEFAULT_BASE_URL.to_string()
            }),
            settings.openai.model.clone().unwrap_or_default(),
        )),
        ProviderKind::Anthropic => Box::new(Anthropic::new(
            credentials.anthropic_api_key.clone().unwrap_or_default(),
            settings.anthropic.base_url.clone().unwrap_or_else(|| {
                fleet_snowfluff_ai::providers::anthropic::DEFAULT_BASE_URL.to_string()
            }),
            settings.anthropic.model.clone().unwrap_or_default(),
        )),
        ProviderKind::Ollama => Box::new(Ollama::new(
            settings.ollama.base_url.clone().unwrap_or_else(|| {
                fleet_snowfluff_ai::providers::ollama::DEFAULT_BASE_URL.to_string()
            }),
            settings.ollama.model.clone().unwrap_or_default(),
        )),
        ProviderKind::Mock => Box::new(Mock),
    }
}

/// Fetches the live model list for `provider` (`ai-provider`'s "Live
/// model listing"), which doubles as an implicit connection/credential
/// test -- a failure here surfaces before the user ever sends a chat
/// message, per that requirement's own scenario.
#[tauri::command]
pub async fn fetch_provider_models(
    ai_settings: State<'_, Mutex<AiSettings>>,
    credentials: State<'_, Mutex<ProviderCredentials>>,
    provider: ProviderKind,
) -> Result<ModelListResult, ()> {
    // Snapshot and drop both locks before the network call -- the same
    // "never hold a lock across an .await" rule updater.rs establishes.
    let provider_impl = {
        let settings = ai_settings.lock().unwrap();
        let creds = credentials.lock().unwrap();
        build_provider(&settings, &creds, provider)
    };

    Ok(match provider_impl.list_models().await {
        Ok(models) => ModelListResult { models, error: None },
        Err(err) => ModelListResult { models: vec![], error: Some(err.to_string()) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disclosure_ok_requires_acknowledgement_for_cloud_providers() {
        let settings = AiSettings::default();
        assert!(!disclosure_ok(&settings, Some(ProviderKind::OpenAi)));
        assert!(!disclosure_ok(&settings, Some(ProviderKind::Anthropic)));
    }

    #[test]
    fn disclosure_ok_never_required_for_local_providers() {
        let settings = AiSettings::default();
        assert!(disclosure_ok(&settings, Some(ProviderKind::Ollama)));
        assert!(disclosure_ok(&settings, Some(ProviderKind::Mock)));
        assert!(disclosure_ok(&settings, None));
    }

    #[test]
    fn disclosure_ok_once_acknowledged() {
        let mut settings = AiSettings::default();
        settings.openai.disclosure_acknowledged = true;
        assert!(disclosure_ok(&settings, Some(ProviderKind::OpenAi)));
        assert!(!disclosure_ok(&settings, Some(ProviderKind::Anthropic)));
    }
}
