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
//! subscription auth" -- Aemeath never reads, stores, or mints a token).
//!
//! Also follows OpenClaw's warm-session model: a Claude-CLI session is
//! resumed across turns (`--resume <id>`) rather than started fresh
//! every message, since a fresh session's context/tool-definition setup
//! is real, measured overhead (see design.md) that a resumed session
//! mostly avoids. The session id lives only in this struct's own
//! runtime state (`Arc<Mutex<Option<String>>>`, exposed via
//! `session_id()`, not the `AiProvider` trait) -- the app crate reads
//! it after a generation completes and supplies it back in on the next
//! `ClaudeCodeCli` it constructs for the same (Aemeath chat session,
//! profile) pair. Never written to disk.
//!
//! The CLI's own default system prompt, project-settings/hooks, and
//! MCP servers are all suppressed (`--system-prompt`, `--setting-sources
//! ""`, `--strict-mcp-config`) so a call behaves as plain persona chat,
//! not a coding-agent session -- verified live during design: without
//! this, a trivial reply picked up ~13K tokens of unrelated
//! project-hook context; with it, ~2.8K (baseline harness overhead that
//! doesn't fully go away, see design.md Risks). `--allowedTools`/
//! `--disallowedTools`, built from the user-adjustable
//! `ClaudeCodeToolAccess` setting (`agent-core-and-task-router`'s
//! Group 7 -- replaces an earlier blanket `DISALLOWED_TOOLS` constant),
//! keep a chat reply from wandering into agentic side effects while
//! still letting read-only tools (including Claude's own first-party
//! web search) through by default.
//!
//! Every spawn resolves `claude` through `cli_locator::resolve` rather
//! than a bare `Command::new("claude")` (`agent-core-and-task-router`
//! Group 10) -- launched outside a terminal (Finder/Dock on macOS, a
//! `.desktop` file on Linux), Aemeath inherits a minimal `PATH` that
//! never sees the CLI's real install location; see that module's own
//! doc comment for the full per-platform reasoning.

use std::{
    path::PathBuf,
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
    providers::{
        anthropic_stream_event, cli_context::CliContext, cli_locator, cli_process, history_preamble,
    },
    settings::ClaudeCodeToolAccess,
};

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
/// subscription auth" -- Aemeath never stores a token or a login status,
/// it asks the CLI fresh every time, matching OpenClaw's own stated
/// principle: "Claude owns the login and token refresh lifecycle." A
/// `claude auth logout` run in a terminal takes effect on Aemeath's very
/// next request.
async fn check_logged_in() -> Result<(), ProviderError> {
    let output = Command::new(cli_locator::resolve("claude").await)
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
    tool_access: ClaudeCodeToolAccess,
    /// The conversation's real recent history (never the persona
    /// few-shot examples that `chat()`'s message list also carries),
    /// used to seed any request that isn't resuming a session -- see
    /// `history_preamble`.
    history: Vec<Message>,
    /// How many leading `history` messages a resumed session already
    /// holds; the rest are sent along on resume.
    seen_turns: usize,
    /// Where the CLI is run -- always explicit, never inherited from
    /// the app's launch directory (see `cli_workdir` in the app crate).
    working_dir: PathBuf,
}

impl ClaudeCodeCli {
    /// `cli.resume_session_id` is the id returned by a prior instance's
    /// `session_id()`, if any -- supplied by the app crate, never
    /// stored by this crate itself. The rest of `cli` (history, how much
    /// of it a resumed session has seen, the working directory) is
    /// described on [`CliContext`].
    pub fn new(model: Option<String>, cli: CliContext, tool_access: ClaudeCodeToolAccess) -> Self {
        Self {
            model,
            session_id: Arc::new(Mutex::new(cli.resume_session_id)),
            tool_access,
            history: cli.history,
            seen_turns: cli.seen_turns,
            working_dir: cli.working_dir,
        }
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

/// The prompt for one request. A resumed session already holds
/// `history[..seen_turns]`, so it gets the latest message plus any turns
/// after that it never saw; a request with no session to resume (first
/// message, stale-resume retry, escalation) is seeded from all of
/// `history`.
fn build_prompt(
    messages: &[Message],
    history: &[Message],
    seen_turns: usize,
    resuming: bool,
) -> String {
    history_preamble::prompt_for_session(
        history,
        seen_turns,
        &latest_user_message(messages),
        resuming,
    )
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

/// Builds the full `claude -p` argument list -- a pure function,
/// separated from the actual spawn, specifically so the
/// `--allowedTools`/`--disallowedTools` construction (task 7.1) is
/// directly testable without spawning a real process.
fn build_args(
    model: Option<&str>,
    resume: Option<&str>,
    system_prompt: &str,
    prompt: &str,
    tool_access: &ClaudeCodeToolAccess,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        "--system-prompt".to_string(),
        system_prompt.to_string(),
        "--strict-mcp-config".to_string(),
        "--setting-sources".to_string(),
        String::new(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--include-partial-messages".to_string(),
        "--verbose".to_string(),
    ];
    // Only passed when non-empty -- an explicit `--allowedTools ""` (or
    // `--disallowedTools ""`) is an unverified edge case not worth
    // risking when simply omitting the flag has an unambiguous meaning
    // (this tier has nothing in it).
    let allowed = tool_access.allowed_tools();
    if !allowed.is_empty() {
        args.push("--allowedTools".to_string());
        args.push(allowed);
    }
    let disallowed = tool_access.disallowed_tools();
    if !disallowed.is_empty() {
        args.push("--disallowedTools".to_string());
        args.push(disallowed);
    }
    if let Some(model) = model {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    if let Some(id) = resume {
        args.push("--resume".to_string());
        args.push(id.to_string());
    }
    // `--allowedTools`/`--disallowedTools` are variadic in the CLI: with
    // nothing between them and the prompt (no `--model`, no `--resume`)
    // the prompt is swallowed as one more tool name and the CLI exits
    // with "Input must be provided" -- an empty reply, from the caller's
    // side. `--` ends option parsing, which also keeps a prompt that
    // happens to start with `-` from being read as a flag.
    args.push("--".to_string());
    args.push(prompt.to_string());
    args
}

/// The fully-configured `claude -p` command for one chat request,
/// separated from the spawn itself so the working directory it will run
/// in is directly inspectable in tests without launching a process.
fn chat_command(
    program: impl AsRef<std::ffi::OsStr>,
    model: Option<&str>,
    resume: Option<&str>,
    system_prompt: &str,
    prompt: &str,
    tool_access: &ClaudeCodeToolAccess,
    working_dir: &std::path::Path,
) -> Command {
    let mut command = Command::new(program);
    command.args(build_args(model, resume, system_prompt, prompt, tool_access));
    cli_process::apply_chat_spawn_settings(&mut command, working_dir);
    command
}

async fn spawn(
    model: Option<&str>,
    resume: Option<&str>,
    system_prompt: &str,
    prompt: &str,
    tool_access: &ClaudeCodeToolAccess,
    working_dir: &std::path::Path,
) -> std::io::Result<tokio::process::Child> {
    chat_command(
        cli_locator::resolve("claude").await,
        model,
        resume,
        system_prompt,
        prompt,
        tool_access,
        working_dir,
    )
    .spawn()
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

    async fn check_availability(&self) -> Result<(), ProviderError> { check_logged_in().await }

    fn session_id(&self) -> Option<String> { ClaudeCodeCli::session_id(self) }

    /// Spawns `claude auth login` detached -- the CLI owns the whole
    /// OAuth ceremony (opening a browser, running its own local
    /// callback listener) from here on; Aemeath neither waits for it nor
    /// reads its output. The child is handed off to a background task
    /// purely so it gets reaped instead of left a zombie, not so its
    /// result can be inspected.
    async fn trigger_login(&self) -> Result<(), ProviderError> {
        let child = Command::new(cli_locator::resolve("claude").await)
            .args(["auth", "login"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| cli_process::map_spawn_error("claude", e))?;
        tokio::spawn(async move {
            let mut child = child;
            let _ = child.wait().await;
        });
        Ok(())
    }

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
        let resume = self.session_id();
        let prompt = build_prompt(&messages, &self.history, self.seen_turns, resume.is_some());
        let model = self.model.clone();
        let session_id_slot = self.session_id.clone();

        let mut child = spawn(
            model.as_deref(),
            resume.as_deref(),
            &system_prompt,
            &prompt,
            &self.tool_access,
            &self.working_dir,
        )
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
                        // The stale session took its memory with it, so
                        // the retry is seeded from the transcript.
                        let fresh_prompt =
                            build_prompt(&messages, &self.history, self.seen_turns, false);
                        child = spawn(
                            model.as_deref(),
                            None,
                            &system_prompt,
                            &fresh_prompt,
                            &self.tool_access,
                            &self.working_dir,
                        )
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
    use crate::settings::NativeToolAccess;

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

    // -- task 7.1: `build_args`'s `--allowedTools`/`--disallowedTools` --

    fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter().position(|a| a == flag).map(|i| args[i + 1].as_str())
    }

    #[test]
    fn build_args_matches_the_default_allow_deny_split() {
        let args = build_args(None, None, "persona", "hi", &ClaudeCodeToolAccess::default());
        assert_eq!(arg_value(&args, "--allowedTools"), Some("Read,Glob,Grep,WebSearch,WebFetch"));
        assert_eq!(
            arg_value(&args, "--disallowedTools"),
            Some("Write,Edit,Bash,NotebookEdit,Task,SlashCommand,TodoWrite")
        );
    }

    #[test]
    fn build_args_reflects_a_user_adjusted_setting() {
        let access = ClaudeCodeToolAccess {
            bash: NativeToolAccess::Auto,
            web_search: NativeToolAccess::Deny,
            ..ClaudeCodeToolAccess::default()
        };
        let args = build_args(None, None, "persona", "hi", &access);
        let allowed = arg_value(&args, "--allowedTools").unwrap();
        let disallowed = arg_value(&args, "--disallowedTools").unwrap();
        assert!(allowed.split(',').any(|t| t == "Bash"), "Bash must move into allowed: {allowed}");
        assert!(!allowed.contains("WebSearch"), "WebSearch must leave allowed: {allowed}");
        assert!(disallowed.split(',').any(|t| t == "WebSearch"));
        assert!(!disallowed.contains("Bash"));
    }

    // -- cli-session-continuity: `chat()` driven against a fake `claude` --

    /// A `claude` stand-in that rejects any `--resume` like a session the
    /// CLI has since dropped, answers everything else with a fresh
    /// session, and records the arguments of every chat invocation.
    #[cfg(unix)]
    fn fake_claude(dir: &std::path::Path, record: &std::path::Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("claude");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
if [ "$1" = "auth" ]; then echo '{{"loggedIn": true}}'; exit 0; fi
printf 'CALL %s\n' "$*" >> '{record}'
case "$*" in
  *--resume*)
    echo '{{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Could not resume session: not found.","session_id":"stale-id"}}'
    ;;
  *)
    echo '{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"fresh reply"}}}},"session_id":"new-id"}}'
    echo '{{"type":"result","subtype":"success","is_error":false,"result":"fresh reply","session_id":"new-id"}}'
    ;;
esac
"#,
                record = record.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_resume_is_retried_fresh_with_the_history_seeded() {
        use futures_util::StreamExt;

        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-fake-claude-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("calls.txt");
        let _guard = cli_locator::override_for_test("claude", fake_claude(&dir, &record));

        // A stored session id the fake CLI will reject as stale. It had seen
        // both history messages, so a *successful* resume would send only "now".
        let provider = ClaudeCodeCli::new(
            None,
            CliContext {
                resume_session_id: Some("stale-id".to_string()),
                history: real_history(),
                seen_turns: 2,
                working_dir: dir.clone(),
            },
            ClaudeCodeToolAccess::default(),
        );
        let mut stream =
            provider.chat(vec![Message::system("be brief"), Message::user("now")]).await.unwrap();
        let mut reply = String::new();
        while let Some(chunk) = stream.next().await {
            reply.push_str(&chunk.unwrap().delta);
        }

        assert_eq!(reply, "fresh reply", "the retry must be transparent to the consumer");
        assert_eq!(
            provider.session_id().as_deref(),
            Some("new-id"),
            "the fresh session replaces the stale id"
        );

        let calls = std::fs::read_to_string(&record).unwrap();
        let calls: Vec<&str> = calls.split("CALL ").filter(|c| !c.trim().is_empty()).collect();
        assert_eq!(calls.len(), 2, "one stale attempt, then one fresh retry: {calls:?}");
        assert!(calls[0].contains("--resume stale-id"), "first attempt resumes: {}", calls[0]);
        assert!(
            !calls[0].contains("Earlier in this conversation"),
            "a resumed attempt carries no history"
        );
        assert!(!calls[1].contains("--resume"), "the retry starts fresh: {}", calls[1]);
        assert!(
            calls[1].contains("Earlier in this conversation"),
            "the retry is seeded: {}",
            calls[1]
        );
        assert!(calls[1].contains("User: earlier question"));
        assert!(calls[1].contains("Assistant: earlier answer"));
    }

    // -- cli-session-continuity: which prompt each kind of request gets --

    /// `assemble_messages`' real shape: system prompt, a persona
    /// few-shot pair, then the new user message. The few-shot pair must
    /// never leak into a history preamble.
    fn assembled_messages(latest: &str) -> Vec<Message> {
        vec![
            Message::system("be brief"),
            Message::user("few-shot user"),
            Message::assistant("few-shot pet"),
            Message::user(latest),
        ]
    }

    fn real_history() -> Vec<Message> {
        vec![Message::user("earlier question"), Message::assistant("earlier answer")]
    }

    #[test]
    fn the_chat_command_runs_in_the_supplied_working_directory() {
        let dir = std::path::Path::new("/some/project");
        let command = chat_command(
            "claude",
            None,
            None,
            "be brief",
            "hello",
            &ClaudeCodeToolAccess::default(),
            dir,
        );
        assert_eq!(command.as_std().get_current_dir(), Some(dir));
    }

    #[test]
    fn a_resumed_chat_command_also_runs_in_the_supplied_working_directory() {
        let dir = std::path::Path::new("/some/project");
        let command = chat_command(
            "claude",
            None,
            Some("session-1"),
            "be brief",
            "hello",
            &ClaudeCodeToolAccess::default(),
            dir,
        );
        assert_eq!(command.as_std().get_current_dir(), Some(dir));
    }

    #[test]
    fn a_request_with_no_resume_is_seeded_with_the_history() {
        let prompt = build_prompt(&assembled_messages("now"), &real_history(), 0, false);
        assert!(prompt.contains("User: earlier question"));
        assert!(prompt.contains("Assistant: earlier answer"));
        assert!(prompt.ends_with("Current message:\nnow"));
        assert!(!prompt.contains("few-shot"), "persona few-shot examples are not history");
    }

    #[test]
    fn a_resumed_request_that_missed_nothing_sends_only_the_latest_message() {
        // The session has seen both history messages.
        assert_eq!(build_prompt(&assembled_messages("now"), &real_history(), 2, true), "now");
    }

    #[test]
    fn a_resumed_request_is_caught_up_on_turns_it_never_saw() {
        // The session has seen only the first history message; the assistant
        // answer after it happened without this session (e.g. another provider).
        let prompt = build_prompt(&assembled_messages("now"), &real_history(), 1, true);
        assert!(prompt.contains("Assistant: earlier answer"));
        assert!(!prompt.contains("User: earlier question"), "already held by the session");
        assert!(prompt.ends_with("Current message:\nnow"));
    }

    #[test]
    fn a_stale_resume_retry_is_seeded_like_any_fresh_request() {
        // `chat()`'s retry path builds its prompt with `resuming = false`,
        // and must include everything the stale session took with it --
        // including turns the session had already seen.
        let messages = assembled_messages("now");
        let resumed = build_prompt(&messages, &real_history(), 2, true);
        let retry = build_prompt(&messages, &real_history(), 2, false);
        assert_ne!(resumed, retry);
        assert!(retry.contains("User: earlier question"));
        assert!(retry.contains("Assistant: earlier answer"));
    }

    #[test]
    fn a_fresh_request_with_no_history_adds_nothing() {
        assert_eq!(build_prompt(&assembled_messages("now"), &[], 0, false), "now");
    }

    #[test]
    fn build_args_includes_model_resume_and_the_trailing_prompt() {
        let args = build_args(
            Some("claude-sonnet-5"),
            Some("session-123"),
            "persona",
            "hello there",
            &ClaudeCodeToolAccess::default(),
        );
        assert_eq!(arg_value(&args, "--model"), Some("claude-sonnet-5"));
        assert_eq!(arg_value(&args, "--resume"), Some("session-123"));
        assert_eq!(args.last().map(String::as_str), Some("hello there"));
    }

    #[test]
    fn the_prompt_follows_an_option_terminator_so_variadic_tool_flags_cannot_swallow_it() {
        // No model, no resume: the tool lists are the last options before
        // the prompt -- exactly the case the CLI's variadic parsing broke.
        let args =
            build_args(None, None, "persona", "hello there", &ClaudeCodeToolAccess::default());
        let n = args.len();
        assert_eq!(&args[n - 2..], ["--", "hello there"]);
        let disallowed = args.iter().position(|a| a == "--disallowedTools").unwrap();
        assert!(disallowed < n - 2, "tool flags come before the terminator");
    }

    #[test]
    fn a_prompt_starting_with_a_dash_is_passed_after_the_terminator() {
        let args =
            build_args(None, None, "persona", "- a list item", &ClaudeCodeToolAccess::default());
        let n = args.len();
        assert_eq!(&args[n - 2..], ["--", "- a list item"]);
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

        let first = ClaudeCodeCli::new(
            None,
            CliContext::fresh(std::env::temp_dir()),
            ClaudeCodeToolAccess::default(),
        );
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

        let second = ClaudeCodeCli::new(
            None,
            CliContext {
                resume_session_id: session_id.clone(),
                ..CliContext::fresh(std::env::temp_dir())
            },
            ClaudeCodeToolAccess::default(),
        );
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

    /// `cli-session-continuity`: a session with nothing to resume must
    /// still know the conversation, via the history preamble.
    #[tokio::test]
    #[ignore = "requires a logged-in `claude` CLI and makes a real subscription call"]
    async fn claude_code_cli_live_fresh_session_is_seeded_with_history() {
        use futures_util::StreamExt;

        let history = vec![
            Message::user("My favourite fruit is durian."),
            Message::assistant("Noted, durian it is!"),
        ];
        let provider = ClaudeCodeCli::new(
            None,
            CliContext { history, ..CliContext::fresh(std::env::temp_dir()) },
            ClaudeCodeToolAccess::default(),
        );
        let mut stream = provider
            .chat(vec![
                Message::system("Reply in one short sentence."),
                Message::user("Which fruit did I say was my favourite? Answer with one word."),
            ])
            .await
            .unwrap();
        let mut reply = String::new();
        while let Some(chunk) = stream.next().await {
            reply.push_str(&chunk.unwrap().delta);
        }
        assert!(
            reply.to_lowercase().contains("durian"),
            "a fresh session should have been seeded with the history, got: {reply}"
        );
    }

    /// `cli-session-continuity`: cancelling a generation drops the
    /// stream, which must terminate the real `claude` process. The
    /// prompt carries a unique marker so the process can be found by
    /// its command line.
    #[tokio::test]
    #[ignore = "requires a logged-in `claude` CLI and makes a real subscription call"]
    async fn claude_code_cli_live_dropping_the_stream_terminates_the_process() {
        use futures_util::StreamExt;

        fn running(marker: &str) -> bool {
            std::process::Command::new("pgrep")
                .args(["-f", marker])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }

        let marker = format!("killprobe-{}", std::process::id());
        let provider = ClaudeCodeCli::new(
            None,
            CliContext::fresh(std::env::temp_dir()),
            ClaudeCodeToolAccess::default(),
        );
        let mut stream = provider
            .chat(vec![
                Message::system("You are a storyteller."),
                Message::user(format!("{marker} Write a very long story, at least 3000 words.")),
            ])
            .await
            .unwrap();
        // Wait for real output, so the process is definitely mid-generation.
        stream.next().await.expect("a first chunk").unwrap();
        assert!(running(&marker), "the claude process should be running mid-generation");

        drop(stream);

        let mut gone = false;
        for _ in 0..75 {
            if !running(&marker) {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        assert!(gone, "the claude process was still running 3s after its stream was dropped");
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
        assert!(parse_auth_status(LIVE_LOGGED_IN_FIXTURE).unwrap());
    }

    #[test]
    fn parses_a_logged_out_status() {
        assert!(!parse_auth_status(r#"{"loggedIn": false}"#).unwrap());
    }

    #[tokio::test]
    async fn list_models_offers_the_known_aliases_not_an_empty_list() {
        let models = ClaudeCodeCli::new(
            None,
            CliContext::fresh(PathBuf::new()),
            ClaudeCodeToolAccess::default(),
        )
        .list_models()
        .await
        .unwrap();
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
