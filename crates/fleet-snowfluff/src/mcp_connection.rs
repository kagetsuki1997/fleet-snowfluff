//! [`McpConnectionState`]: app-lifetime live connections to connected
//! MCP servers, keyed by server id -- same tier as
//! `chat_commands::ChatRuntimeState`/
//! `tool_confirmation::ToolConfirmationState`. `mcp-client-support`'s own
//! design.md: "Eager-connect once, at add time; lazy reconnect after that" --
//! the *first* connection for a server is established eagerly by whatever
//! handles `connect_mcp_server`/the Settings-UI add-server flow (Group 4)
//! calling [`McpConnectionState::connection_for`] right after a fresh
//! `McpServerConfig` is persisted; every connection after that (app
//! restart, a connection that died mid-session) goes through the same
//! function's lazy-reconnect-on-miss branch -- there is no separate
//! "first connect" code path.

use std::{collections::HashMap, future::Future, sync::Arc};

use fleet_snowfluff_ai::{
    HttpTransport, McpClient, McpError, McpServerConfig, McpServerTransportConfig, McpTransport,
    StdioTransport,
};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct McpConnectionState {
    connections: Mutex<HashMap<String, Arc<McpClient>>>,
}

impl McpConnectionState {
    /// Returns `config.id`'s existing live connection, or establishes
    /// one from `config` (and `credential`, if this server has one) if
    /// there isn't one yet.
    pub async fn connection_for(
        &self,
        config: &McpServerConfig,
        credential: Option<&str>,
    ) -> Result<Arc<McpClient>, McpError> {
        let id = config.id.clone();
        self.get_or_establish(&id, || connect(config, credential)).await
    }

    /// Drops `server_id`'s live connection, if any -- removal (Group
    /// 4.4) calls this so the connection doesn't outlive the config
    /// entry it belonged to. Dropping the last `Arc<McpClient>` drops
    /// its underlying transport, which for a stdio connection
    /// terminates the subprocess via `StdioTransport`'s own
    /// `kill_on_drop` (proven in `mcp::stdio_transport`'s own tests) --
    /// this map is the *only* place a connection's `Arc` is held
    /// long-lived; every other reference (a `ToolRegistry` built for
    /// one turn) is transient and already gone by the time removal can
    /// even be requested.
    pub async fn remove(&self, server_id: &str) { self.connections.lock().await.remove(server_id); }

    async fn get_or_establish<F, Fut>(
        &self,
        server_id: &str,
        establish: F,
    ) -> Result<Arc<McpClient>, McpError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<McpClient, McpError>>,
    {
        {
            let connections = self.connections.lock().await;
            if let Some(client) = connections.get(server_id) {
                return Ok(client.clone());
            }
        }
        let client = Arc::new(establish().await?);
        self.connections.lock().await.insert(server_id.to_string(), client.clone());
        Ok(client)
    }
}

async fn connect(
    config: &McpServerConfig,
    credential: Option<&str>,
) -> Result<McpClient, McpError> {
    let transport: Arc<dyn McpTransport> = match &config.transport {
        McpServerTransportConfig::Stdio { command, args, credential_env_var } => {
            let mut env = Vec::new();
            if let (Some(name), Some(value)) = (credential_env_var, credential) {
                env.push((name.clone(), value.to_string()));
            }
            Arc::new(StdioTransport::spawn(command, args, &env)?)
        }
        McpServerTransportConfig::Http { url } => {
            Arc::new(HttpTransport::new(url.clone(), credential.map(str::to_string)))
        }
    };
    let client = McpClient::new(transport);
    client.initialize().await?;
    Ok(client)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::Value;

    use super::*;

    /// A trivial `McpTransport` double standing in for a real
    /// connection: counts how many times it was actually constructed
    /// (not how many times a lookup was attempted), so a test can
    /// assert a cache hit never re-establishes, and counts its own
    /// drops, so a test can assert removal actually releases the
    /// connection rather than merely forgetting the id.
    struct CountingTransport {
        drops: Arc<AtomicUsize>,
    }

    impl Drop for CountingTransport {
        fn drop(&mut self) { self.drops.fetch_add(1, Ordering::SeqCst); }
    }

    #[async_trait]
    impl McpTransport for CountingTransport {
        async fn request(&self, _method: &str, _params: Option<Value>) -> Result<Value, McpError> {
            Ok(Value::Null)
        }

        async fn notify(&self, _method: &str, _params: Option<Value>) -> Result<(), McpError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_second_lookup_for_the_same_id_hits_the_cache_instead_of_reestablishing() {
        let state = McpConnectionState::default();
        let establish_count = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));

        for _ in 0..2 {
            let establish_count = establish_count.clone();
            let drops = drops.clone();
            state
                .get_or_establish("server-a", || async move {
                    establish_count.fetch_add(1, Ordering::SeqCst);
                    Ok(McpClient::new(Arc::new(CountingTransport { drops })))
                })
                .await
                .unwrap();
        }

        assert_eq!(
            establish_count.load(Ordering::SeqCst),
            1,
            "the second lookup must hit the cache"
        );
    }

    #[tokio::test]
    async fn a_miss_for_a_different_id_establishes_its_own_connection() {
        let state = McpConnectionState::default();
        let establish_count = Arc::new(AtomicUsize::new(0));

        for id in ["server-a", "server-b"] {
            let establish_count = establish_count.clone();
            state
                .get_or_establish(id, || async move {
                    establish_count.fetch_add(1, Ordering::SeqCst);
                    Ok(McpClient::new(Arc::new(CountingTransport {
                        drops: Arc::new(AtomicUsize::new(0)),
                    })))
                })
                .await
                .unwrap();
        }

        assert_eq!(
            establish_count.load(Ordering::SeqCst),
            2,
            "each distinct id must establish its own"
        );
    }

    #[tokio::test]
    async fn removal_drops_the_connections_own_transport() {
        let state = McpConnectionState::default();
        let drops = Arc::new(AtomicUsize::new(0));
        state
            .get_or_establish("server-a", || async {
                Ok(McpClient::new(Arc::new(CountingTransport { drops: drops.clone() })))
            })
            .await
            .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0, "still live before removal");

        state.remove("server-a").await;

        assert_eq!(drops.load(Ordering::SeqCst), 1, "removal must drop the last reference");
    }

    #[tokio::test]
    async fn removing_an_unknown_id_is_a_harmless_no_op() {
        let state = McpConnectionState::default();
        state.remove("never-connected").await;
    }
}
