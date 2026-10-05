//! `ProviderCredentials`: the shape of `secrets.json` -- API keys and,
//! since `mcp-client-support`, connected MCP servers' own credentials
//! -- kept entirely separate from `AiSettings`/`ai-config.json` so a
//! config file a user might reasonably hand-edit, screenshot, or back
//! up never contains a key (`ai-provider`'s "Separate credential
//! storage").

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// One connected MCP server's own credential, shaped by its transport
/// (design.md's "Credential shape forks by transport"): a stdio server
/// takes a plain environment-variable value (no pending state -- either
/// the user has it or they don't); an HTTP server that needed OAuth
/// gets the resulting token pair instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum McpServerCredential {
    EnvVar {
        value: String,
    },
    OAuthToken {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCredentials {
    #[serde(default)]
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub anthropic_api_key: Option<String>,
    // Ollama and Mock need no credentials.
    /// Keyed by the connected MCP server's own id (`McpServerConfig::id`,
    /// Group 4's persisted config) -- a server with no credential at
    /// all (an unauthenticated HTTP endpoint, or a stdio server needing
    /// none) simply has no entry here.
    #[serde(default)]
    pub mcp_server_credentials: HashMap<String, McpServerCredential>,
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
            mcp_server_credentials: HashMap::new(),
        };
        let reloaded = load_from_str(&to_json_string(&creds));
        assert_eq!(reloaded, creds);
    }

    #[test]
    fn serialized_form_uses_the_expected_field_names() {
        // Sanity check that a reader hand-inspecting secrets.json (as
        // this project's whole file-transparency ethos assumes they
        // might) sees exactly the two documented key names.
        let creds = ProviderCredentials {
            openai_api_key: Some("sk-test".into()),
            anthropic_api_key: None,
            mcp_server_credentials: HashMap::new(),
        };
        let json = to_json_string(&creds);
        assert!(json.contains("openai_api_key"));
        assert!(json.contains("anthropic_api_key"));
    }

    #[test]
    fn an_env_var_mcp_credential_round_trips_keyed_by_server_id() {
        let mut creds = ProviderCredentials::default();
        creds.mcp_server_credentials.insert(
            "local-filesystem".to_string(),
            McpServerCredential::EnvVar { value: "super-secret".to_string() },
        );
        let reloaded = load_from_str(&to_json_string(&creds));
        assert_eq!(reloaded, creds);
        assert_eq!(
            reloaded.mcp_server_credentials.get("local-filesystem"),
            Some(&McpServerCredential::EnvVar { value: "super-secret".to_string() })
        );
    }

    #[test]
    fn an_oauth_token_mcp_credential_round_trips_with_an_optional_refresh_token() {
        let mut creds = ProviderCredentials::default();
        creds.mcp_server_credentials.insert(
            "github".to_string(),
            McpServerCredential::OAuthToken {
                access_token: "at-123".to_string(),
                refresh_token: Some("rt-456".to_string()),
            },
        );
        let reloaded = load_from_str(&to_json_string(&creds));
        assert_eq!(reloaded, creds);
    }
}
