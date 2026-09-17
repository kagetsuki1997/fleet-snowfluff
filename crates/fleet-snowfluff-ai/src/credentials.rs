//! `ProviderCredentials`: the shape of `secrets.json` -- API keys
//! only, kept entirely separate from `AiSettings`/`ai-config.json` so
//! a config file a user might reasonably hand-edit, screenshot, or
//! back up never contains a key (`ai-provider`'s "Separate credential
//! storage").

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCredentials {
    #[serde(default)]
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub anthropic_api_key: Option<String>,
    // Ollama and Mock need no credentials.
}

/// Missing/unreadable/corrupt content yields empty credentials rather
/// than an error -- a brand-new install has no secrets file at all.
pub fn load_from_str(contents: &str) -> ProviderCredentials {
    serde_json::from_str(contents).unwrap_or_default()
}

pub fn to_json_string(credentials: &ProviderCredentials) -> String {
    serde_json::to_string_pretty(credentials)
        .expect("ProviderCredentials serialization is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_content_yields_empty_credentials() {
        assert_eq!(load_from_str(""), ProviderCredentials::default());
    }

    #[test]
    fn corrupt_json_yields_empty_credentials() {
        assert_eq!(load_from_str("{not valid json"), ProviderCredentials::default());
    }

    #[test]
    fn round_trips_through_json() {
        let creds = ProviderCredentials {
            openai_api_key: Some("sk-test".to_string()),
            anthropic_api_key: None,
        };
        let reloaded = load_from_str(&to_json_string(&creds));
        assert_eq!(reloaded, creds);
    }

    #[test]
    fn serialized_form_uses_the_expected_field_names() {
        // Sanity check that a reader hand-inspecting secrets.json (as
        // this project's whole file-transparency ethos assumes they
        // might) sees exactly the two documented key names.
        let creds =
            ProviderCredentials { openai_api_key: Some("sk-test".into()), anthropic_api_key: None };
        let json = to_json_string(&creds);
        assert!(json.contains("openai_api_key"));
        assert!(json.contains("anthropic_api_key"));
    }
}
