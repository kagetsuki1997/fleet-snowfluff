//! [`StdioTransport`]: speaks MCP JSON-RPC over a spawned subprocess's
//! stdin/stdout, newline-delimited (MCP's own stdio transport spec).
//! Matches `cli_process.rs`'s own `kill_on_drop(true)` discipline
//! (confirmed by reading it, not assumed) for the same reason: dropping
//! this transport -- an abandoned or replaced connection -- must not
//! leave the server process running in the background.
//!
//! One request at a time per connection: a request holds the write
//! half's lock for the write, then the read half's lock until *a*
//! response line arrives whose `id` matches, skipping any that don't
//! (e.g. a server-initiated message addressed to no request of ours).
//! This is deliberately simpler than a full id-multiplexing client --
//! every MCP tool is `Confirm`-tier (`super::tool::McpTool`), and
//! `AemeathAgentRuntime::run()` only ever executes `Confirm`-tier calls
//! one at a time, in request order (see `agent_runtime.rs`'s own
//! `pending_confirm` loop), so no caller in this codebase ever needs
//! two requests in flight on the same connection at once.

use std::{
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

use super::{error::McpError, transport::McpTransport};

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Default, Deserialize)]
struct JsonRpcResponse {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

pub struct StdioTransport {
    // Held only to keep the child alive and to let `kill_on_drop`
    // actually terminate it when this transport is dropped -- never
    // read or written to directly; `stdin`/`reader` below own that.
    _child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    reader: Mutex<Lines<BufReader<ChildStdout>>>,
    next_id: AtomicU64,
}

impl StdioTransport {
    /// Spawns `command` with `args` and `env`, piping stdin/stdout
    /// (stderr is left to inherit nowhere -- discarded -- since this
    /// client has no UI surface for a server's own diagnostic output
    /// today). Returns before any MCP handshake happens; call
    /// [`super::protocol::McpClient::initialize`] next.
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<Self, McpError> {
        let mut builder = Command::new(command);
        builder
            .args(args)
            .envs(env.iter().map(|(key, value)| (key.as_str(), value.as_str())))
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = builder
            .spawn()
            .map_err(|err| McpError(format!("failed to spawn `{command}`: {err}")))?;
        let stdin = child.stdin.take().expect("stdin was piped by spawn");
        let stdout = child.stdout.take().expect("stdout was piped by spawn");
        Ok(Self {
            _child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            reader: Mutex::new(BufReader::new(stdout).lines()),
            next_id: AtomicU64::new(1),
        })
    }

    async fn write_line(&self, payload: &Value) -> Result<(), McpError> {
        let mut line = serde_json::to_string(payload)
            .map_err(|err| McpError(format!("failed to serialize request: {err}")))?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|err| McpError(format!("failed to write to server: {err}")))?;
        stdin.flush().await.map_err(|err| McpError(format!("failed to flush to server: {err}")))
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params.unwrap_or(Value::Object(Default::default())),
        });
        self.write_line(&payload).await?;

        let mut reader = self.reader.lock().await;
        loop {
            let line = reader
                .next_line()
                .await
                .map_err(|err| McpError(format!("failed to read from server: {err}")))?
                .ok_or_else(|| McpError("server closed the connection".to_string()))?;
            if line.trim().is_empty() {
                continue;
            }
            let response: JsonRpcResponse = serde_json::from_str(&line)
                .map_err(|err| McpError(format!("invalid response from server: {err}")))?;
            if response.id != Some(Value::from(id)) {
                // Not this request's response (a notification, or a
                // response to a request this connection never made) --
                // keep reading until the matching id arrives.
                continue;
            }
            return match response.error {
                Some(err) => Err(McpError(format!("{} ({})", err.message, err.code))),
                None => Ok(response.result.unwrap_or(Value::Null)),
            };
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params.unwrap_or(Value::Object(Default::default())),
        });
        self.write_line(&payload).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mcp::protocol::McpClient;

    /// A tiny fixture script standing in for a real MCP server: reads
    /// one JSON-RPC line, and for `tools/list` specifically replies
    /// with a fixed tool list; every other method (including
    /// `initialize`) gets an empty, successful result, and the
    /// `notifications/initialized` notification (no `id`) is read and
    /// silently ignored, exactly as a real server would.
    const FIXTURE_SCRIPT: &str = r#"
import sys, json

for raw_line in sys.stdin:
    line = raw_line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if "id" not in msg:
        # a notification -- no response
        continue
    if msg.get("method") == "tools/list":
        result = {"tools": [{"name": "echo", "description": "Echoes", "inputSchema": {"type": "object"}}]}
    else:
        result = {}
    reply = {"jsonrpc": "2.0", "id": msg["id"], "result": result}
    print(json.dumps(reply))
    sys.stdout.flush()
"#;

    fn python_available() -> Option<&'static str> {
        ["python3", "python"].into_iter().find(|candidate| {
            std::process::Command::new(candidate).arg("--version").output().is_ok()
        })
    }

    #[tokio::test]
    async fn initialize_and_tools_list_round_trip_against_a_real_subprocess() {
        let Some(python) = python_available() else {
            eprintln!("skipping: no python3/python on PATH");
            return;
        };
        let script_path = std::env::temp_dir()
            .join(format!("fleet-snowfluff-mcp-fixture-{}.py", std::process::id()));
        std::fs::write(&script_path, FIXTURE_SCRIPT).unwrap();

        let transport =
            StdioTransport::spawn(python, &[script_path.to_string_lossy().to_string()], &[])
                .expect("fixture script should spawn");
        let client = McpClient::new(Arc::new(transport));

        client.initialize().await.expect("initialize should succeed");
        let tools = client.list_tools().await.expect("tools/list should succeed");

        std::fs::remove_file(&script_path).ok();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
    }

    #[tokio::test]
    async fn dropping_the_transport_terminates_the_subprocess() {
        // `cat` echoes stdin back on stdout forever -- it never replies
        // with valid JSON-RPC, so this only exercises kill-on-drop, not
        // a protocol round trip.
        let transport = StdioTransport::spawn("cat", &[], &[]).expect("cat should spawn");
        let pid = transport._child.lock().await.id().expect("a running child has a pid");
        let is_running = |pid: u32| {
            let out = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .expect("ps should be available");
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        };
        assert!(is_running(pid), "cat should be running before the drop");

        drop(transport);

        let mut gone = false;
        for _ in 0..50 {
            if !is_running(pid) {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        assert!(gone, "process {pid} still running after its StdioTransport was dropped");
    }
}
