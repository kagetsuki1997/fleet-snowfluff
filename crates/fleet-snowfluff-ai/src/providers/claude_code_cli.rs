//! `ClaudeCodeCli`: Anthropic's `Subscription` auth method, implemented
//! by spawning `claude -p` (Claude Code's own non-interactive mode)
//! rather than calling the Messages API directly. This is not a style
//! choice -- an earlier version of this change had `Anthropic` send a
//! `claude setup-token` bearer token straight to `/v1/messages`, which
//! is both rejected by the API and a Consumer Terms of Service
//! violation for subscription-sourced OAuth tokens (they are scoped to
//! Claude Code and claude.ai only). This implementation follows
//! OpenClaw's own approach instead: shell out to the already-logged-in
//! `claude` CLI and let it own the credential entirely
//! (`subscription-first-chat`'s "No persisted credential for
//! subscription auth" -- Fleet never reads, stores, or mints a token).
//!
//! Also follows OpenClaw's warm-session model: a Claude-CLI session is
//! resumed across turns (`--resume <id>`) rather than started fresh
//! every message, since a fresh session's context/tool-definition setup
//! is real, measured overhead (see design.md) that a resumed session
//! mostly avoids. The session id lives only in this struct's own
//! runtime state (`Arc<Mutex<Option<String>>>`, exposed via
//! `session_id()`, not the `AiProvider` trait) -- the app crate reads
//! it after a generation completes and supplies it back in on the next
//! `ClaudeCodeCli` it constructs for the same (Fleet chat session,
//! profile) pair. Never written to disk.
//!
//! The CLI's own default system prompt, project-settings/hooks, and
//! MCP servers are all suppressed (`--system-prompt`, `--setting-sources
//! ""`, `--strict-mcp-config`) so a call behaves as plain persona chat,
//! not a coding-agent session -- verified live during design: without
//! this, a trivial reply picked up ~13K tokens of unrelated
//! project-hook context; with it, ~2.8K (baseline harness overhead that
//! doesn't fully go away, see design.md Risks). `--disallowedTools`
//! blocks the built-in file/shell/task tools so a chat reply can't
//! wander into agentic side effects.

use std::{
    process::Stdio,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

use crate::{
    message::{Message, ModelInfo, ProviderError, ProviderKind, Role, StreamChunk},
    provider::{AiProvider, ChatStream},
    providers::{anthropic_stream_event, cli_process},
};

/// Built-in tools a plain chat reply has no business reaching for.
/// `SlashCommand` is included because `-p` mode still resolves
/// `/skill-name` invocations otherwise.
const DISALLOWED_TOOLS: &str =
    "Bash,Read,Write,Edit,Glob,Grep,WebFetch,WebSearch,NotebookEdit,Task,TodoWrite,SlashCommand";

#[derive(Debug, Deserialize)]
struct AuthStatus {
    #[serde(rename = "loggedIn")]
    logged_in: bool,
}

/// Parses `claude auth status --json`'s stdout. Only `loggedIn` is
/// read -- every other field (`authMethod`, `email`, `subscriptionType`,
/// ...) is informational and not needed to answer "can we make a
/// request right now." The shape below was verified live during
/// discovery against a real, logged-in installation -- real ground
/// truth, not a guess.
fn parse_auth_status(stdout: &str) -> Result<bool, ProviderError> {
    let status: AuthStatus = serde_json::from_str(stdout).map_err(|e| {
        ProviderError::InvalidResponse(format!("malformed `claude auth status` output: {e}"))
    })?;
    Ok(status.logged_in)
}

/// Runs `claude auth status --json` and returns `Ok(())` only if
/// logged in. Called before every `chat()` rather than cached, per
/// `subscription-first-chat`'s "No persisted credential for
/// subscription auth" -- Fleet never stores a token or a login status,
/// it asks the CLI fresh every time, matching OpenClaw's own stated
/// principle: "Claude owns the login and token refresh lifecycle." A
/// `claude auth logout` run in a terminal takes effect on Fleet's very
/// next request.
async fn check_logged_in() -> Result<(), ProviderError> {
    let output = Command::new("claude")
        .args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| cli_process::map_spawn_error("claude", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ProviderError::SubscriptionExpired(format!(
            "`claude auth status` exited with an error: {}",
            if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() }
        )));
    }

    match parse_auth_status(&stdout) {
        Ok(true) => Ok(()),
        Ok(false) => Err(ProviderError::SubscriptionExpired(
            "not logged in to Claude -- run `claude auth login`".to_string(),
        )),
        Err(e) => Err(e),
    }
}

pub struct ClaudeCodeCli {
    pub model: Option<String>,
    session_id: Arc<Mutex<Option<String>>>,
}

impl ClaudeCodeCli {
    /// `resume_session_id` is the id returned by a prior instance's
    /// `session_id()`, if any -- supplied by the app crate, never
    /// stored by this crate itself.
    pub fn new(model: Option<String>, resume_session_id: Option<String>) -> Self {
        Self { model, session_id: Arc::new(Mutex::new(resume_session_id)) }
    }

    /// The Claude-CLI session id captured from this instance's most
    /// recent `chat()` call, if any -- read by the app crate after a
    /// generation completes and threaded into the *next* `ClaudeCodeCli`
    /// for the same chat session, so it can `--resume`.
    pub fn session_id(&self) -> Option<String> { self.session_id.lock().unwrap().clone() }
}

fn system_prompt(messages: &[Message]) -> String {
    messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn latest_user_message(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

/// What one parsed line of `claude -p --output-format stream-json`
/// output means for the chat stream, separate from whether it also
/// carried a `session_id` (almost every line does).
#[derive(Debug, PartialEq)]
enum LineOutcome {
    Chunk(StreamChunk),
    /// A terminal `{"type":"result","is_error":true,...}` line.
    ResultError(ProviderError),
    Ignored,
}

struct ParsedLine {
    session_id: Option<String>,
    outcome: LineOutcome,
}

/// Best-effort classification of a `result`-line error message into a
/// `ProviderError` variant. **Assumption, not verified against a real
/// failure** -- only the success path (`is_error: false`) was observed
/// live during design; a real quota-exhausted/rate-limited/stale-resume
/// response was not. If real error text turns out to look different,
/// only this function needs to change.
fn classify_result_error(message: &str) -> ProviderError {
    let lower = message.to_lowercase();
    if lower.contains("resum") || lower.contains("session") {
        // Treated as a resume-specific failure by the caller (which
        // retries fresh) via the `ResultError` variant's message still
        // being inspected there; kept as `InvalidResponse` here so this
        // function's job stays "classify the text," not "decide the
        // retry policy."
        ProviderError::InvalidResponse(message.to_string())
    } else if lower.contains("quota") || lower.contains("usage limit") {
        ProviderError::QuotaExhausted(message.to_string())
    } else if lower.contains("rate limit") {
        ProviderError::RateLimited(message.to_string())
    } else if lower.contains("not logged in")
        || lower.contains("login")
        || lower.contains("unauthenticated")
    {
        ProviderError::SubscriptionExpired(message.to_string())
    } else {
        ProviderError::InvalidResponse(message.to_string())
    }
}

/// Pure parser for one already-read line of stdout -- the unit-tested
/// core, mirroring `anthropic.rs`'s `parse_sse_line` /
/// `providers/ollama.rs`'s NDJSON-line-parser precedent. Blank lines
/// and lines that aren't valid JSON at all are ignored rather than
/// treated as fatal, since `claude -p`'s output can include incidental
/// blank lines between JSON records.
fn parse_stream_json_line(line: &str) -> ParsedLine {
    let line = line.trim();
    if line.is_empty() {
        return ParsedLine { session_id: None, outcome: LineOutcome::Ignored };
    }

    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return ParsedLine { session_id: None, outcome: LineOutcome::Ignored };
    };

    let session_id = value.get("session_id").and_then(Value::as_str).map(str::to_string);

    let outcome = match value.get("type").and_then(Value::as_str) {
        Some("stream_event") => match value.get("event") {
            Some(event) => match anthropic_stream_event::extract_chunk(event) {
                Ok(Some(chunk)) => LineOutcome::Chunk(chunk),
                Ok(None) => LineOutcome::Ignored,
                Err(e) => LineOutcome::ResultError(e),
            },
            None => LineOutcome::Ignored,
        },
        Some("result") => {
            let is_error = value.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            if is_error {
                let message = value
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("claude -p reported an error")
                    .to_string();
                LineOutcome::ResultError(classify_result_error(&message))
            } else {
                LineOutcome::Ignored
            }
        }
        _ => LineOutcome::Ignored,
    };

    ParsedLine { session_id, outcome }
}

/// Whether a `ResultError`'s underlying message looks like it was
/// caused specifically by a bad `--resume` id, as opposed to any other
/// failure -- used to decide "retry fresh" vs. "propagate the error."
/// Same not-verified-against-a-real-failure caveat as
/// `classify_result_error`.
fn looks_resume_related(err: &ProviderError) -> bool {
    let ProviderError::InvalidResponse(msg) = err else { return false };
    let lower = msg.to_lowercase();
    lower.contains("resum") || lower.contains("session")
}

async fn spawn(
    model: Option<&str>,
    resume: Option<&str>,
    system_prompt: &str,
    prompt: &str,
) -> std::io::Result<tokio::process::Child> {
    let mut command = Command::new("claude");
    command
        .arg("-p")
        .args(["--system-prompt", system_prompt])
        .args(["--disallowedTools", DISALLOWED_TOOLS])
        .arg("--strict-mcp-config")
        .args(["--setting-sources", ""])
        .args(["--output-format", "stream-json"])
        .arg("--include-partial-messages")
        .arg("--verbose")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(model) = model {
        command.args(["--model", model]);
    }
    if let Some(id) = resume {
        command.args(["--resume", id]);
    }
    command.arg(prompt);
    command.spawn()
}

/// Reads one line from `lines`, mapping a read error to a
/// `ProviderError` and end-of-stream to `None` -- shared by the main
/// read loop and the "peek the first line" resume-retry check.
async fn read_line(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> Result<Option<String>, ProviderError> {
    lines.next_line().await.map_err(|e| ProviderError::Network(e.to_string()))
}

#[async_trait]
impl AiProvider for ClaudeCodeCli {
    fn kind(&self) -> ProviderKind { ProviderKind::Anthropic }

    /// Genuinely incremental: `claude -p`'s stdout is read line-by-line
    /// inside the returned stream's own generator (same shape as
    /// `providers/http_stream.rs::stream_lines`), not drained before
    /// returning -- a consumer sees each chunk as it's produced, not
    /// only once the whole reply is done.
    ///
    /// Resume fallback (`subscription-first-chat`'s "Session
    /// continuity" requirement): if `--resume` was used and the very
    /// first meaningful line is a resume-looking error with no chunks
    /// yielded yet, the stale child is killed and a fresh one spawned
    /// without `--resume`, transparently to the consumer -- it just
    /// sees the stream continue with real content. A resume failure
    /// that somehow occurs *after* content has already streamed is not
    /// retried (there is nothing sane to retry into at that point); it
    /// simply ends the stream with that error, like any other
    /// mid-stream failure.
    async fn chat(&self, messages: Vec<Message>) -> Result<ChatStream, ProviderError> {
        check_logged_in().await?;

        let system_prompt = system_prompt(&messages);
        let prompt = latest_user_message(&messages);
        let resume = self.session_id();
        let model = self.model.clone();
        let session_id_slot = self.session_id.clone();

        let mut child = spawn(model.as_deref(), resume.as_deref(), &system_prompt, &prompt)
            .await
            .map_err(|e| cli_process::map_spawn_error("claude", e))?;
        let mut lines =
            BufReader::new(child.stdout.take().expect("stdout was piped by `spawn`")).lines();

        // Peek the first line only when a resume was actually attempted
        // -- a fresh session (no `--resume`) has nothing to fall back
        // to, so there is no retry decision to make for it.
        let mut pending_first_chunk = None;
        if resume.is_some() {
            if let Some(line) = read_line(&mut lines).await? {
                let parsed = parse_stream_json_line(&line);
                if let LineOutcome::ResultError(err) = &parsed.outcome {
                    if looks_resume_related(err) {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        *session_id_slot.lock().unwrap() = None;
                        child = spawn(model.as_deref(), None, &system_prompt, &prompt)
                            .await
                            .map_err(|e| cli_process::map_spawn_error("claude", e))?;
                        lines = BufReader::new(
                            child.stdout.take().expect("stdout was piped by `spawn`"),
                        )
                        .lines();
                    }
                } else {
                    if let Some(id) = parsed.session_id {
                        *session_id_slot.lock().unwrap() = Some(id);
                    }
                    if let LineOutcome::Chunk(chunk) = parsed.outcome {
                        pending_first_chunk = Some(chunk);
                    }
                }
            }
        }

        let stream = async_stream::stream! {
            let _child = child; // kept alive for the duration of the stream
            if let Some(chunk) = pending_first_chunk {
                yield Ok(chunk);
            }
            loop {
                let line = match read_line(&mut lines).await {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };
                let parsed = parse_stream_json_line(&line);
                if let Some(id) = parsed.session_id {
                    *session_id_slot.lock().unwrap() = Some(id);
                }
                match parsed.outcome {
                    LineOutcome::Chunk(chunk) => yield Ok(chunk),
                    LineOutcome::ResultError(err) => {
                        yield Err(err);
                        return;
                    }
                    LineOutcome::Ignored => {}
                }
            }
        };
        Ok(Box::pin(stream))
    }

    /// `claude -p` has no live model-listing surface of its own -- no
    /// `/models`-style endpoint exists for a CLI, unlike the HTTP
    /// providers `ai-provider`'s "Live model listing" requirement was
    /// originally written for. Returning an empty list here (as a first
    /// pass did) left the settings UI with no way to pick a model at
    /// all for this profile. Instead: the curated set of aliases
    /// Claude Code itself offers for model selection. Leaving the
    /// profile's model unset is still valid and means "no `--model`
    /// flag, Claude Code's own default."
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(CLAUDE_MODEL_ALIASES
            .iter()
            .map(|&(id, display_name)| ModelInfo {
                id: id.to_string(),
                display_name: display_name.to_string(),
            })
            .collect())
    }
}

/// `claude --help`'s own `--model` flag documentation (Claude Code
/// v2.1.275) gives "'fable', 'opus', or 'sonnet'" as examples, not an
/// exhaustive list -- confirmed separately that `haiku` is also a valid
/// alias (it appears in Claude Code's own `/model` picker). Not a live
/// list -- there is nothing to fetch it from -- so kept as a small,
/// explicitly-sourced constant rather than an open-ended guess at every
/// alias that might exist.
const CLAUDE_MODEL_ALIASES: &[(&str, &str)] = &[
    ("sonnet", "Sonnet (latest)"),
    ("opus", "Opus (latest)"),
    ("fable", "Fable (latest)"),
    ("haiku", "Haiku (latest)"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_system_prompt_and_latest_user_message() {
        let messages = vec![
            Message::system("be brief"),
            Message::user("first"),
            Message::assistant("ok"),
            Message::user("second"),
        ];
        assert_eq!(system_prompt(&messages), "be brief");
        assert_eq!(latest_user_message(&messages), "second");
    }

    // -- Literal fixtures captured live this session from
    // `claude -p --output-format stream-json --include-partial-messages`
    // -- real ground truth, not a guess (unlike the result-error
    // fixtures further down, which are constructed to match the
    // documented shape but were not observed live).

    const LIVE_MESSAGE_START: &str = r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-sonnet-5","id":"msg_1","type":"message","role":"assistant","content":[]}},"session_id":"559728db-b717-4102-a7d7-e838278c5091"}"#;
    const LIVE_CONTENT_DELTA: &str = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Snow"}},"session_id":"559728db-b717-4102-a7d7-e838278c5091"}"#;
    const LIVE_RESULT_SUCCESS: &str = r#"{"is_error":false,"subtype":"success","result":"Hi!","type":"result","session_id":"559728db-b717-4102-a7d7-e838278c5091"}"#;

    #[test]
    fn parses_live_message_start_as_ignored_but_captures_session_id() {
        let parsed = parse_stream_json_line(LIVE_MESSAGE_START);
        assert_eq!(parsed.session_id.as_deref(), Some("559728db-b717-4102-a7d7-e838278c5091"));
        assert_eq!(parsed.outcome, LineOutcome::Ignored);
    }

    #[test]
    fn parses_live_content_delta_as_a_chunk() {
        let parsed = parse_stream_json_line(LIVE_CONTENT_DELTA);
        assert_eq!(parsed.session_id.as_deref(), Some("559728db-b717-4102-a7d7-e838278c5091"));
        assert_eq!(parsed.outcome, LineOutcome::Chunk(StreamChunk { delta: "Snow".to_string() }));
    }

    #[test]
    fn parses_live_successful_result_as_ignored_not_an_error() {
        let parsed = parse_stream_json_line(LIVE_RESULT_SUCCESS);
        assert_eq!(parsed.outcome, LineOutcome::Ignored);
    }

    #[test]
    fn blank_lines_are_ignored() {
        assert_eq!(parse_stream_json_line("").outcome, LineOutcome::Ignored);
        assert_eq!(parse_stream_json_line("   ").outcome, LineOutcome::Ignored);
    }

    #[test]
    fn malformed_json_is_ignored_not_fatal() {
        assert_eq!(parse_stream_json_line("not json").outcome, LineOutcome::Ignored);
    }

    #[test]
    fn a_full_captured_event_sequence_yields_exactly_the_visible_text() {
        // The exact sequence (minus surrounding system/rate_limit/text
        // duplicate lines, which are all `Ignored`) captured from the
        // second live test this session.
        let lines = [
            LIVE_MESSAGE_START,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},"session_id":"s1"}"#,
            LIVE_CONTENT_DELTA,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" is"}},"session_id":"s1"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0},"session_id":"s1"}"#,
            LIVE_RESULT_SUCCESS,
        ];
        let mut text = String::new();
        let mut session_id = None;
        for line in lines {
            let parsed = parse_stream_json_line(line);
            if parsed.session_id.is_some() {
                session_id = parsed.session_id;
            }
            if let LineOutcome::Chunk(chunk) = parsed.outcome {
                text.push_str(&chunk.delta);
            }
        }
        assert_eq!(text, "Snow is");
        assert!(session_id.is_some());
    }

    // -- Result-error fixtures: constructed to the documented
    // `{"type":"result","is_error":true,...}` shape, not observed live
    // (see `classify_result_error`'s doc comment).

    #[test]
    fn quota_exhausted_result_error_is_classified_distinctly_from_rate_limited() {
        let quota = classify_result_error("You've reached your usage limit for this period.");
        let rate = classify_result_error("Rate limit exceeded, please slow down.");
        assert!(matches!(quota, ProviderError::QuotaExhausted(_)));
        assert!(matches!(rate, ProviderError::RateLimited(_)));
        assert_ne!(quota.to_string(), rate.to_string());
    }

    #[test]
    fn not_logged_in_result_error_maps_to_subscription_expired() {
        let err = classify_result_error("You are not logged in. Run `claude auth login`.");
        assert!(matches!(err, ProviderError::SubscriptionExpired(_)));
    }

    #[test]
    fn resume_failure_is_detected_for_the_retry_decision() {
        let err = classify_result_error("Could not resume session: not found.");
        assert!(looks_resume_related(&err));

        let unrelated = classify_result_error("Internal server error.");
        assert!(!looks_resume_related(&unrelated));
    }

    #[test]
    fn a_result_line_error_is_surfaced_as_a_result_error_outcome() {
        let line = r#"{"type":"result","is_error":true,"result":"Rate limit exceeded, please slow down.","session_id":"s1"}"#;
        let parsed = parse_stream_json_line(line);
        assert!(matches!(parsed.outcome, LineOutcome::ResultError(ProviderError::RateLimited(_))));
    }

    /// Manual, subscription-gated round trip against the real `claude`
    /// CLI on this machine -- never run in CI. Exercises the actual
    /// implementation (not just a hand-verified raw CLI invocation):
    /// two messages in a row, asserting the second constructs with the
    /// first's session id (real session-resume, not just the unit-level
    /// "would pass the flag" check).
    /// Run manually: `cargo test -p fleet-snowfluff-ai
    /// --test-threads=1 -- --ignored claude_code_cli_live`.
    #[tokio::test]
    #[ignore = "requires the `claude` CLI and a real Claude subscription"]
    async fn claude_code_cli_live_two_turn_session_resume() {
        use futures_util::StreamExt;

        let first = ClaudeCodeCli::new(None, None);
        let mut stream = first
            .chat(vec![
                Message::system("You are a cheerful desktop pet. Reply in one short sentence."),
                Message::user("Say hi."),
            ])
            .await
            .unwrap();
        let mut reply = String::new();
        while let Some(chunk) = stream.next().await {
            reply.push_str(&chunk.unwrap().delta);
        }
        assert!(!reply.is_empty(), "expected a non-empty first reply");
        let session_id = first.session_id();
        assert!(session_id.is_some(), "expected a session id to be captured from the live stream");

        let second = ClaudeCodeCli::new(None, session_id.clone());
        let mut stream = second
            .chat(vec![
                Message::system("You are a cheerful desktop pet. Reply in one short sentence."),
                Message::user("What did I just ask you to do?"),
            ])
            .await
            .unwrap();
        let mut reply2 = String::new();
        while let Some(chunk) = stream.next().await {
            reply2.push_str(&chunk.unwrap().delta);
        }
        assert!(!reply2.is_empty(), "expected a non-empty second reply");
        assert_eq!(
            second.session_id(),
            session_id,
            "resuming should keep the same session id, not mint a new one"
        );
    }

    // -- `claude auth status` parsing --

    /// Literal fixture captured live from `claude auth status --json`
    /// (Claude Code v2.1.275) during discovery -- real ground truth,
    /// not a guess.
    const LIVE_LOGGED_IN_FIXTURE: &str = r#"{
        "loggedIn": true,
        "authMethod": "claude.ai",
        "apiProvider": "firstParty",
        "analyticsDisabled": false,
        "projectsDirectory": "/Users/example/.claude/projects",
        "configDirectory": "/Users/example/.claude",
        "email": "user@example.com",
        "orgId": "00000000-0000-0000-0000-000000000000",
        "orgName": "user@example.com's Organization",
        "subscriptionType": "pro"
    }"#;

    #[test]
    fn parses_the_live_verified_logged_in_shape() {
        assert_eq!(parse_auth_status(LIVE_LOGGED_IN_FIXTURE).unwrap(), true);
    }

    #[test]
    fn parses_a_logged_out_status() {
        assert_eq!(parse_auth_status(r#"{"loggedIn": false}"#).unwrap(), false);
    }

    #[tokio::test]
    async fn list_models_offers_the_known_aliases_not_an_empty_list() {
        let models = ClaudeCodeCli::new(None, None).list_models().await.unwrap();
        assert!(!models.is_empty(), "settings UI needs something to show in the model picker");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"sonnet"));
        assert!(ids.contains(&"opus"));
        assert!(ids.contains(&"fable"));
        assert!(ids.contains(&"haiku"));
    }

    #[test]
    fn malformed_status_output_is_invalid_response_not_a_panic() {
        let err = parse_auth_status("not json").unwrap_err();
        assert!(matches!(err, ProviderError::InvalidResponse(_)));
    }
}
