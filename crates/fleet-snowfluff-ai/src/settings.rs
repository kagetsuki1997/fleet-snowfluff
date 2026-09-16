//! `AiSettings`: the shape of `ai-config.json` (non-secret provider
//! settings, the master switch, and the active-provider pointer).
//! Sanitized field-by-field from loosely-typed JSON, same philosophy
//! as `fleet-snowfluff-core::config::sanitize` -- unlike persona
//! parsing, a broken field here degrades gracefully rather than
//! discarding the whole file.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::ProviderKind;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OpenAiSettings {
    /// `None` means "use the provider's own default base URL" --
    /// distinguishing "not set" from "explicitly the default" so a
    /// future default-URL change doesn't silently strand users who
    /// never touched this field.
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Per-provider persisted acknowledgment of the cloud-provider
    /// data disclosure (`ai-provider`'s "Cloud provider data
    /// disclosure") -- switching away and back must not show it again.
    #[serde(default)]
    pub disclosure_acknowledged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AnthropicSettings {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub disclosure_acknowledged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OllamaSettings {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    // No disclosure field: Ollama is local-only, no data ever leaves
    // the device, so there's nothing to acknowledge.
}

/// Nothing to configure -- kept as a real (empty) struct rather than
/// omitting Mock's settings entirely, so the four providers stay
/// structurally uniform in `AiSettings`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MockSettings {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSettings {
    /// Master switch (`ai-provider`'s "AI features disabled by
    /// default"): even a fully configured provider does nothing while
    /// this is `false`. `bool`'s own `Default` (`false`) is exactly the
    /// value this needs, so `#[derive(Default)]` above is correct as-is
    /// -- no manual impl needed.
    #[serde(default)]
    pub ai_enabled: bool,
    /// `None` means "no provider configured" (`ai-provider`'s "No
    /// provider configured by default") -- the default on a fresh
    /// install, deliberately never defaulting to a cloud provider.
    #[serde(default)]
    pub active_provider: Option<ProviderKind>,
    #[serde(default)]
    pub openai: OpenAiSettings,
    #[serde(default)]
    pub anthropic: AnthropicSettings,
    #[serde(default)]
    pub ollama: OllamaSettings,
    #[serde(default)]
    pub mock: MockSettings,
}

fn sub_settings<T: Default + for<'de> Deserialize<'de>>(value: Option<&Value>) -> T {
    value.and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
}

/// Sanitizes a raw config JSON value into a fully-valid `AiSettings`,
/// same field-by-field-degrades-gracefully approach as
/// `fleet-snowfluff-core::config::sanitize`: an unexpected type or a
/// missing field falls back to that field's default rather than
/// discarding the whole file (unlike persona parsing, which is
/// deliberately all-or-nothing).
pub fn sanitize(raw: &Value) -> AiSettings {
    let obj = raw.as_object();
    let get = |key: &str| obj.and_then(|o| o.get(key));

    AiSettings {
        ai_enabled: get("ai_enabled").and_then(Value::as_bool).unwrap_or(false),
        active_provider: get("active_provider")
            .and_then(|v| serde_json::from_value::<ProviderKind>(v.clone()).ok()),
        openai: sub_settings(get("openai")),
        anthropic: sub_settings(get("anthropic")),
        ollama: sub_settings(get("ollama")),
        mock: MockSettings::default(),
    }
}

/// Loads settings from raw file contents. Missing/unreadable/corrupt
/// content yields full defaults (`ai_enabled: false`, no active
/// provider), matching `config::load_from_str`'s precedent.
pub fn load_from_str(contents: &str) -> AiSettings {
    match serde_json::from_str::<Value>(contents) {
        Ok(value) => sanitize(&value),
        Err(_) => AiSettings::default(),
    }
}

pub fn to_json_string(settings: &AiSettings) -> String {
    serde_json::to_string_pretty(settings).expect("AiSettings serialization is infallible")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_are_disabled_with_no_provider_configured() {
        let settings = AiSettings::default();
        assert!(!settings.ai_enabled);
        assert_eq!(settings.active_provider, None);
    }

    #[test]
    fn missing_file_content_yields_defaults() {
        let settings = load_from_str("");
        assert!(!settings.ai_enabled);
        assert_eq!(settings.active_provider, None);
    }

    #[test]
    fn corrupt_json_yields_defaults() {
        let settings = load_from_str("{not valid json");
        assert_eq!(settings, AiSettings::default());
    }

    #[test]
    fn partial_json_fills_missing_fields_with_defaults() {
        let raw = json!({ "ai_enabled": true });
        let settings = sanitize(&raw);
        assert!(settings.ai_enabled);
        assert_eq!(settings.active_provider, None);
        assert_eq!(settings.openai, OpenAiSettings::default());
    }

    #[test]
    fn active_provider_round_trips() {
        let raw = json!({ "ai_enabled": true, "active_provider": "anthropic" });
        let settings = sanitize(&raw);
        assert_eq!(settings.active_provider, Some(ProviderKind::Anthropic));
    }

    #[test]
    fn invalid_active_provider_falls_back_to_none_not_a_crash() {
        let raw = json!({ "active_provider": "not_a_real_provider" });
        let settings = sanitize(&raw);
        assert_eq!(settings.active_provider, None);
    }

    #[test]
    fn per_provider_settings_are_preserved_when_switching_active_provider() {
        // Exercises ai-provider's "Independent per-provider
        // configuration": every provider's settings are stored
        // simultaneously, not overwritten when a different one becomes
        // active.
        let raw = json!({
            "active_provider": "ollama",
            "openai": { "model": "gpt-4o-mini", "disclosure_acknowledged": true },
            "ollama": { "model": "llama3.2:3b" },
        });
        let settings = sanitize(&raw);
        assert_eq!(settings.active_provider, Some(ProviderKind::Ollama));
        assert_eq!(settings.openai.model.as_deref(), Some("gpt-4o-mini"));
        assert!(settings.openai.disclosure_acknowledged);
        assert_eq!(settings.ollama.model.as_deref(), Some("llama3.2:3b"));
    }

    #[test]
    fn round_trips_through_json() {
        let settings = AiSettings {
            ai_enabled: true,
            active_provider: Some(ProviderKind::OpenAi),
            openai: OpenAiSettings {
                model: Some("gpt-4o".to_string()),
                disclosure_acknowledged: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let json_str = to_json_string(&settings);
        let reloaded = load_from_str(&json_str);
        assert_eq!(reloaded, settings);
    }
}
