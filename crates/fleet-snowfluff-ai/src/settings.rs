//! `AiSettings`: the shape of `ai-config.json` (non-secret provider
//! settings, the master switch, and the enabled provider profiles).
//! Sanitized field-by-field from loosely-typed JSON, same philosophy
//! as `fleet-snowfluff-core::config::sanitize` -- unlike persona
//! parsing, a broken field here degrades gracefully rather than
//! discarding the whole file.
//!
//! Stage 2 (`subscription-first-chat`) replaced the single
//! `active_provider: Option<ProviderKind>` pointer with a list of
//! enabled `ProviderProfile`s plus a `default_profile` pointer, so more
//! than one provider+auth-method combination can be configured at
//! once. `sanitize` auto-migrates a Stage 1 config (which only ever
//! had one provider, necessarily API-key auth) into exactly one
//! profile -- see `migrate_legacy_active_provider`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::ProviderKind;

/// How a profile authenticates. `Local` covers both Ollama (a local
/// HTTP server, nothing to authenticate) and Mock (nothing at all) --
/// neither shows the cloud-provider disclosure and neither has a
/// meaningful second auth method, so one shared variant is enough
/// rather than a `None`/`Local` split that would just be two names for
/// the same "no auth ceremony" case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    ApiKey,
    /// Reuses the provider's own official CLI login/session
    /// (`claude`/`codex`) rather than a Fleet-managed credential --
    /// see `subscription-first-chat`'s "Subscription auth via the
    /// provider's own CLI" requirement. Deliberately one generic
    /// variant regardless of *how* the underlying implementation talks
    /// to the provider (raw HTTP with a borrowed token for Anthropic,
    /// a wrapped CLI subprocess for OpenAI) -- that distinction is an
    /// implementation detail below `AiProvider`, not something a
    /// profile's shape should expose.
    Subscription,
    Local,
}

/// Identifies a profile by what the user actually configured --
/// provider brand plus auth method -- rather than an arbitrary
/// generated ID. There is never a reason to have two profiles with the
/// same `(provider, auth_method)` pair enabled at once (nothing would
/// distinguish them), so this pair is a stable, meaningful key on its
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileKey {
    pub provider: ProviderKind,
    pub auth_method: AuthMethod,
}

/// One configured, independently-enabled provider profile
/// (`subscription-first-chat`'s "Independent per-provider
/// configuration"). `base_url` is only meaningful for API-key/local
/// auth (a custom OpenAI-compatible endpoint, or Ollama's server
/// address) -- a `Subscription` profile always talks to whatever
/// endpoint its CLI/runtime uses internally, so it stays `None` there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub provider: ProviderKind,
    pub auth_method: AuthMethod,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

impl ProviderProfile {
    pub fn key(&self) -> ProfileKey {
        ProfileKey { provider: self.provider, auth_method: self.auth_method }
    }

    /// `subscription-first-chat`'s "Experimental provider marking":
    /// true for a provider implementation that hasn't been manually
    /// verified end-to-end. Only OpenAI's subscription auth qualifies
    /// today -- `ClaudeCodeCli` (Anthropic, Subscription) *was*
    /// verified live during design, and every API-key/Local profile
    /// reuses Stage 1's already-shipped, already-used implementations.
    /// A profile-level fact, not a provider-instance one, so this lives
    /// here rather than on `AiProvider` -- the settings UI can check it
    /// without ever constructing a provider.
    pub fn is_experimental(&self) -> bool {
        matches!(
            (self.provider, self.auth_method),
            (ProviderKind::OpenAi, AuthMethod::Subscription)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSettings {
    /// Master switch (`ai-provider`'s "AI features disabled by
    /// default"): even a fully configured provider does nothing while
    /// this is `false`. `bool`'s own `Default` (`false`) is exactly the
    /// value this needs, so `#[derive(Default)]` above is correct as-is
    /// -- no manual impl needed.
    #[serde(default)]
    pub ai_enabled: bool,
    /// Every profile the user has enabled, stored independently and
    /// simultaneously (`subscription-first-chat`'s "Independent
    /// per-provider configuration"). Empty on a fresh install --
    /// `ai-provider`'s "No provider configured by default", extended
    /// to profiles.
    #[serde(default)]
    pub enabled_profiles: Vec<ProviderProfile>,
    /// Which enabled profile currently handles chat requests, manually
    /// chosen. `None` when `enabled_profiles` is empty. Task Routing /
    /// Fallback between profiles is out of scope for this change (see
    /// `subscription-first-chat`'s design.md Non-Goals) -- exactly one
    /// profile is ever live at a time, and only a user action changes
    /// which one.
    #[serde(default)]
    pub default_profile: Option<ProfileKey>,
    /// Which (provider, auth method) pairs have had their cloud-provider
    /// data disclosure acknowledged (`ai-provider`'s "Cloud provider
    /// data disclosure", modified per (provider, auth method) by
    /// `subscription-first-chat`). Deliberately tracked here, separate
    /// from `enabled_profiles`, rather than as a field on
    /// `ProviderProfile` itself: a profile is removed from
    /// `enabled_profiles` when disabled, but the *fact* that the user
    /// was already told "this sends your messages to X's servers"
    /// doesn't become untrue just because the profile was toggled off
    /// -- re-enabling the same (provider, auth method) later must not
    /// ask again (this capability's "Disclosure does not repeat once
    /// acknowledged" scenario, which explicitly covers a disable/
    /// re-enable cycle, not just switching away and back while still
    /// enabled). A `ProviderProfile`-level field couldn't express that
    /// without surviving its own removal, which would make "disabled"
    /// and "never configured" indistinguishable in exactly the state
    /// that matters here.
    #[serde(default)]
    pub acknowledged_disclosures: Vec<ProfileKey>,
}

impl AiSettings {
    /// The profile currently handling chat requests, if any.
    pub fn default_profile(&self) -> Option<&ProviderProfile> {
        let key = self.default_profile?;
        self.enabled_profiles.iter().find(|p| p.key() == key)
    }

    pub fn profile(&self, key: ProfileKey) -> Option<&ProviderProfile> {
        self.enabled_profiles.iter().find(|p| p.key() == key)
    }

    /// Whether `key`'s cloud-provider data disclosure has ever been
    /// acknowledged -- independent of whether that profile is currently
    /// enabled (see `acknowledged_disclosures`'s own doc comment).
    pub fn disclosure_acknowledged(&self, key: ProfileKey) -> bool {
        self.acknowledged_disclosures.contains(&key)
    }

    pub fn profile_mut(&mut self, key: ProfileKey) -> Option<&mut ProviderProfile> {
        self.enabled_profiles.iter_mut().find(|p| p.key() == key)
    }
}

fn parse_profile_key(value: &Value) -> Option<ProfileKey> {
    serde_json::from_value(value.clone()).ok()
}

/// Migrates a Stage 1 config (`active_provider` present, no
/// `enabled_profiles` key at all) into exactly one `ProviderProfile`.
/// Every Stage 1 config's provider was necessarily API-key auth --
/// subscription auth didn't exist yet -- so `AuthMethod::ApiKey` (or
/// `Local` for Ollama/Mock) is the only value that could have produced
/// today's file, not a guess. Only the previously *active* provider's
/// settings are carried over; the other three providers' Stage 1
/// settings (stored but inactive) are not preserved as disabled
/// profiles, matching `subscription-first-chat`'s scoped migration
/// plan.
fn migrate_legacy_active_provider(obj: &serde_json::Map<String, Value>) -> AiSettings {
    let get = |key: &str| obj.get(key);

    let active_provider =
        get("active_provider").and_then(|v| serde_json::from_value::<ProviderKind>(v.clone()).ok());

    let Some(provider) = active_provider else {
        return AiSettings {
            ai_enabled: get("ai_enabled").and_then(Value::as_bool).unwrap_or(false),
            enabled_profiles: vec![],
            default_profile: None,
            acknowledged_disclosures: vec![],
        };
    };

    let sub_key = match provider {
        ProviderKind::OpenAi => "openai",
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Ollama => "ollama",
        ProviderKind::Mock => "mock",
    };
    let sub = get(sub_key).and_then(Value::as_object);
    let model = sub.and_then(|s| s.get("model")).and_then(Value::as_str).map(str::to_string);
    let base_url = sub.and_then(|s| s.get("base_url")).and_then(Value::as_str).map(str::to_string);
    let disclosure_acknowledged = sub
        .and_then(|s| s.get("disclosure_acknowledged"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let auth_method = match provider {
        ProviderKind::OpenAi | ProviderKind::Anthropic => AuthMethod::ApiKey,
        ProviderKind::Ollama | ProviderKind::Mock => AuthMethod::Local,
    };

    let profile = ProviderProfile { provider, auth_method, model, base_url };
    let key = profile.key();

    AiSettings {
        ai_enabled: get("ai_enabled").and_then(Value::as_bool).unwrap_or(false),
        enabled_profiles: vec![profile],
        default_profile: Some(key),
        acknowledged_disclosures: if disclosure_acknowledged { vec![key] } else { vec![] },
    }
}

/// Sanitizes a raw config JSON value into a fully-valid `AiSettings`,
/// same field-by-field-degrades-gracefully approach as
/// `fleet-snowfluff-core::config::sanitize`: an unexpected type or a
/// missing field falls back to that field's default rather than
/// discarding the whole file.
pub fn sanitize(raw: &Value) -> AiSettings {
    let Some(obj) = raw.as_object() else {
        return AiSettings::default();
    };

    // Legacy-shape detection: a Stage 1 file has `active_provider` and
    // no `enabled_profiles` key at all. A fresh Stage 2 file (even one
    // with zero profiles) always has `enabled_profiles` written out,
    // so its *absence* -- not emptiness -- is what distinguishes "never
    // touched this format" from "has this format with nothing enabled."
    if obj.contains_key("active_provider") && !obj.contains_key("enabled_profiles") {
        return migrate_legacy_active_provider(obj);
    }

    let ai_enabled = obj.get("ai_enabled").and_then(Value::as_bool).unwrap_or(false);

    let enabled_profiles: Vec<ProviderProfile> = obj
        .get("enabled_profiles")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value::<ProviderProfile>(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    let default_profile = obj
        .get("default_profile")
        .and_then(parse_profile_key)
        .filter(|key| enabled_profiles.iter().any(|p| p.key() == *key));

    let acknowledged_disclosures: Vec<ProfileKey> = obj
        .get("acknowledged_disclosures")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(parse_profile_key).collect())
        .unwrap_or_default();

    AiSettings { ai_enabled, enabled_profiles, default_profile, acknowledged_disclosures }
}

/// Loads settings from raw file contents. Missing/unreadable/corrupt
/// content yields full defaults (`ai_enabled: false`, no profiles),
/// matching `config::load_from_str`'s precedent.
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

    fn profile(provider: ProviderKind, auth_method: AuthMethod) -> ProviderProfile {
        ProviderProfile { provider, auth_method, model: None, base_url: None }
    }

    #[test]
    fn defaults_are_disabled_with_no_profiles_configured() {
        let settings = AiSettings::default();
        assert!(!settings.ai_enabled);
        assert!(settings.enabled_profiles.is_empty());
        assert_eq!(settings.default_profile, None);
    }

    #[test]
    fn missing_file_content_yields_defaults() {
        let settings = load_from_str("");
        assert!(!settings.ai_enabled);
        assert!(settings.enabled_profiles.is_empty());
    }

    #[test]
    fn corrupt_json_yields_defaults() {
        let settings = load_from_str("{not valid json");
        assert_eq!(settings, AiSettings::default());
    }

    #[test]
    fn a_profile_round_trips_through_json() {
        let mut p = profile(ProviderKind::Anthropic, AuthMethod::Subscription);
        p.model = Some("claude-opus-5".to_string());
        let json = serde_json::to_string(&p).unwrap();
        let back: ProviderProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn disclosure_acknowledged_is_independent_of_profile_presence() {
        // Exercises the "Disclosure does not repeat once acknowledged"
        // scenario's disable/re-enable case: acknowledging a
        // (provider, auth method) pair, then removing that profile
        // entirely, must still report it as acknowledged -- the fact
        // doesn't get un-learned just because the profile was disabled.
        let key = ProfileKey { provider: ProviderKind::OpenAi, auth_method: AuthMethod::ApiKey };
        let mut settings = AiSettings::default();
        assert!(!settings.disclosure_acknowledged(key));

        settings.acknowledged_disclosures.push(key);
        assert!(settings.disclosure_acknowledged(key));

        // Profile never existed in enabled_profiles at all here -- the
        // acknowledgment is tracked independently, not read off a
        // profile field.
        assert!(settings.profile(key).is_none());
        assert!(settings.disclosure_acknowledged(key));
    }

    #[test]
    fn only_openai_subscription_is_marked_experimental() {
        assert!(profile(ProviderKind::OpenAi, AuthMethod::Subscription).is_experimental());

        assert!(!profile(ProviderKind::OpenAi, AuthMethod::ApiKey).is_experimental());
        assert!(!profile(ProviderKind::Anthropic, AuthMethod::ApiKey).is_experimental());
        assert!(!profile(ProviderKind::Anthropic, AuthMethod::Subscription).is_experimental());
        assert!(!profile(ProviderKind::Ollama, AuthMethod::Local).is_experimental());
        assert!(!profile(ProviderKind::Mock, AuthMethod::Local).is_experimental());
    }

    #[test]
    fn partial_json_fills_missing_fields_with_defaults() {
        let raw = json!({ "ai_enabled": true, "enabled_profiles": [] });
        let settings = sanitize(&raw);
        assert!(settings.ai_enabled);
        assert!(settings.enabled_profiles.is_empty());
        assert_eq!(settings.default_profile, None);
    }

    #[test]
    fn enabled_profiles_and_default_round_trip() {
        let raw = json!({
            "ai_enabled": true,
            "enabled_profiles": [
                { "provider": "ollama", "auth_method": "local", "model": "llama3.2:3b" },
                { "provider": "anthropic", "auth_method": "subscription" },
            ],
            "default_profile": { "provider": "anthropic", "auth_method": "subscription" },
            "acknowledged_disclosures": [{ "provider": "anthropic", "auth_method": "subscription" }],
        });
        let settings = sanitize(&raw);
        assert_eq!(settings.enabled_profiles.len(), 2);
        let default_key =
            ProfileKey { provider: ProviderKind::Anthropic, auth_method: AuthMethod::Subscription };
        assert_eq!(settings.default_profile, Some(default_key));
        assert_eq!(settings.default_profile().unwrap().model, None);
        assert!(settings.disclosure_acknowledged(default_key));
    }

    #[test]
    fn default_profile_pointing_at_a_profile_that_is_not_enabled_falls_back_to_none() {
        let raw = json!({
            "enabled_profiles": [{ "provider": "ollama", "auth_method": "local" }],
            "default_profile": { "provider": "anthropic", "auth_method": "api_key" },
        });
        let settings = sanitize(&raw);
        assert_eq!(
            settings.default_profile, None,
            "a dangling pointer must not crash or be trusted"
        );
    }

    #[test]
    fn independent_profiles_survive_switching_the_default() {
        // Exercises `subscription-first-chat`'s "Switching back to a
        // previously configured provider": every enabled profile's
        // settings stay present and usable regardless of which one is
        // currently the default.
        let raw = json!({
            "enabled_profiles": [
                { "provider": "open_ai", "auth_method": "api_key", "model": "gpt-4o-mini" },
                { "provider": "ollama", "auth_method": "local", "model": "llama3.2:3b" },
            ],
            "default_profile": { "provider": "ollama", "auth_method": "local" },
        });
        let mut settings = sanitize(&raw);
        assert_eq!(settings.default_profile().unwrap().provider, ProviderKind::Ollama);

        settings.default_profile =
            Some(ProfileKey { provider: ProviderKind::OpenAi, auth_method: AuthMethod::ApiKey });
        assert_eq!(settings.default_profile().unwrap().model.as_deref(), Some("gpt-4o-mini"));
    }

    #[test]
    fn round_trips_through_json() {
        let key = ProfileKey { provider: ProviderKind::OpenAi, auth_method: AuthMethod::ApiKey };
        let settings = AiSettings {
            ai_enabled: true,
            enabled_profiles: vec![{
                let mut p = profile(ProviderKind::OpenAi, AuthMethod::ApiKey);
                p.model = Some("gpt-4o".to_string());
                p
            }],
            default_profile: Some(key),
            acknowledged_disclosures: vec![key],
        };

        let json_str = to_json_string(&settings);
        let reloaded = load_from_str(&json_str);
        assert_eq!(reloaded, settings);
    }

    // -- Legacy migration (subscription-first-chat task 1.3/1.4) --

    #[test]
    fn legacy_active_provider_config_migrates_to_one_api_key_profile() {
        let raw = json!({
            "ai_enabled": true,
            "active_provider": "anthropic",
            "openai": { "model": "gpt-4o-mini", "disclosure_acknowledged": true },
            "anthropic": { "model": "claude-sonnet-5", "disclosure_acknowledged": true },
            "ollama": { "model": "llama3.2:3b" },
        });
        let settings = sanitize(&raw);

        assert!(settings.ai_enabled);
        assert_eq!(
            settings.enabled_profiles.len(),
            1,
            "only the previously active provider migrates"
        );
        let migrated = &settings.enabled_profiles[0];
        assert_eq!(migrated.provider, ProviderKind::Anthropic);
        assert_eq!(migrated.auth_method, AuthMethod::ApiKey);
        assert_eq!(migrated.model.as_deref(), Some("claude-sonnet-5"));
        assert!(settings.disclosure_acknowledged(migrated.key()));
        assert_eq!(settings.default_profile, Some(migrated.key()));
    }

    #[test]
    fn legacy_config_with_no_active_provider_migrates_to_zero_profiles() {
        let raw = json!({ "ai_enabled": false });
        let settings = sanitize(&raw);
        assert!(settings.enabled_profiles.is_empty());
        assert_eq!(settings.default_profile, None);
    }

    #[test]
    fn legacy_ollama_active_provider_migrates_with_local_auth_method() {
        let raw = json!({
            "active_provider": "ollama",
            "ollama": { "model": "llama3.2:3b" },
        });
        let settings = sanitize(&raw);
        assert_eq!(settings.enabled_profiles.len(), 1);
        assert_eq!(settings.enabled_profiles[0].auth_method, AuthMethod::Local);
        assert_eq!(settings.enabled_profiles[0].model.as_deref(), Some("llama3.2:3b"));
    }

    #[test]
    fn a_config_already_in_the_new_shape_is_never_treated_as_legacy() {
        // Presence of `enabled_profiles` (even empty) must win over
        // `active_provider` possibly still lingering from a hand-edited
        // file, so migration never re-runs on an already-migrated file.
        let raw = json!({
            "active_provider": "anthropic",
            "enabled_profiles": [],
            "default_profile": null,
        });
        let settings = sanitize(&raw);
        assert!(settings.enabled_profiles.is_empty());
        assert_eq!(settings.default_profile, None);
    }

    #[test]
    fn invalid_active_provider_falls_back_to_none_not_a_crash() {
        let raw = json!({ "active_provider": "not_a_real_provider" });
        let settings = sanitize(&raw);
        assert!(settings.enabled_profiles.is_empty());
        assert_eq!(settings.default_profile, None);
    }
}
