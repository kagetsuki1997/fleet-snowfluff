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

pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    bearer_token: Option<String>,
}

impl HttpTransport {
    pub fn new(url: impl Into<String>, bearer_token: Option<String>) -> Self {
        Self { client: reqwest::Client::new(), url: url.into(), bearer_token }
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
        match send_json_rpc(&self.client, &self.url, method, params, self.bearer_token.as_deref())
            .await
        {
            HttpAttemptOutcome::Err(err) => Err(err),
            _ => Ok(()),
        }
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
    async fn a_401_response_is_reported_as_an_mcp_error_at_the_transport_level() {
        let (base_url, _handle) =
            test_server::serve_once_with_headers(401, vec![("WWW-Authenticate", "Bearer")], vec![]);
        let transport = HttpTransport::new(base_url, None);

        let err = transport.request("tools/list", None).await.unwrap_err();
        assert!(err.to_string().contains("401"));
    }
}
