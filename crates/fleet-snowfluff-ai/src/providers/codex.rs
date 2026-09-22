//! `Codex`: OpenAI's `Subscription` auth method for the OpenAI provider
//! brand, implemented by spawning `codex exec` (Codex CLI's
//! non-interactive mode) against an already-authenticated ChatGPT/Codex
//! login, following the same "shell out to the official CLI, let it
//! own the credential" principle as `ClaudeCodeCli`
//! (`subscription-first-chat`'s "No persisted credential for
//! subscription auth").
//!
//! **Experimental** (`subscription-first-chat`'s "Experimental provider
//! marking" -- see `ProviderProfile::is_experimental` in `settings.rs`):
//! unlike `ClaudeCodeCli`, nothing here was verified against a real
//! ChatGPT/Codex subscription -- no such subscription was available
//! during design. Everything below is built from Codex's own published
//! documentation and OpenClaw's own implementation (the reference this
//! whole feature was modeled on), not live testing. Two confirmed,
//! real limitations shape this implementation and are not workarounds
//! to be "fixed" later so much as inherent differences from the
//! Anthropic path:
//!
//! - **No incremental streaming.** `codex exec --json`'s assistant text arrives
//!   as exactly one complete `item.completed` event, not delta-by-delta
//!   (confirmed via documentation). To still honor `ai-provider`'s "Streaming
//!   responses" requirement, the complete text is chunked client-side after the
//!   fact, reusing `Mock`'s own `chunk_text` -- this is synthesized pacing, not
//!   real incremental generation, and is documented as such rather than
//!   presented as equivalent to the other providers' genuine streaming.
//! - **No system-prompt override flag.** `codex exec` has no
//!   `--system-prompt`/`--instructions` equivalent. Following OpenClaw's own
//!   approach: the persona is written to a temporary file and passed via `-c
//!   model_instructions_file=<path>`, a real documented Codex config key
//!   overridable from the command line. Whether this reliably takes precedence
//!   over a project's own `AGENTS.md` in every case is not confirmed
//!   (documentation describes an instruction *search order* --
//!   `AGENTS.override.md` > `AGENTS.md` > a configured fallback file -- and
//!   it's not fully clear from documentation alone whether
//!   `model_instructions_file` always wins or only applies when no `AGENTS.md`
//!   exists in the invocation's working directory).
//!
//! Session continuity (`subscription-first-chat`'s "Session continuity
//! for CLI-backed subscription providers") uses `codex exec resume
//! <SESSION_ID>`, confirmed to exist in Codex's own documentation --
//! the same warm-session model as `ClaudeCodeCli`, not the "maybe
//! Codex doesn't support this" asymmetry design.md originally left as
//! an open question.
//!
//! **No per-tool allow-list (`agent-core-and-task-router` Group 7).**
//! Unlike `ClaudeCodeCli`, which builds a per-tool `--allowedTools`/
//! `--disallowedTools` split from `ClaudeCodeToolAccess`, this provider
//! does not attempt equivalent by-case granularity -- tool access stays
//! at whatever coarse `--sandbox read-only` already grants below. This
//! is deliberate, not an oversight: Codex's own by-case tool controls
//! are unverified/experimental (no installed Codex CLI to check
//! against during design), and `--sandbox read-only` may incidentally
//! also block Codex's own first-party web search alongside file
//! writes -- an open, documented risk (see design.md's Risks section)
//! rather than a silently-assumed match to Claude Code's UI.
//!
//! Every spawn resolves `codex` through `cli_locator::resolve` rather
//! than a bare `Command::new("codex")` (`agent-core-and-task-router`
//! Group 10), for the same launched-outside-a-terminal reason as
//! `ClaudeCodeCli` -- see that module's own doc comment for the full
//! per-platform search-order reasoning shared by both providers.

use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

use crate::{
    message::{Message, ModelInfo, ProviderError, ProviderKind, Role, StreamChunk},
    provider::{AiProvider, ChatStream},
    providers::{cli_locator, cli_process, mock},
};

/// Small enough to feel responsive, large enough to visibly show
/// progressive text rather than flashing the whole reply in at once --
/// same reasoning as `Mock`'s own constants, applied here to a
/// synthesized rather than genuine stream.
const SYNTHETIC_CHUNK_DELAY: std::time::Duration = std::time::Duration::from_millis(20);
const SYNTHETIC_CHUNK_SIZE_CHARS: usize = 6;

pub struct Codex {
    pub model: Option<String>,
    session_id: Arc<Mutex<Option<String>>>,
}

impl Codex {
    /// `resume_session_id` is the id returned by a prior instance's
    /// `session_id()`, if any -- supplied by the app crate, never
    /// stored by this crate itself.
    pub fn new(model: Option<String>, resume_session_id: Option<String>) -> Self {
        Self { model, session_id: Arc::new(Mutex::new(resume_session_id)) }
    }

    /// The Codex thread id captured from this instance's most recent
    /// `chat()` call, if any -- read by the app crate after a
    /// generation completes and threaded into the *next* `Codex` for
    /// the same chat session, so it can `resume`.
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

/// A path in the OS temp directory unique enough not to collide with a
/// concurrent call, without pulling in a `uuid` dependency for one
/// short-lived file.
fn temp_instructions_path() -> std::path::PathBuf {
    let unique = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default()
    );
    std::env::temp_dir().join(format!("fleet-snowfluff-codex-instructions-{unique}.md"))
}

/// Runs `codex login status` and returns `Ok(())` only if logged in.
/// Called before every `chat()` rather than cached, per
/// `subscription-first-chat`'s "No persisted credential for
/// subscription auth". Unlike `claude auth status --json`, this
/// subcommand's documented contract is exit-code-based (0 = logged in,
/// 1 = not) with plain text on stdout/stderr, not structured JSON.
async fn check_logged_in() -> Result<(), ProviderError> {
    let output = Command::new(cli_locator::resolve("codex").await)
        .args(["login", "status"])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| cli_process::map_spawn_error("codex", e))?;

    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = if !stdout.trim().is_empty() { stdout.trim() } else { stderr.trim() };
    Err(ProviderError::SubscriptionExpired(if message.is_empty() {
        "not logged in to Codex -- run `codex login`".to_string()
    } else {
        message.to_string()
    }))
}

/// What one parsed line of `codex exec --json` output means for the
/// chat stream, separate from whether it also carried a thread/session
/// id (only `thread.started` does).
#[derive(Debug, PartialEq)]
enum LineOutcome {
    /// The complete assistant reply text -- not a delta (see module
    /// doc): `codex exec` has no incremental text events at all.
    FinalText(String),
    TerminalError(ProviderError),
    Ignored,
}

struct ParsedLine {
    session_id: Option<String>,
    outcome: LineOutcome,
}

/// Best-effort classification of an error message into a
/// `ProviderError` variant. **Assumption, not verified against a real
/// failure** -- no live Codex subscription was available during
/// design, so no real quota-exhausted/rate-limited/stale-resume/
/// not-logged-in response was observed for this provider (unlike
/// `ClaudeCodeCli`, where at least the success path was verified live).
fn classify_error(message: &str) -> ProviderError {
    let lower = message.to_lowercase();
    if lower.contains("resum") || lower.contains("session") || lower.contains("thread") {
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

fn looks_resume_related(err: &ProviderError) -> bool {
    let ProviderError::InvalidResponse(msg) = err else { return false };
    let lower = msg.to_lowercase();
    lower.contains("resum") || lower.contains("session") || lower.contains("thread")
}

/// Pure parser for one already-read line of stdout -- mirrors
/// `claude_code_cli.rs`'s `parse_stream_json_line`. Event shapes here
/// are sourced from Codex's own published documentation (see module
/// doc), not from a live capture.
fn parse_codex_json_line(line: &str) -> ParsedLine {
    let line = line.trim();
    if line.is_empty() {
        return ParsedLine { session_id: None, outcome: LineOutcome::Ignored };
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return ParsedLine { session_id: None, outcome: LineOutcome::Ignored };
    };

    let session_id = value.get("thread_id").and_then(Value::as_str).map(str::to_string);

    let outcome = match value.get("type").and_then(Value::as_str) {
        Some("item.completed") => match value.get("item") {
            Some(item) => match item.get("type").and_then(Value::as_str) {
                Some("agent_message") => item
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|text| LineOutcome::FinalText(text.to_string()))
                    .unwrap_or(LineOutcome::Ignored),
                Some("error") => {
                    let message = item
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("codex exec reported an error");
                    LineOutcome::TerminalError(classify_error(message))
                }
                _ => LineOutcome::Ignored,
            },
            None => LineOutcome::Ignored,
        },
        Some("turn.failed") => {
            let message = value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("codex exec turn failed");
            LineOutcome::TerminalError(classify_error(message))
        }
        Some("error") => {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("codex exec reported an error");
            LineOutcome::TerminalError(classify_error(message))
        }
        _ => LineOutcome::Ignored,
    };

    ParsedLine { session_id, outcome }
}

async fn spawn(
    model: Option<&str>,
    resume: Option<&str>,
    instructions_path: &std::path::Path,
    prompt: &str,
) -> std::io::Result<tokio::process::Child> {
    let mut command = Command::new(cli_locator::resolve("codex").await);
    command.arg("exec");
    if let Some(id) = resume {
        command.args(["resume", id]);
    }
    command
        .arg("--json")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--skip-git-repo-check")
        .args(["-c", &format!("model_instructions_file={}", instructions_path.display())])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(model) = model {
        command.args(["--model", model]);
    }
    command.arg(prompt);
    command.spawn()
}

async fn read_line(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> Result<Option<String>, ProviderError> {
    lines.next_line().await.map_err(|e| ProviderError::Network(e.to_string()))
}

#[async_trait]
impl AiProvider for Codex {
    fn kind(&self) -> ProviderKind { ProviderKind::OpenAi }

    async fn check_availability(&self) -> Result<(), ProviderError> { check_logged_in().await }

    fn session_id(&self) -> Option<String> { Codex::session_id(self) }

    /// Spawns `codex login` detached -- mirrors
    /// `ClaudeCodeCli::trigger_login`: Codex's CLI owns the whole
    /// browser-based login ceremony from here, Aemeath only starts it.
    /// Untested against a real login flow (same caveat as the rest of
    /// this experimental provider, see module doc) -- if headless
    /// `codex login` turns out not to complete this way, the spawn
    /// itself still succeeds (it's a fire-and-forget launch, not a
    /// wait-for-success check), and the settings UI's existing
    /// not-logged-in status text remains the fallback instruction.
    async fn trigger_login(&self) -> Result<(), ProviderError> {
        let child = Command::new(cli_locator::resolve("codex").await)
            .arg("login")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| cli_process::map_spawn_error("codex", e))?;
        tokio::spawn(async move {
            let mut child = child;
            let _ = child.wait().await;
        });
        Ok(())
    }

    /// See module doc for the two confirmed limitations this works
    /// around: synthesized chunking (no real incremental text from
    /// Codex) and `model_instructions_file` (no system-prompt flag).
    /// Resume fallback mirrors `ClaudeCodeCli::chat()`'s: a stale
    /// `resume` on the first meaningful line, with nothing yielded yet,
    /// retries fresh instead of failing outright.
    async fn chat(&self, messages: Vec<Message>) -> Result<ChatStream, ProviderError> {
        check_logged_in().await?;

        let system_prompt = system_prompt(&messages);
        let prompt = latest_user_message(&messages);
        let resume = self.session_id();
        let model = self.model.clone();
        let session_id_slot = self.session_id.clone();

        let instructions_path = temp_instructions_path();
        tokio::fs::write(&instructions_path, &system_prompt).await.map_err(|e| {
            ProviderError::InvalidResponse(format!(
                "could not write persona instructions file: {e}"
            ))
        })?;

        let mut child = spawn(model.as_deref(), resume.as_deref(), &instructions_path, &prompt)
            .await
            .map_err(|e| cli_process::map_spawn_error("codex", e))?;
        let mut lines =
            BufReader::new(child.stdout.take().expect("stdout was piped by `spawn`")).lines();

        let mut pending_final_text = None;
        if resume.is_some() {
            if let Some(line) = read_line(&mut lines).await? {
                let parsed = parse_codex_json_line(&line);
                if let LineOutcome::TerminalError(err) = &parsed.outcome {
                    if looks_resume_related(err) {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        *session_id_slot.lock().unwrap() = None;
                        child = spawn(model.as_deref(), None, &instructions_path, &prompt)
                            .await
                            .map_err(|e| cli_process::map_spawn_error("codex", e))?;
                        lines = BufReader::new(
                            child.stdout.take().expect("stdout was piped by `spawn`"),
                        )
                        .lines();
                    }
                } else {
                    if let Some(id) = parsed.session_id {
                        *session_id_slot.lock().unwrap() = Some(id);
                    }
                    if let LineOutcome::FinalText(text) = parsed.outcome {
                        pending_final_text = Some(text);
                    }
                }
            }
        }

        let stream = async_stream::stream! {
            let _child = child; // kept alive for the duration of the stream
            let _instructions_path = instructions_path; // deleted below, after streaming
            let mut final_text = pending_final_text;
            loop {
                if final_text.is_some() {
                    break;
                }
                let line = match read_line(&mut lines).await {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tokio::fs::remove_file(&_instructions_path).await;
                        yield Err(e);
                        return;
                    }
                };
                let parsed = parse_codex_json_line(&line);
                if let Some(id) = parsed.session_id {
                    *session_id_slot.lock().unwrap() = Some(id);
                }
                match parsed.outcome {
                    LineOutcome::FinalText(text) => final_text = Some(text),
                    LineOutcome::TerminalError(err) => {
                        let _ = tokio::fs::remove_file(&_instructions_path).await;
                        yield Err(err);
                        return;
                    }
                    LineOutcome::Ignored => {}
                }
            }

            let _ = tokio::fs::remove_file(&_instructions_path).await;
            if let Some(text) = final_text {
                for chunk in mock::chunk_text(&text, SYNTHETIC_CHUNK_SIZE_CHARS) {
                    tokio::time::sleep(SYNTHETIC_CHUNK_DELAY).await;
                    yield Ok(StreamChunk { delta: chunk });
                }
            }
        };
        Ok(Box::pin(stream))
    }

    /// No live model-listing surface (same reasoning as
    /// `ClaudeCodeCli::list_models`) -- an empty list is not an error
    /// here. Unlike Claude Code's `--model` flag, which documents a
    /// small, stable set of named aliases (`sonnet`/`opus`/`fable`/
    /// `haiku`), Codex CLI's own `model` config key takes an
    /// open-ended, frequently-revised version string (confirmed via
    /// its official docs at developers.openai.com/codex during
    /// `/opsx:verify`: the only example given is a bare `model =
    /// "gpt-5.6"`, with no enumerated list, and third-party sources
    /// disagree with each other on current values) -- there is no
    /// confirmed, stable alias set to offer here the way there is for
    /// Claude. `ai-provider`'s "Live model listing" requirement was
    /// updated to make this an explicit second case (its own "no
    /// confirmed alias set" scenario) rather than leaving the empty
    /// list looking like an unreconciled gap against text that assumed
    /// every CLI has a small alias list the way Claude Code does.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> { Ok(vec![]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_system_prompt_and_latest_user_message() {
        let messages =
            vec![Message::system("be brief"), Message::user("first"), Message::user("second")];
        assert_eq!(system_prompt(&messages), "be brief");
        assert_eq!(latest_user_message(&messages), "second");
    }

    // -- Fixtures matching Codex's own published event-shape
    // documentation (`item.completed`/`agent_message`, `thread.started`,
    // `turn.failed`) -- not observed live, unlike the Anthropic fixtures
    // in `claude_code_cli.rs`.

    const DOCUMENTED_THREAD_STARTED: &str =
        r#"{"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}"#;
    const DOCUMENTED_AGENT_MESSAGE: &str = r#"{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Done. I updated the docs and added examples."}}"#;

    #[test]
    fn captures_thread_id_as_the_session_id_but_yields_no_chunk() {
        let parsed = parse_codex_json_line(DOCUMENTED_THREAD_STARTED);
        assert_eq!(parsed.session_id.as_deref(), Some("0199a213-81c0-7800-8aa1-bbab2a035a53"));
        assert_eq!(parsed.outcome, LineOutcome::Ignored);
    }

    #[test]
    fn agent_message_item_completed_yields_the_full_final_text() {
        let parsed = parse_codex_json_line(DOCUMENTED_AGENT_MESSAGE);
        assert_eq!(
            parsed.outcome,
            LineOutcome::FinalText("Done. I updated the docs and added examples.".to_string())
        );
    }

    #[test]
    fn turn_failed_is_a_terminal_error() {
        let line = r#"{"type":"turn.failed","error":{"message":"model response stream ended unexpectedly"}}"#;
        let parsed = parse_codex_json_line(line);
        assert!(matches!(
            parsed.outcome,
            LineOutcome::TerminalError(ProviderError::InvalidResponse(_))
        ));
    }

    #[test]
    fn top_level_error_event_is_a_terminal_error() {
        let line = r#"{"type":"error","message":"stream error: broken pipe"}"#;
        let parsed = parse_codex_json_line(line);
        assert!(matches!(parsed.outcome, LineOutcome::TerminalError(_)));
    }

    #[test]
    fn ignores_non_terminal_item_and_turn_events() {
        for line in [
            r#"{"type":"turn.started"}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}"#,
            r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"ls","status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"thinking"}}"#,
        ] {
            assert_eq!(parse_codex_json_line(line).outcome, LineOutcome::Ignored, "line: {line}");
        }
    }

    #[test]
    fn blank_and_malformed_lines_are_ignored() {
        assert_eq!(parse_codex_json_line("").outcome, LineOutcome::Ignored);
        assert_eq!(parse_codex_json_line("not json").outcome, LineOutcome::Ignored);
    }

    #[test]
    fn quota_and_rate_limit_errors_are_classified_distinctly() {
        let quota = classify_error("You've hit your usage limit for this period.");
        let rate = classify_error("Rate limit exceeded.");
        assert!(matches!(quota, ProviderError::QuotaExhausted(_)));
        assert!(matches!(rate, ProviderError::RateLimited(_)));
    }

    #[test]
    fn not_logged_in_error_maps_to_subscription_expired() {
        let err = classify_error("Not logged in. Run `codex login`.");
        assert!(matches!(err, ProviderError::SubscriptionExpired(_)));
    }

    #[test]
    fn resume_failure_is_detected_for_the_retry_decision() {
        assert!(looks_resume_related(&classify_error("Could not resume thread: not found.")));
        assert!(!looks_resume_related(&classify_error("Internal server error.")));
    }

    #[tokio::test]
    async fn list_models_is_empty_by_design_since_no_confirmed_alias_set_exists() {
        // Deliberately the opposite assertion from
        // `claude_code_cli.rs`'s
        // `list_models_offers_the_known_aliases_not_an_empty_list` -- see this
        // function's own doc comment for why the two providers can't share the
        // same answer here.
        let models = Codex::new(None, None).list_models().await.unwrap();
        assert!(models.is_empty());
    }

    #[test]
    fn temp_instructions_paths_are_unique_across_calls() {
        let a = temp_instructions_path();
        let b = temp_instructions_path();
        assert_ne!(a, b, "concurrent calls must not collide on the same instructions file");
    }
}
