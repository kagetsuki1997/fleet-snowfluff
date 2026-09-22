//! `web_search`: the Ollama `ToolCallingProvider` path's only native
//! tool that talks to a third-party network service. Scoped explicitly
//! to that path -- Claude Code/Codex get their own first-party search
//! for free via the native-tool allow-list instead (Group 7); this is
//! not a general Aemeath search feature.
//!
//! Two-tier, no-key, no-login fallback: a public SearXNG instance first
//! (real ranked results, unofficial/no-SLA infrastructure), falling
//! back to the official DuckDuckGo Instant Answer API if unreachable
//! or unparseable. If both fail, returns an honest "no results"
//! `ToolResult` (`is_error: false`) rather than erroring the tool call
//! -- a SearXNG outage degrades this tool's quality, not its
//! availability (design.md's Risks).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    agent_tool::{PermissionTier, Tool, ToolContext, ToolError, ToolResult},
    tool_provider::ToolDefinition,
};

/// A well-known public instance -- there is no settings surface to
/// configure this in this change, so a SearXNG outage falls straight
/// through to the DuckDuckGo tier below rather than to a user-editable
/// alternative.
const SEARXNG_BASE_URL: &str = "https://searx.be";
const DUCKDUCKGO_URL: &str = "https://api.duckduckgo.com/";
const MAX_RESULTS: usize = 5;

/// A real desktop-browser User-Agent -- `reqwest::Client::new()` sends
/// none at all by default, and a bare/missing User-Agent is itself a
/// simple, common bot-filter trigger independent of anything more
/// sophisticated. Confirmed live (this session, via `curl` with this
/// exact UA string) that this alone does **not** get past the JS
/// proof-of-work/CAPTCHA challenge most public SearXNG instances run
/// today (`searx.be` included) -- that class of gate can only be
/// solved by executing JavaScript, which no HTTP client here does, so
/// this is worth sending as basic hygiene, not a fix for that problem.
/// See `parse_searxng_results`'s doc comment and the live tests below
/// for how that's actually handled (graceful degrade, not a retry
/// strategy this module can win).
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                                  AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 \
                                  Safari/537.36";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// SearXNG's `?format=json` response shape -- only the fields this
/// tool actually uses. A response this can't parse (wrong shape, HTML
/// instead of JSON because an instance disabled the JSON API -- or, as
/// observed live against every public instance tried this session, an
/// anti-bot JS-challenge/CAPTCHA/"you're rate-limited" HTML page served
/// with a `200 OK` instead of the real search results) is treated the
/// same as an empty result set, not an error -- the caller falls
/// through to DuckDuckGo either way. See `live_searxng_...` in this
/// module's tests for how to check a given instance's current state by
/// hand.
fn parse_searxng_results(body: &str) -> Vec<SearchResult> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        results: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        #[serde(default)]
        title: String,
        #[serde(default)]
        url: String,
        #[serde(default)]
        content: String,
    }

    let Ok(envelope) = serde_json::from_str::<Envelope>(body) else { return vec![] };
    envelope
        .results
        .into_iter()
        .take(MAX_RESULTS)
        .map(|entry| SearchResult { title: entry.title, url: entry.url, snippet: entry.content })
        .collect()
}

/// DuckDuckGo's Instant Answer API shape -- narrow (infoboxes/
/// definitions/disambiguation, not full search results, per this
/// tool's own two-tier design), mapped into the same `SearchResult`
/// shape SearXNG produces so `format_results` doesn't need to know
/// which tier answered.
fn parse_duckduckgo_results(body: &str) -> Vec<SearchResult> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default, rename = "AbstractText")]
        abstract_text: String,
        #[serde(default, rename = "AbstractURL")]
        abstract_url: String,
        #[serde(default, rename = "Heading")]
        heading: String,
        #[serde(default, rename = "RelatedTopics")]
        related_topics: Vec<RelatedTopic>,
    }
    #[derive(Deserialize)]
    struct RelatedTopic {
        #[serde(default, rename = "Text")]
        text: String,
        #[serde(default, rename = "FirstURL")]
        first_url: String,
    }

    let Ok(envelope) = serde_json::from_str::<Envelope>(body) else { return vec![] };
    let mut results = Vec::new();
    if !envelope.abstract_text.is_empty() {
        let title =
            if envelope.heading.is_empty() { "Summary".to_string() } else { envelope.heading };
        results.push(SearchResult {
            title,
            url: envelope.abstract_url,
            snippet: envelope.abstract_text,
        });
    }
    for topic in envelope.related_topics {
        if results.len() >= MAX_RESULTS {
            break;
        }
        if topic.text.is_empty() {
            continue;
        }
        results.push(SearchResult {
            title: topic.text.clone(),
            url: topic.first_url,
            snippet: topic.text,
        });
    }
    results
}

fn format_results(results: &[SearchResult]) -> String {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}. {}\n{}\n{}", i + 1, r.title, r.url, r.snippet))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The actual HTTP transport, abstracted behind a trait so the
/// try-SearXNG-then-DuckDuckGo-then-honest-empty orchestration in
/// [`search`] is testable without a real network call (mirrors
/// `agent_runtime.rs`'s own `ScriptedProvider` test-double pattern).
#[async_trait]
trait SearchTransport: Send + Sync {
    async fn fetch_searxng(&self, query: &str) -> Result<String, String>;
    async fn fetch_duckduckgo(&self, query: &str) -> Result<String, String>;
}

struct ReqwestSearchTransport {
    client: reqwest::Client,
}

impl Default for ReqwestSearchTransport {
    /// Builds the shared client once with `BROWSER_USER_AGENT` set --
    /// `reqwest::Client::new()`'s bare default (no `User-Agent` header
    /// at all) is worth avoiding regardless of whether it changes the
    /// outcome for any particular instance (see that constant's doc
    /// comment). Falls back to `reqwest::Client::new()`'s own default
    /// only if the builder somehow fails (it can't, for a plain
    /// `user_agent()` call with no TLS/proxy config involved -- this is
    /// just avoiding an `.unwrap()` for a call that cannot practically
    /// panic).
    fn default() -> Self {
        let client =
            reqwest::Client::builder().user_agent(BROWSER_USER_AGENT).build().unwrap_or_default();
        Self { client }
    }
}

/// Appends `pairs` to `url`'s query string via `url::Url` (re-exported
/// as `reqwest::Url`) rather than `RequestBuilder::query`, which needs
/// reqwest's optional `query` feature -- not enabled workspace-wide,
/// and every other provider in this codebase already builds its own
/// URLs by hand rather than pulling that feature in for one call site.
fn url_with_query(base: &str, pairs: &[(&str, &str)]) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(base).map_err(|e| e.to_string())?;
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in pairs {
            query.append_pair(key, value);
        }
    }
    Ok(url)
}

#[async_trait]
impl SearchTransport for ReqwestSearchTransport {
    async fn fetch_searxng(&self, query: &str) -> Result<String, String> {
        let url = url_with_query(
            &format!("{SEARXNG_BASE_URL}/search"),
            &[("q", query), ("format", "json")],
        )?;
        self.client
            .get(url)
            // On top of `?format=json`, some instances also gate on the
            // `Accept` header before deciding whether to serve JSON at
            // all versus redirecting to an HTML/challenge page.
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())
    }

    async fn fetch_duckduckgo(&self, query: &str) -> Result<String, String> {
        let url = url_with_query(
            DUCKDUCKGO_URL,
            &[("q", query), ("format", "json"), ("no_html", "1"), ("skip_disambig", "1")],
        )?;
        self.client
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())
    }
}

/// The two-tier fallback logic itself, generic over the transport so
/// it's directly testable. Never returns `is_error: true` -- a search
/// failure is reported honestly as "no results" for the model to react
/// to, not as a tool error that would fail the turn.
async fn search(transport: &dyn SearchTransport, query: &str) -> ToolResult {
    if let Ok(body) = transport.fetch_searxng(query).await {
        let results = parse_searxng_results(&body);
        if !results.is_empty() {
            return ToolResult::ok(format_results(&results));
        }
    }
    if let Ok(body) = transport.fetch_duckduckgo(query).await {
        let results = parse_duckduckgo_results(&body);
        if !results.is_empty() {
            return ToolResult::ok(format_results(&results));
        }
    }
    ToolResult::ok("No search results available.")
}

pub struct WebSearchTool {
    transport: Box<dyn SearchTransport>,
}

impl Default for WebSearchTool {
    fn default() -> Self { Self { transport: Box::new(ReqwestSearchTransport::default()) } }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".to_string(),
            description: "Searches the web for current information. No credential required."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": { "query": { "type": "string", "description": "The search query" } },
                "required": ["query"],
            }),
        }
    }

    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Auto
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError("missing \"query\" argument".to_string()))?;
        Ok(search(self.transport.as_ref(), query).await)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::conversation::ConversationId;

    fn ctx() -> ToolContext {
        ToolContext {
            project_root: None,
            conversation_id: ConversationId::from_session_path(std::path::Path::new("/tmp/x")),
        }
    }

    const SEARXNG_BODY: &str = r#"{"results":[{"title":"Rust","url":"https://rust-lang.org","content":"A systems language"}]}"#;
    const DUCKDUCKGO_BODY: &str = r#"{"AbstractText":"Rust is a language","AbstractURL":"https://duckduckgo.com/Rust","Heading":"Rust","RelatedTopics":[]}"#;
    const EMPTY_SEARXNG_BODY: &str = r#"{"results":[]}"#;
    const EMPTY_DUCKDUCKGO_BODY: &str = r#"{"AbstractText":"","RelatedTopics":[]}"#;

    struct ScriptedTransport {
        searxng: Mutex<Option<Result<String, String>>>,
        duckduckgo: Mutex<Option<Result<String, String>>>,
    }

    #[async_trait]
    impl SearchTransport for ScriptedTransport {
        async fn fetch_searxng(&self, _query: &str) -> Result<String, String> {
            self.searxng.lock().unwrap().take().expect("fetch_searxng called unexpectedly")
        }

        async fn fetch_duckduckgo(&self, _query: &str) -> Result<String, String> {
            self.duckduckgo.lock().unwrap().take().expect("fetch_duckduckgo called unexpectedly")
        }
    }

    #[test]
    fn parse_searxng_results_extracts_title_url_and_snippet() {
        let results = parse_searxng_results(SEARXNG_BODY);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust");
        assert_eq!(results[0].url, "https://rust-lang.org");
    }

    #[test]
    fn parse_duckduckgo_results_uses_the_abstract_when_present() {
        let results = parse_duckduckgo_results(DUCKDUCKGO_BODY);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust");
        assert_eq!(results[0].snippet, "Rust is a language");
    }

    #[test]
    fn an_unparseable_body_yields_no_results_not_an_error() {
        assert!(parse_searxng_results("not json").is_empty());
        assert!(parse_duckduckgo_results("<html>blocked</html>").is_empty());
    }

    #[tokio::test]
    async fn searxng_success_is_used_without_calling_duckduckgo() {
        let transport = ScriptedTransport {
            searxng: Mutex::new(Some(Ok(SEARXNG_BODY.to_string()))),
            duckduckgo: Mutex::new(Some(Err("must not be called".to_string()))),
        };
        let result = search(&transport, "rust").await;
        assert!(!result.is_error);
        assert!(result.content.contains("rust-lang.org"));
    }

    #[tokio::test]
    async fn searxng_failure_falls_back_to_duckduckgo_success() {
        let transport = ScriptedTransport {
            searxng: Mutex::new(Some(Err("connection refused".to_string()))),
            duckduckgo: Mutex::new(Some(Ok(DUCKDUCKGO_BODY.to_string()))),
        };
        let result = search(&transport, "rust").await;
        assert!(!result.is_error);
        assert!(result.content.contains("Rust is a language"));
    }

    #[tokio::test]
    async fn searxng_empty_results_also_falls_back_to_duckduckgo() {
        let transport = ScriptedTransport {
            searxng: Mutex::new(Some(Ok(EMPTY_SEARXNG_BODY.to_string()))),
            duckduckgo: Mutex::new(Some(Ok(DUCKDUCKGO_BODY.to_string()))),
        };
        let result = search(&transport, "rust").await;
        assert!(result.content.contains("Rust is a language"));
    }

    #[tokio::test]
    async fn both_backends_failing_returns_an_honest_no_results_message_not_an_error() {
        let transport = ScriptedTransport {
            searxng: Mutex::new(Some(Err("timed out".to_string()))),
            duckduckgo: Mutex::new(Some(Err("timed out".to_string()))),
        };
        let result = search(&transport, "rust").await;
        assert!(!result.is_error, "a search outage must not fail the tool call");
        assert_eq!(result.content, "No search results available.");
    }

    #[tokio::test]
    async fn both_backends_returning_empty_results_also_yields_the_honest_message() {
        let transport = ScriptedTransport {
            searxng: Mutex::new(Some(Ok(EMPTY_SEARXNG_BODY.to_string()))),
            duckduckgo: Mutex::new(Some(Ok(EMPTY_DUCKDUCKGO_BODY.to_string()))),
        };
        let result = search(&transport, "rust").await;
        assert!(!result.is_error);
        assert_eq!(result.content, "No search results available.");
    }

    #[test]
    fn web_search_requires_no_credential_and_is_auto_permitted() {
        let tool = WebSearchTool::default();
        assert_eq!(tool.required_permission(&json!({}), &ctx()), PermissionTier::Auto);
    }

    #[tokio::test]
    async fn execute_reports_a_model_visible_error_for_a_missing_query() {
        let tool = WebSearchTool::default();
        let err = tool.execute(json!({}), &ctx()).await.unwrap_err();
        assert_eq!(err.to_string(), "missing \"query\" argument");
    }

    // -- Live network tests (agent-core-and-task-router follow-up) --
    //
    // `#[ignore]`d, same convention as this crate's other tests that
    // depend on real external infrastructure (e.g.
    // `task_router_rules_live_classification_round_trip`, the
    // `claude_code_cli`/`codex` CLI-dependent tests) -- these hit real,
    // third-party services this project doesn't control, so they don't
    // run in normal `cargo test` or CI. Run by hand with:
    //   cargo test -p fleet-snowfluff-ai native_tools::web_search -- --ignored
    // A failure here reflects that external service's *current* state
    // (bot-mitigation, rate-limiting, an API's inherent narrowness),
    // not necessarily a bug in this module -- see each test's own doc
    // comment for what it actually demonstrates.

    /// As of this test's writing, every public SearXNG instance tried
    /// by hand (`searx.be` plus roughly a dozen others commonly listed
    /// as JSON-API-enabled) returned either an anti-bot JS-challenge/
    /// CAPTCHA page (confirmed with a real Chrome `User-Agent`, so this
    /// is not a simple UA-sniffing block a header can fix) or an HTTP
    /// `429`. This test documents that reality rather than asserting a
    /// specific outcome: it only requires the request to succeed at the
    /// HTTP transport level (`fetch_searxng` returning `Ok`, i.e. no
    /// connection/timeout failure) and prints whether the body actually
    /// parsed into real results, so a human running it can see the
    /// instance's current state without the test itself being flaky
    /// (an `assert!(!results.is_empty())` here would fail today through
    /// no fault of this module's code, on infrastructure this project
    /// doesn't control).
    #[tokio::test]
    #[ignore = "hits a real, third-party SearXNG instance -- run manually"]
    async fn live_searxng_reports_its_current_reachability_and_parse_result() {
        let transport = ReqwestSearchTransport::default();
        let body = transport
            .fetch_searxng("rust programming language")
            .await
            .expect("the HTTP request itself should succeed even if SearXNG blocks the query");
        let results = parse_searxng_results(&body);
        eprintln!(
            "live SearXNG ({SEARXNG_BASE_URL}) returned {} parsed result(s); first 200 chars of \
             body: {:?}",
            results.len(),
            body.chars().take(200).collect::<String>()
        );
    }

    /// A Wikipedia-entity-style query is the one shape the DuckDuckGo
    /// Instant Answer API reliably answers -- confirmed live returning
    /// a real `AbstractText`/`Heading` for exactly this query, with or
    /// without a custom `User-Agent`, so headers were never the issue
    /// for this tier.
    #[tokio::test]
    #[ignore = "hits the real DuckDuckGo Instant Answer API -- run manually"]
    async fn live_duckduckgo_returns_an_abstract_for_a_wikipedia_style_entity_query() {
        let transport = ReqwestSearchTransport::default();
        let body = transport
            .fetch_duckduckgo("rust programming language")
            .await
            .expect("DuckDuckGo request should succeed at the HTTP level");
        let results = parse_duckduckgo_results(&body);
        assert!(
            !results.is_empty(),
            "expected a Wikipedia-backed abstract for an entity-style query; got body: {body}"
        );
    }

    /// Documents, rather than reports as a bug, what actually produced
    /// the "test message" this test was written to explain: for most
    /// ordinary natural-language queries (not a Wikipedia-style
    /// entity), the Instant Answer API responds `200 OK` with every
    /// content field empty and a `meta` object literally describing
    /// itself as `"name":"Just Another Test"`/`"description":"testing"`
    /// -- a real, documented placeholder response for "no instant
    /// answer for this query," not a bot-block and not something a
    /// header changes. This module's parser only reads
    /// `AbstractText`/`Heading`/`RelatedTopics` (never `meta`), so that
    /// placeholder already can't leak into a user-visible result --
    /// this test pins that down against the real API instead of only a
    /// hand-written fixture.
    #[tokio::test]
    #[ignore = "hits the real DuckDuckGo Instant Answer API -- run manually"]
    async fn live_duckduckgo_yields_no_parseable_results_for_a_generic_free_form_query() {
        let transport = ReqwestSearchTransport::default();
        let body = transport
            .fetch_duckduckgo("how to fix a leaky faucet")
            .await
            .expect("DuckDuckGo request should succeed at the HTTP level");
        let results = parse_duckduckgo_results(&body);
        assert!(
            results.is_empty(),
            "expected the narrow Instant Answer API to have nothing for a generic how-to query; \
             got: {results:?}"
        );
    }
}
