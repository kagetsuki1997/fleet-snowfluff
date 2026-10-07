//! [`HttpTransport`]: speaks MCP JSON-RPC over a single HTTP endpoint,
//! the "Streamable HTTP" shape MCP's own spec describes -- one POST per
//! JSON-RPC request, with `Authorization: Bearer <token>` once a token
//! exists. This client never needs genuine server-initiated streaming
//! for `initialize`/`tools/list`/`tools/call` (none of them push
//! unsolicited updates), so [`send_json_rpc`] only unwraps a single
//! `data:`-framed SSE event if the server chose to answer that way,
//! rather than implementing a full SSE event loop.
//!
//! [`HttpTransport`] itself assumes authorization (if the server needs
//! any) is already resolved -- a `401` reaching it at that point is
//! just an ordinary failure (e.g. an expired token). Distinguishing
//! "this server needs OAuth" from "this call failed" is exactly the
//! bootstrapping-time question [`super::oauth`] answers, *before* an
//! `HttpTransport` is ever constructed for steady-state use -- see
//! [`HttpAttemptOutcome`] below, which is what makes that distinction
//! available to `oauth`'s own discovery step without baking it into
//! [`super::transport::McpTransport`]'s own signature.

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;

use super::{error::McpError, transport::McpTransport};

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Default, Deserialize)]
struct JsonRpcHttpResponse {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

/// What one raw POST against an MCP HTTP endpoint found -- the
/// `Unauthorized` case is the one piece of information plain
/// `Result<Value, McpError>` can't carry, since it needs the
/// `WWW-Authenticate` header's value, not just "this failed."
#[derive(Debug)]
pub(crate) enum HttpAttemptOutcome {
    Ok(Value),
    Unauthorized { www_authenticate: Option<String> },
    Err(McpError),
}

/// Sends one JSON-RPC request (`id: 1` -- this is always a single,
/// synchronous round trip, never pipelined) and classifies the result.
/// Shared by [`HttpTransport::request`] (steady-state use, once
/// authorization is resolved) and [`super::oauth`]'s own bootstrapping
/// probe (which specifically needs the `Unauthorized` case).
///
/// **Requests only** -- a JSON-RPC *notification* must carry no `id`
/// field at all, which is what actually distinguishes it from a
/// request a server must answer; see [`send_json_rpc_notification`]
/// for that case, used by [`HttpTransport::notify`].
pub(crate) async fn send_json_rpc(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: Option<Value>,
    bearer_token: Option<&str>,
) -> HttpAttemptOutcome {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params.unwrap_or(Value::Object(Default::default())),
    });
    let mut request = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&body);
    if let Some(token) = bearer_token {
        request = request.bearer_auth(token);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => return HttpAttemptOutcome::Err(McpError(format!("request failed: {err}"))),
    };
    if response.status() == StatusCode::UNAUTHORIZED {
        let www_authenticate = response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        return HttpAttemptOutcome::Unauthorized { www_authenticate };
    }
    if !response.status().is_success() {
        return HttpAttemptOutcome::Err(McpError(format!("HTTP {}", response.status())));
    }
    let text = match response.text().await {
        Ok(text) => text,
        Err(err) => {
            return HttpAttemptOutcome::Err(McpError(format!("failed to read response: {err}")))
        }
    };
    let json_text = text.lines().find_map(|line| line.strip_prefix("data: ")).unwrap_or(&text);
    let parsed: JsonRpcHttpResponse = match serde_json::from_str(json_text) {
        Ok(parsed) => parsed,
        Err(err) => {
            return HttpAttemptOutcome::Err(McpError(format!("invalid JSON-RPC response: {err}")))
        }
    };
    match parsed.error {
        Some(err) => HttpAttemptOutcome::Err(McpError(format!("{} ({})", err.message, err.code))),
        None => HttpAttemptOutcome::Ok(parsed.result.unwrap_or(Value::Null)),
    }
}

/// Sends a JSON-RPC *notification* -- no `id` field, per spec, which is
/// what tells a compliant server not to treat this as a method it must
/// look up and answer. No response body is parsed: a server that
/// follows the spec answers a notification with an empty `202
/// Accepted` (or similar) and nothing to parse as JSON-RPC; only the
/// HTTP-level outcome is reported.
pub(crate) async fn send_json_rpc_notification(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: Option<Value>,
    bearer_token: Option<&str>,
) -> Result<(), McpError> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params.unwrap_or(Value::Object(Default::default())),
    });
    let mut request = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&body);
    if let Some(token) = bearer_token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|err| McpError(format!("notification request failed: {err}")))?;
    if !response.status().is_success() {
        return Err(McpError(format!("HTTP {}", response.status())));
    }
    Ok(())
}

/// A bounded-timeout client for every production MCP network call --
/// `tool-list-optimization`'s own real-world trigger: `mcp_sourced_tools()`
/// reconnects to every `Ready` server before *every* chat turn, not
/// just MCP-related ones, and a plain `reqwest::Client::new()` has no
/// timeout at all. A stale OAuth token, an unreachable server, or just
/// a slow one would otherwise hang that reconnect indefinitely --
/// silently stalling every single message, confirmed by a real report
/// ("even a new conversation" came back empty after a connected
/// server's own access token had likely expired). 15s is generous for
/// a local MCP round trip but still bounded.
pub fn mcp_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("a bounded-timeout reqwest client should always build")
}

pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    bearer_token: Option<String>,
}

impl HttpTransport {
    pub fn new(url: impl Into<String>, bearer_token: Option<String>) -> Self {
        Self { client: mcp_http_client(), url: url.into(), bearer_token }
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        match send_json_rpc(&self.client, &self.url, method, params, self.bearer_token.as_deref())
            .await
        {
            HttpAttemptOutcome::Ok(value) => Ok(value),
            HttpAttemptOutcome::Unauthorized { .. } => {
                Err(McpError("server returned 401 Unauthorized".to_string()))
            }
            HttpAttemptOutcome::Err(err) => Err(err),
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        send_json_rpc_notification(
            &self.client,
            &self.url,
            method,
            params,
            self.bearer_token.as_deref(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::providers::test_server;

    #[tokio::test]
    async fn request_sends_bearer_auth_once_a_token_exists_and_parses_the_result() {
        let (base_url, handle) = test_server::serve_once(
            200,
            test_server::chunked(r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#, 4096),
        );
        let transport = HttpTransport::new(base_url, Some("secret-token".to_string()));

        let result = transport.request("tools/list", None).await.unwrap();

        assert_eq!(result, json!({"tools": []}));
        let captured = handle.join().unwrap();
        assert_eq!(captured.header("authorization"), Some("Bearer secret-token"));
        assert!(captured.body.contains("\"method\":\"tools/list\""));
    }

    #[tokio::test]
    async fn request_sends_no_authorization_header_without_a_token() {
        let (base_url, handle) = test_server::serve_once(
            200,
            test_server::chunked(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, 4096),
        );
        let transport = HttpTransport::new(base_url, None);

        transport.request("initialize", None).await.unwrap();

        let captured = handle.join().unwrap();
        assert_eq!(captured.header("authorization"), None);
    }

    #[tokio::test]
    async fn a_jsonrpc_error_response_becomes_an_mcp_error() {
        let (base_url, _handle) = test_server::serve_once(
            200,
            test_server::chunked(
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#,
                4096,
            ),
        );
        let transport = HttpTransport::new(base_url, None);

        let err = transport.request("no_such_method", None).await.unwrap_err();
        assert!(err.to_string().contains("-32601"));
    }

    #[tokio::test]
    async fn notify_sends_no_id_field_unlike_a_request() {
        // The bug this guards against: reusing the request-framing path
        // for a notification sends an `id`, making a compliant server
        // treat it as a real method call it must look up -- which,
        // for a method name like "notifications/initialized" that's
        // only ever meaningful as a true notification, correctly comes
        // back as "Method not found" (confirmed against the real
        // `mcp.notion.com` server during manual testing).
        let (base_url, handle) = test_server::serve_once(202, vec![]);
        let transport = HttpTransport::new(base_url, None);

        transport.notify("notifications/initialized", None).await.unwrap();

        let captured = handle.join().unwrap();
        let sent: Value = serde_json::from_str(&captured.body).unwrap();
        assert!(sent.get("id").is_none(), "a notification must carry no id field: {sent}");
        assert_eq!(sent["method"], "notifications/initialized");
    }

    #[tokio::test]
    async fn a_401_response_is_reported_as_an_mcp_error_at_the_transport_level() {
        let (base_url, _handle) =
            test_server::serve_once_with_headers(401, vec![("WWW-Authenticate", "Bearer")], vec![]);
        let transport = HttpTransport::new(base_url, None);

        let err = transport.request("tools/list", None).await.unwrap_err();
        assert!(err.to_string().contains("401"));
    }

    #[tokio::test]
    async fn request_times_out_against_a_server_that_never_responds() {
        // The real bug this guards against: `mcp_sourced_tools()` (app
        // crate) reconnects to every `Ready` server before *every*
        // chat turn -- a plain `reqwest::Client::new()` has no timeout
        // at all, so a server that accepts the connection and then
        // never answers (a stale OAuth token against a slow server,
        // confirmed by a real report of "even a new conversation"
        // coming back empty) would hang that reconnect, and so the
        // whole turn, indefinitely. `HttpTransport` fields are private
        // but same-module, so this constructs one directly with a
        // short test timeout rather than waiting out the real 15s
        // `mcp_http_client()` uses.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            // Accept and then hold the connection open, answering
            // nothing, for the lifetime of this test process.
            let _ = listener.accept();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        });

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(300))
            .build()
            .unwrap();
        let transport = HttpTransport { client, url: format!("http://{addr}"), bearer_token: None };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            transport.request("initialize", None),
        )
        .await
        .expect("the client's own timeout must fire well within 2s, not hang indefinitely");

        assert!(
            result.is_err(),
            "a server that never responds must be reported as a failure, not hang forever"
        );
    }
}
