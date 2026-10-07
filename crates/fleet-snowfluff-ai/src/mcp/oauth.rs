//! OAuth 2.1 authorization for the HTTP/SSE transport -- the protocol's
//! own stance, verified directly against
//! `modelcontextprotocol.io/specification/2025-06-18/basic/authorization`
//! rather than assumed: a server's need for authorization is discovered
//! via a `401` + `WWW-Authenticate` header, never declared upfront
//! ([`discover`]); Dynamic Client Registration (RFC 7591) is what lets
//! this client connect to an arbitrary, unregistered server at all
//! ([`register_client_or_explain`]); PKCE with no client secret is the
//! standard flow for a native/desktop client like this one ([`generate_pkce`],
//! [`build_authorization_url`], [`exchange_code_for_token`]).
//!
//! Opening a system browser is deliberately **not** done here: this
//! crate has no `tauri` dependency (see design.md's own Decision), so
//! this module only produces the URL to open and waits for the
//! redirect -- actually launching a browser is the app crate's job
//! (`AppHandle`/`tauri-plugin-shell`), the same boundary
//! `AgentRuntime::run`'s own `on_text_delta` callback already draws
//! between "this crate decides what should happen" and "the app crate
//! is what can actually make it happen."

use std::{collections::HashMap, time::Duration};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

use super::{
    error::McpError,
    http_transport::{send_json_rpc, HttpAttemptOutcome},
};

/// What probing an HTTP MCP endpoint with no credential found -- the
/// MCP spec's own discovery trigger ("on a `401` response..."), kept
/// as its own step so a caller can tell "no authorization needed" from
/// "authorization needed, here's what `discover` needs" before
/// deciding whether to run the rest of this module at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthProbeOutcome {
    NotRequired,
    Required { www_authenticate: Option<String> },
}

/// Sends one unauthenticated `initialize` request and classifies the
/// result. A non-`401` failure (the server is just down, or genuinely
/// broken) is propagated as an error rather than folded into either
/// probe outcome -- it's neither "no auth needed" nor "here's how to
/// authenticate."
pub async fn probe_authorization(
    client: &reqwest::Client,
    url: &str,
) -> Result<AuthProbeOutcome, McpError> {
    match send_json_rpc(client, url, "initialize", None, None).await {
        HttpAttemptOutcome::Unauthorized { www_authenticate } => {
            Ok(AuthProbeOutcome::Required { www_authenticate })
        }
        HttpAttemptOutcome::Ok(_) => Ok(AuthProbeOutcome::NotRequired),
        HttpAttemptOutcome::Err(err) => Err(err),
    }
}

const REDIRECT_PATH: &str = "/callback";
/// How long [`RedirectListener::wait_for_code`] waits for the user to
/// finish in their browser before giving up -- generous for a human
/// completing a consent screen, but still bounded, so an abandoned
/// authorization attempt doesn't hold the listener open forever
/// (design.md: "starts only when needed, exits on success or timeout").
const REDIRECT_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

/// A PKCE verifier/challenge pair (RFC 7636), generated fresh for one
/// authorization attempt -- `verifier` is sent only at the final token
/// exchange, `challenge` only in the up-front authorization URL; a
/// party that only ever sees the URL (e.g. a browser history, a proxy
/// log) cannot derive the verifier from the challenge, since that
/// direction requires inverting SHA-256.
#[derive(Debug, Clone)]
pub struct PkceChallenge {
    pub verifier: String,
    pub challenge: String,
}

pub fn generate_pkce() -> PkceChallenge {
    let verifier_bytes: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    PkceChallenge { verifier, challenge }
}

/// A fresh, unguessable value for the authorization request's own
/// `state` parameter -- `RedirectListener::wait_for_code` rejects a
/// redirect whose `state` doesn't match, which is what actually makes
/// this a CSRF defense rather than a formality.
pub fn generate_state() -> String {
    let bytes: Vec<u8> = (0..16).map(|_| rand::random::<u8>()).collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Builds the URL to open in the system browser. `resource` is the MCP
/// server's own URL (RFC 8707 Resource Indicators) -- binding the
/// issued token to that specific server, so a token this client
/// obtains for one MCP server can't be replayed against a different
/// one the same authorization server also happens to protect.
pub fn build_authorization_url(
    metadata: &AuthorizationServerMetadata,
    client_id: &str,
    redirect_uri: &str,
    pkce: &PkceChallenge,
    state: &str,
    resource: &str,
) -> String {
    format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&\
         code_challenge_method=S256&state={}&resource={}",
        metadata.authorization_endpoint,
        percent_encode(client_id),
        percent_encode(redirect_uri),
        percent_encode(&pkce.challenge),
        percent_encode(state),
        percent_encode(resource),
    )
}

/// A resource server's own advertised authorization server(s) (RFC
/// 9728's `/.well-known/oauth-protected-resource` shape) -- only
/// `authorization_servers` is used; this client always picks the
/// first one listed.
#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    authorization_servers: Vec<String>,
}

/// An authorization server's own metadata (RFC 8414's
/// `/.well-known/oauth-authorization-server` shape) -- the fields this
/// client actually needs; anything else the metadata document carries
/// is ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizationServerMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
}

/// Parses `resource_metadata="..."` out of a `WWW-Authenticate` header
/// value (the parameter RFC 9728 defines for exactly this), falling
/// back to the well-known path relative to `resource_url`'s own origin
/// when the header is missing or doesn't carry it -- the fallback the
/// MCP spec itself documents for a server that skips the parameter.
fn resource_metadata_url(www_authenticate: Option<&str>, resource_url: &str) -> String {
    const PARAM: &str = "resource_metadata=\"";
    if let Some(header) = www_authenticate {
        if let Some(start) = header.find(PARAM) {
            let rest = &header[start + PARAM.len()..];
            if let Some(end) = rest.find('"') {
                return rest[..end].to_string();
            }
        }
    }
    format!("{}/.well-known/oauth-protected-resource", origin_of(resource_url))
}

fn origin_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .map(|parsed| format!("{}://{}", parsed.scheme(), parsed.authority()))
        .unwrap_or_else(|_| url.to_string())
}

/// Discovers the authorization server protecting `resource_url`:
/// fetches its protected-resource metadata, then that authorization
/// server's own metadata. `www_authenticate` is the `401` response's
/// own header value, when the server sent one.
pub async fn discover(
    client: &reqwest::Client,
    resource_url: &str,
    www_authenticate: Option<&str>,
) -> Result<AuthorizationServerMetadata, McpError> {
    let metadata_url = resource_metadata_url(www_authenticate, resource_url);
    let protected: ProtectedResourceMetadata = client
        .get(&metadata_url)
        .send()
        .await
        .map_err(|err| McpError(format!("failed to fetch {metadata_url}: {err}")))?
        .json()
        .await
        .map_err(|err| {
            McpError(format!("invalid protected-resource metadata at {metadata_url}: {err}"))
        })?;
    let authorization_server = protected
        .authorization_servers
        .first()
        .ok_or_else(|| McpError(format!("{metadata_url} advertised no authorization server")))?;
    let as_metadata_url = format!(
        "{}/.well-known/oauth-authorization-server",
        authorization_server.trim_end_matches('/')
    );
    client
        .get(&as_metadata_url)
        .send()
        .await
        .map_err(|err| McpError(format!("failed to fetch {as_metadata_url}: {err}")))?
        .json::<AuthorizationServerMetadata>()
        .await
        .map_err(|err| {
            McpError(format!("invalid authorization-server metadata at {as_metadata_url}: {err}"))
        })
}

#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    client_id: String,
}

#[derive(Debug)]
pub struct DynamicClientRegistration {
    pub client_id: String,
}

async fn register_client(
    client: &reqwest::Client,
    registration_endpoint: &str,
    redirect_uri: &str,
) -> Result<DynamicClientRegistration, McpError> {
    let body = serde_json::json!({
        "client_name": "Fleet Snowfluff",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let response =
        client.post(registration_endpoint).json(&body).send().await.map_err(|err| {
            McpError(format!("dynamic client registration request failed: {err}"))
        })?;
    if !response.status().is_success() {
        return Err(McpError(format!(
            "dynamic client registration failed against {registration_endpoint}: HTTP {}",
            response.status()
        )));
    }
    let parsed: RegistrationResponse = response
        .json()
        .await
        .map_err(|err| McpError(format!("invalid dynamic client registration response: {err}")))?;
    Ok(DynamicClientRegistration { client_id: parsed.client_id })
}

/// Registers a client against `metadata`'s own authorization server, or
/// returns a clear, specific error naming `server_label` if it never
/// advertised a `registration_endpoint` at all -- the "not a silent
/// dead end" fallback design.md calls for, rather than attempting a
/// request against an endpoint that was never there.
pub async fn register_client_or_explain(
    client: &reqwest::Client,
    metadata: &AuthorizationServerMetadata,
    server_label: &str,
    redirect_uri: &str,
) -> Result<DynamicClientRegistration, McpError> {
    let Some(registration_endpoint) = metadata.registration_endpoint.as_deref() else {
        return Err(McpError(format!(
            "{server_label} does not support Dynamic Client Registration (RFC 7591) -- it needs a \
             manually configured client id, which this client cannot supply on its own"
        )));
    };
    register_client(client, registration_endpoint, redirect_uri).await
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

/// Exchanges an authorization code for a token. Used both for a code
/// obtained via [`RedirectListener::wait_for_code`] and for one a user
/// pastes in manually (the fallback for when the loopback redirect
/// can't be used at all) -- both are just a `code` string by the time
/// they reach this function, so there is no separate "manual" variant.
pub async fn exchange_code_for_token(
    client: &reqwest::Client,
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    code_verifier: &str,
) -> Result<TokenResponse, McpError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ];
    let response = client
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|err| McpError(format!("token exchange request failed: {err}")))?;
    if !response.status().is_success() {
        return Err(McpError(format!(
            "token exchange failed against {token_endpoint}: HTTP {}",
            response.status()
        )));
    }
    response.json().await.map_err(|err| McpError(format!("invalid token response: {err}")))
}

/// Trades a refresh token for a fresh access token. A server may rotate
/// the refresh token too; the caller keeps the new one when present.
pub async fn refresh_access_token(
    client: &reqwest::Client,
    token_endpoint: &str,
    refresh_token: &str,
    client_id: &str,
) -> Result<TokenResponse, McpError> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    let response = client
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|err| McpError(format!("token refresh request failed: {err}")))?;
    if !response.status().is_success() {
        return Err(McpError(format!(
            "token refresh failed against {token_endpoint}: HTTP {}",
            response.status()
        )));
    }
    response.json().await.map_err(|err| McpError(format!("invalid token response: {err}")))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_redirect_query(request_line: &str) -> HashMap<String, String> {
    let path = request_line.split_whitespace().nth(1).unwrap_or_default();
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or_default();
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_string(), percent_decode(value)))
        .collect()
}

async fn read_request_line(stream: &mut TcpStream) -> Result<String, McpError> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream
            .read(&mut byte)
            .await
            .map_err(|err| McpError(format!("failed to read the redirect request: {err}")))?;
        if n == 0 || byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&buf).trim().to_string())
}

/// A short-lived, on-demand local HTTP listener catching exactly one
/// OAuth redirect -- bound only right before opening the browser, and
/// never reused: this is not a long-running local server, it exists
/// only to answer the one callback this authorization attempt expects.
pub struct RedirectListener {
    listener: TcpListener,
    port: u16,
}

impl RedirectListener {
    pub async fn bind() -> Result<Self, McpError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|err| McpError(format!("failed to bind a local redirect listener: {err}")))?;
        let port = listener
            .local_addr()
            .map_err(|err| McpError(format!("failed to read the listener's own port: {err}")))?
            .port();
        Ok(Self { listener, port })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{REDIRECT_PATH}", self.port)
    }

    /// Waits for one connection, validates its `state` matches
    /// `expected_state` (rejecting a forged or stale redirect), answers
    /// with a short human-readable confirmation page, and returns the
    /// authorization code. Consumes `self`: this listener answers
    /// exactly one request, ever.
    pub async fn wait_for_code(self, expected_state: &str) -> Result<String, McpError> {
        let attempt = async {
            let (mut stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|err| McpError(format!("redirect listener failed: {err}")))?;
            let request_line = read_request_line(&mut stream).await?;
            let query = parse_redirect_query(&request_line);

            let body =
                "<html><body>You can close this tab and return to Fleet Snowfluff.</body></html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: \
                 close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.ok();

            match (query.get("code"), query.get("state")) {
                (Some(code), Some(state)) if state == expected_state => Ok(code.clone()),
                (_, Some(state)) => Err(McpError(format!(
                    "redirect state did not match (expected {expected_state}, got {state})"
                ))),
                _ => Err(McpError("redirect did not include an authorization code".to_string())),
            }
        };
        match timeout(REDIRECT_WAIT_TIMEOUT, attempt).await {
            Ok(result) => result,
            Err(_) => Err(McpError("timed out waiting for the authorization redirect".to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::providers::test_server;

    #[test]
    fn pkce_challenge_is_the_base64url_sha256_of_the_verifier() {
        let pkce = generate_pkce();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);
        assert_ne!(pkce.verifier, pkce.challenge);
    }

    #[test]
    fn generate_state_produces_distinct_values() {
        assert_ne!(generate_state(), generate_state());
    }

    #[test]
    fn authorization_url_percent_encodes_every_parameter() {
        let metadata = AuthorizationServerMetadata {
            authorization_endpoint: "https://auth.example.com/authorize".to_string(),
            token_endpoint: "https://auth.example.com/token".to_string(),
            registration_endpoint: None,
        };
        let pkce = PkceChallenge { verifier: "v".to_string(), challenge: "c+h/a=l".to_string() };
        let url = build_authorization_url(
            &metadata,
            "client-1",
            "http://127.0.0.1:4096/callback",
            &pkce,
            "state value",
            "https://mcp.example.com/sse",
        );
        assert!(url.starts_with("https://auth.example.com/authorize?"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A4096%2Fcallback"));
        assert!(url.contains("code_challenge=c%2Bh%2Fa%3Dl"));
        assert!(url.contains("state=state%20value"));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[tokio::test]
    async fn probe_authorization_reports_not_required_on_a_successful_response() {
        let (base_url, _handle) = test_server::serve_once(
            200,
            test_server::chunked(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, 4096),
        );
        let outcome = probe_authorization(&reqwest::Client::new(), &base_url).await.unwrap();
        assert_eq!(outcome, AuthProbeOutcome::NotRequired);
    }

    #[tokio::test]
    async fn probe_authorization_reports_required_with_the_header_on_a_401() {
        let (base_url, _handle) = test_server::serve_once_with_headers(
            401,
            vec![("WWW-Authenticate", r#"Bearer resource_metadata="https://example.com/meta""#)],
            vec![],
        );
        let outcome = probe_authorization(&reqwest::Client::new(), &base_url).await.unwrap();
        assert_eq!(
            outcome,
            AuthProbeOutcome::Required {
                www_authenticate: Some(
                    r#"Bearer resource_metadata="https://example.com/meta""#.to_string()
                )
            }
        );
    }

    #[tokio::test]
    async fn discover_uses_the_resource_metadata_hint_from_www_authenticate() {
        let (resource_meta_base, resource_meta_handle) = test_server::serve_once(
            200,
            test_server::chunked(
                &json!({"authorization_servers": ["https://auth.example.com"]}).to_string(),
                4096,
            ),
        );
        let www_authenticate = format!(
            r#"Bearer resource_metadata="{resource_meta_base}/.well-known/oauth-protected-resource""#
        );

        // discover() fetches resource metadata first, then the
        // authorization server's own metadata -- but the latter's URL
        // is derived from the *content* of the first response
        // ("https://auth.example.com"), which this test can't actually
        // serve. So this test only exercises the first hop directly,
        // confirming the header's hint (not the origin fallback) is
        // what gets used.
        let metadata_url =
            resource_metadata_url(Some(&www_authenticate), "https://mcp.example.com/sse");
        assert_eq!(
            metadata_url,
            format!("{resource_meta_base}/.well-known/oauth-protected-resource")
        );

        let client = reqwest::Client::new();
        let fetched: ProtectedResourceMetadata =
            client.get(&metadata_url).send().await.unwrap().json().await.unwrap();
        assert_eq!(fetched.authorization_servers, vec!["https://auth.example.com".to_string()]);
        resource_meta_handle.join().unwrap();
    }

    #[test]
    fn resource_metadata_url_falls_back_to_the_well_known_path_without_a_header() {
        let url = resource_metadata_url(None, "https://mcp.example.com/sse");
        assert_eq!(url, "https://mcp.example.com/.well-known/oauth-protected-resource");
    }

    #[tokio::test]
    async fn discover_fetches_both_hops_end_to_end_against_mocked_servers() {
        let (as_base, as_handle) = test_server::serve_once(
            200,
            test_server::chunked(
                &json!({
                    "authorization_endpoint": "https://auth.example.com/authorize",
                    "token_endpoint": "https://auth.example.com/token",
                    "registration_endpoint": "https://auth.example.com/register",
                })
                .to_string(),
                4096,
            ),
        );
        let (resource_base, resource_handle) = test_server::serve_once(
            200,
            test_server::chunked(&json!({"authorization_servers": [as_base]}).to_string(), 4096),
        );
        let www_authenticate = format!(
            r#"Bearer resource_metadata="{resource_base}/.well-known/oauth-protected-resource""#
        );
        let client = reqwest::Client::new();

        let metadata = discover(&client, "https://mcp.example.com/sse", Some(&www_authenticate))
            .await
            .unwrap();

        assert_eq!(metadata.authorization_endpoint, "https://auth.example.com/authorize");
        assert_eq!(
            metadata.registration_endpoint,
            Some("https://auth.example.com/register".to_string())
        );
        resource_handle.join().unwrap();
        as_handle.join().unwrap();
    }

    #[tokio::test]
    async fn register_client_or_explain_names_the_server_when_unsupported() {
        let metadata = AuthorizationServerMetadata {
            authorization_endpoint: "https://auth.example.com/authorize".to_string(),
            token_endpoint: "https://auth.example.com/token".to_string(),
            registration_endpoint: None,
        };
        let client = reqwest::Client::new();

        let err = register_client_or_explain(
            &client,
            &metadata,
            "Example MCP Server",
            "http://127.0.0.1:1/callback",
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("Example MCP Server"));
        assert!(err.to_string().contains("Dynamic Client Registration"));
    }

    #[tokio::test]
    async fn register_client_or_explain_succeeds_against_a_mocked_registration_endpoint() {
        let (base_url, handle) = test_server::serve_once(
            200,
            test_server::chunked(&json!({"client_id": "generated-client-id"}).to_string(), 4096),
        );
        let metadata = AuthorizationServerMetadata {
            authorization_endpoint: "https://auth.example.com/authorize".to_string(),
            token_endpoint: "https://auth.example.com/token".to_string(),
            registration_endpoint: Some(base_url),
        };
        let client = reqwest::Client::new();

        let registration = register_client_or_explain(
            &client,
            &metadata,
            "Example MCP Server",
            "http://127.0.0.1:1/callback",
        )
        .await
        .unwrap();

        assert_eq!(registration.client_id, "generated-client-id");
        let captured = handle.join().unwrap();
        assert!(captured.body.contains("\"token_endpoint_auth_method\":\"none\""));
    }

    #[tokio::test]
    async fn exchange_code_for_token_sends_the_verifier_and_parses_the_token() {
        let (base_url, handle) = test_server::serve_once(
            200,
            test_server::chunked(
                &json!({"access_token": "at-123", "refresh_token": "rt-456"}).to_string(),
                4096,
            ),
        );

        let token = exchange_code_for_token(
            &reqwest::Client::new(),
            &base_url,
            "auth-code",
            "http://127.0.0.1:4096/callback",
            "client-1",
            "the-verifier",
        )
        .await
        .unwrap();

        assert_eq!(token.access_token, "at-123");
        assert_eq!(token.refresh_token, Some("rt-456".to_string()));
        let captured = handle.join().unwrap();
        assert!(captured.body.contains("code_verifier=the-verifier"));
        assert!(captured.body.contains("grant_type=authorization_code"));
    }

    #[tokio::test]
    async fn refresh_access_token_sends_the_refresh_grant_and_parses_the_new_token() {
        let (base_url, handle) = test_server::serve_once(
            200,
            test_server::chunked(
                &json!({"access_token": "at-new", "refresh_token": "rt-new"}).to_string(),
                4096,
            ),
        );

        let token = refresh_access_token(&reqwest::Client::new(), &base_url, "rt-old", "client-1")
            .await
            .unwrap();

        assert_eq!(token.access_token, "at-new");
        assert_eq!(token.refresh_token, Some("rt-new".to_string()));
        let captured = handle.join().unwrap();
        assert!(captured.body.contains("grant_type=refresh_token"));
        assert!(captured.body.contains("refresh_token=rt-old"));
    }

    #[tokio::test]
    async fn refresh_access_token_reports_a_rejected_refresh() {
        let (base_url, _handle) = test_server::serve_once(400, Vec::new());

        let err = refresh_access_token(&reqwest::Client::new(), &base_url, "rt-old", "client-1")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("HTTP 400"));
    }

    #[tokio::test]
    async fn redirect_listener_extracts_the_code_from_a_matching_redirect() {
        let listener = RedirectListener::bind().await.unwrap();
        let redirect_uri = listener.redirect_uri();
        let port = redirect_uri
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches(REDIRECT_PATH)
            .parse::<u16>()
            .unwrap();

        let client_task = tokio::spawn(async move {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            stream
                .write_all(
                    b"GET /callback?code=the-code&state=the-state HTTP/1.1\r\nHost: \
                      127.0.0.1\r\n\r\n",
                )
                .await
                .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.ok();
        });

        let code = listener.wait_for_code("the-state").await.unwrap();
        assert_eq!(code, "the-code");
        client_task.await.unwrap();
    }

    #[tokio::test]
    async fn redirect_listener_rejects_a_state_mismatch() {
        let listener = RedirectListener::bind().await.unwrap();
        let redirect_uri = listener.redirect_uri();
        let port = redirect_uri
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches(REDIRECT_PATH)
            .parse::<u16>()
            .unwrap();

        let client_task = tokio::spawn(async move {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            stream
                .write_all(
                    b"GET /callback?code=the-code&state=wrong-state HTTP/1.1\r\nHost: \
                      127.0.0.1\r\n\r\n",
                )
                .await
                .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.ok();
        });

        let err = listener.wait_for_code("expected-state").await.unwrap_err();
        assert!(err.to_string().contains("state did not match"));
        client_task.await.unwrap();
    }
}
