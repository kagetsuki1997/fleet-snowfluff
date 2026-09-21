//! Task Router domain types (`agent-core-and-task-router`'s "Task
//! routing mode" capability). Originally scoped into Stage 2 but
//! excluded when `subscription-first-chat` shipped -- see that
//! change's own Non-Goals -- so this lands here instead, alongside the
//! Stage 3 Agent Core work that actually needs it.
//!
//! This module is deliberately minimal relative to
//! `docs/fleet-snowfluff-feature-planning.md` §4's fuller sketch:
//! `ExecutionRoute` carries just enough to pick a profile (no
//! `capabilities`/`RoutingReason` fields -- there's nothing to audit
//! yet with only two possible destinations), and `TaskRequirements` is
//! defined but not consumed by `route()` -- it's reserved for a future
//! capability-escalation change, not this one (see design.md's
//! Non-Goals).
//!
//! `TaskRouter::route()` is called exactly once per message, to
//! produce the *initial* route. It is never called a second time to
//! handle an escalation or an infra-level failure of that route --
//! those are the execution layer's job (`chat_commands.rs`), the same
//! boundary the planning doc already draws for session-resume retries
//! (§4.6: the Router's job ends at producing the route; retries are
//! the Runtime's job).

use crate::{
    message::{Message, ProviderError, Role},
    settings::{ProfileKey, ProviderProfile, TaskRouterMode},
};

/// The exact token `mix` mode's local-model classification call must
/// reply with, and nothing else, to signal "this message is complex,
/// hand it to `default_profile` instead" -- see
/// `docs/fleet-snowfluff-feature-planning.md` §4.13. Shared as one
/// constant between the bundled rules text (which tells the model what
/// to say) and `detect_escalation` (which watches for it) so the two
/// can never drift apart.
pub const ESCALATE_MARKER: &str = "<<ESCALATE>>";

/// Seeded into `personas/task-router-rules.md` the first time it's
/// needed (mirroring `persona::BUNDLED_DEFAULT_PERSONA_YAML`'s own
/// seed-on-first-run pattern) and used as the in-memory fallback if
/// that file is ever missing or unreadable at load time -- `mix` mode
/// stays functional even before the user has looked at the file once.
/// Loaded straight from the real file (same `include_str!` pattern as
/// `persona::BUNDLED_DEFAULT_PERSONA_YAML`) so the file the user edits
/// and the bundled fallback can never drift apart. The file itself owns
/// the full reply-format contract (persona reply vs. [`ESCALATE_MARKER`])
/// -- there is no separate code-side preamble layered on top of it.
pub const BUNDLED_DEFAULT_TASK_ROUTER_RULES: &str =
    include_str!("../../../personas/task-router-rules.md");

/// Appends `rules` to `messages`' leading system message -- `messages`
/// is assumed to be `prompt::assemble_messages`'s own output, whose
/// first entry is always `Role::System` (a persona-only call, e.g.
/// `default_profile`'s, never goes through this at all). A no-op if
/// `messages` is empty or doesn't start with a system message, so a
/// caller can apply this unconditionally without checking the shape
/// itself.
pub fn with_task_router_rules(mut messages: Vec<Message>, rules: &str) -> Vec<Message> {
    if let Some(first) = messages.first_mut() {
        if first.role == Role::System {
            first.content = format!("{}\n\n{}", first.content, rules);
        }
    }
    messages
}

/// Where a streamed reply's accumulating prefix stands relative to
/// [`ESCALATE_MARKER`]. Leading whitespace is trimmed before comparing
/// -- a local model that prepends a stray space or newline before the
/// marker shouldn't be read as "definitely not escalating" just because
/// the raw bytes don't start with `<` yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationDecision {
    /// Still a strict prefix of the marker -- need more of the stream
    /// before this can be decided either way. If the stream ends while
    /// still `Undecided`, the caller must treat that as `Simple` (a
    /// reply that never fully says the marker cannot be escalating).
    Undecided,
    /// Diverged from the marker at some position -- conclusively not
    /// escalating, regardless of what the rest of the stream contains.
    Simple,
    /// The buffer is (at least) the full marker, matched exactly from
    /// the start.
    Escalate,
}

/// `str::starts_with` already short-circuits at the first mismatching
/// byte, so this is O(min(buffer.len(), marker.len())) either way --
/// no need to hand-roll a character-by-character comparison.
pub fn detect_escalation(buffer: &str) -> EscalationDecision {
    let trimmed = buffer.trim_start();
    if trimmed.len() >= ESCALATE_MARKER.len() {
        if trimmed.starts_with(ESCALATE_MARKER) {
            EscalationDecision::Escalate
        } else {
            EscalationDecision::Simple
        }
    } else if ESCALATE_MARKER.starts_with(trimmed) {
        EscalationDecision::Undecided
    } else {
        EscalationDecision::Simple
    }
}

/// What capabilities a task needs, judged independently of which
/// runtime ends up handling it. Defined now so a future capability-
/// escalation change has a real type to route on; not populated with
/// real values or consumed by anything in this change -- `mode: mix`'s
/// simple/complex judgment is made by the local model itself (see
/// `docs/fleet-snowfluff-feature-planning.md` §4.13), not by scoring
/// these flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskRequirements {
    pub needs_web: bool,
    pub needs_filesystem: bool,
    pub needs_shell: bool,
    pub needs_code_edit: bool,
    pub needs_browser: bool,
    pub needs_long_running: bool,
    pub needs_multiple_steps: bool,
    pub needs_iteration: bool,
}

/// One message to be routed, bundled with its (currently always
/// default) capability requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub message: String,
    pub requirements: TaskRequirements,
}

impl Task {
    /// `requirements` defaults to "needs nothing special" -- nothing
    /// populates it yet, see this module's own doc comment.
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), requirements: TaskRequirements::default() }
    }
}

/// Everything `TaskRouter::route()` needs to decide the initial route
/// for one message. `default_profile` is guaranteed present by the
/// caller (`chat_commands.rs` already refuses to route at all when no
/// provider is configured, before ever constructing this) -- `route()`
/// itself never has to handle "no profile exists anywhere."
/// `local_profile` is the enabled Ollama profile, if any; `None` when
/// Ollama isn't enabled, which `route()` treats as "not available,"
/// not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingContext {
    pub task: Task,
    pub mode: TaskRouterMode,
    pub default_profile: ProviderProfile,
    pub local_profile: Option<ProviderProfile>,
}

/// Whether, and how, an external-runtime session should be involved
/// for this execution. Not exercised beyond `None` by this change's own
/// `TaskRouter` implementation -- session resume for CLI-backed
/// subscription profiles already works via `ChatRuntimeState.cli_sessions`
/// (`subscription-first-chat`), untouched here; this type exists so a
/// future, more capable router has a real place to express that
/// decision instead of `chat_commands.rs` reaching into it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStrategy {
    None,
    Create,
    Resume(String),
}

/// The router's output: which profile handles this message, and
/// whether an external-runtime session should be created/resumed for
/// it. Deliberately just `profile_key` + `session_strategy` -- see this
/// module's own doc comment for why `capabilities`/`RoutingReason`
/// aren't here yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRoute {
    pub profile_key: ProfileKey,
    pub session_strategy: SessionStrategy,
}

/// Decides which provider profile handles a message. Implementations
/// never need to manage `Conversation`/session lifecycle themselves
/// (`docs/fleet-snowfluff-feature-planning.md` §4.12) -- `route()`'s
/// job ends at producing an `ExecutionRoute`.
#[async_trait::async_trait]
pub trait TaskRouter: Send + Sync {
    async fn route(&self, context: RoutingContext) -> Result<ExecutionRoute, ProviderError>;
}

/// The router this change actually ships: `single` mode always
/// resolves to `default_profile`; `mix` mode resolves to `local_profile`
/// when Ollama is enabled, falling back to `default_profile` directly
/// otherwise -- this is knowable upfront from settings, not a runtime
/// failure, so it belongs in the initial route rather than the
/// execution-layer fallback path that handles escalation/infra failure
/// once an attempt is already underway (see this module's own doc
/// comment).
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultTaskRouter;

#[async_trait::async_trait]
impl TaskRouter for DefaultTaskRouter {
    async fn route(&self, context: RoutingContext) -> Result<ExecutionRoute, ProviderError> {
        let profile = match context.mode {
            TaskRouterMode::Single => context.default_profile,
            TaskRouterMode::Mix => context.local_profile.unwrap_or(context.default_profile),
        };
        Ok(ExecutionRoute { profile_key: profile.key(), session_strategy: SessionStrategy::None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{message::ProviderKind, settings::AuthMethod};

    fn profile(provider: ProviderKind, auth_method: AuthMethod) -> ProviderProfile {
        ProviderProfile { provider, auth_method, model: None, base_url: None }
    }

    // -- 2.3: rules get appended to the leading system message only --

    #[test]
    fn rules_are_appended_to_the_leading_system_message() {
        let messages = vec![Message::system("be brief"), Message::user("hi")];
        let with_rules = with_task_router_rules(messages, "some rules");
        assert!(with_rules[0].content.contains("be brief"));
        assert!(with_rules[0].content.contains("some rules"));
        assert_eq!(with_rules[1].content, "hi", "non-system messages are untouched");
    }

    #[test]
    fn appending_rules_to_an_empty_message_list_is_a_no_op() {
        assert_eq!(with_task_router_rules(vec![], "some rules"), Vec::<Message>::new());
    }

    #[test]
    fn bundled_default_rules_contain_the_escalate_marker() {
        // Loaded via `include_str!`, same pattern as
        // `persona::BUNDLED_DEFAULT_PERSONA_YAML` -- if the real file
        // ever drifted (e.g. a copy-paste edit) to no longer mention the
        // marker, the local model would have no way to know what token
        // to reply with. Catch that here, not at runtime.
        assert!(BUNDLED_DEFAULT_TASK_ROUTER_RULES.contains(ESCALATE_MARKER));
    }

    // -- 2.4: escalation-marker detection --

    #[test]
    fn immediate_mismatch_is_conclusively_simple() {
        assert_eq!(detect_escalation("雪絨在這裡陪你"), EscalationDecision::Simple);
    }

    #[test]
    fn a_full_match_escalates() {
        assert_eq!(detect_escalation(ESCALATE_MARKER), EscalationDecision::Escalate);
    }

    #[test]
    fn a_full_match_with_leading_whitespace_still_escalates() {
        assert_eq!(
            detect_escalation(&format!("  {ESCALATE_MARKER}")),
            EscalationDecision::Escalate
        );
    }

    #[test]
    fn a_strict_prefix_of_the_marker_is_undecided() {
        assert_eq!(detect_escalation("<<ESC"), EscalationDecision::Undecided);
        assert_eq!(detect_escalation(""), EscalationDecision::Undecided);
    }

    #[test]
    fn a_short_reply_that_never_diverges_must_be_resolved_by_the_caller_as_simple_at_stream_end() {
        // "<<" is a genuine strict prefix of the marker (matches so
        // far, just hasn't finished) -- if the model's whole reply
        // happened to be exactly this (e.g. it got cut off, or that's
        // really all it said), `detect_escalation` alone can only
        // report `Undecided`, since it has no way to know the stream
        // won't produce more input. The "resolve Undecided to Simple at
        // end-of-stream" rule lives in the caller (`chat_commands.rs`),
        // exercised there, not here.
        assert_eq!(detect_escalation("<<"), EscalationDecision::Undecided);
    }

    #[test]
    fn a_mismatch_after_a_partial_prefix_is_simple() {
        assert_eq!(detect_escalation("<<ESCALATE-nope"), EscalationDecision::Simple);
    }

    // -- 1.1: types round-trip through construction/equality --

    #[test]
    fn task_defaults_to_no_capability_requirements() {
        let task = Task::new("hello");
        assert_eq!(task.message, "hello");
        assert_eq!(task.requirements, TaskRequirements::default());
        assert!(!task.requirements.needs_web);
        assert!(!task.requirements.needs_shell);
    }

    #[test]
    fn routing_context_round_trips_through_equality() {
        let default_profile = profile(ProviderKind::Anthropic, AuthMethod::Subscription);
        let a = RoutingContext {
            task: Task::new("hi"),
            mode: TaskRouterMode::Single,
            default_profile: default_profile.clone(),
            local_profile: None,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn execution_route_round_trips_through_equality() {
        let key = ProfileKey { provider: ProviderKind::Ollama, auth_method: AuthMethod::Local };
        let a = ExecutionRoute { profile_key: key, session_strategy: SessionStrategy::None };
        let b = ExecutionRoute { profile_key: key, session_strategy: SessionStrategy::None };
        assert_eq!(a, b);
        let resumed = ExecutionRoute {
            profile_key: key,
            session_strategy: SessionStrategy::Resume("s1".into()),
        };
        assert_ne!(a, resumed);
    }

    // -- 1.2: `TaskRouter` is object-safe --

    struct AlwaysDefaultRouter;

    #[async_trait::async_trait]
    impl TaskRouter for AlwaysDefaultRouter {
        async fn route(&self, context: RoutingContext) -> Result<ExecutionRoute, ProviderError> {
            Ok(ExecutionRoute {
                profile_key: context.default_profile.key(),
                session_strategy: SessionStrategy::None,
            })
        }
    }

    #[tokio::test]
    async fn task_router_trait_is_usable_behind_a_box_dyn() {
        let router: Box<dyn TaskRouter> = Box::new(AlwaysDefaultRouter);
        let default_profile = profile(ProviderKind::OpenAi, AuthMethod::ApiKey);
        let route = router
            .route(RoutingContext {
                task: Task::new("hi"),
                mode: TaskRouterMode::Single,
                default_profile: default_profile.clone(),
                local_profile: None,
            })
            .await
            .unwrap();
        assert_eq!(route.profile_key, default_profile.key());
    }

    // -- 1.3: `DefaultTaskRouter`'s three resolutions --

    #[tokio::test]
    async fn single_mode_always_resolves_to_default_profile() {
        let default_profile = profile(ProviderKind::Anthropic, AuthMethod::Subscription);
        let local_profile = profile(ProviderKind::Ollama, AuthMethod::Local);
        let route = DefaultTaskRouter
            .route(RoutingContext {
                task: Task::new("hi"),
                mode: TaskRouterMode::Single,
                default_profile: default_profile.clone(),
                local_profile: Some(local_profile),
            })
            .await
            .unwrap();
        assert_eq!(route.profile_key, default_profile.key());
    }

    #[tokio::test]
    async fn mix_mode_resolves_to_local_profile_when_enabled() {
        let default_profile = profile(ProviderKind::Anthropic, AuthMethod::Subscription);
        let local_profile = profile(ProviderKind::Ollama, AuthMethod::Local);
        let route = DefaultTaskRouter
            .route(RoutingContext {
                task: Task::new("hi"),
                mode: TaskRouterMode::Mix,
                default_profile,
                local_profile: Some(local_profile.clone()),
            })
            .await
            .unwrap();
        assert_eq!(route.profile_key, local_profile.key());
    }

    #[tokio::test]
    async fn mix_mode_falls_back_to_default_profile_when_ollama_is_not_enabled() {
        let default_profile = profile(ProviderKind::Anthropic, AuthMethod::Subscription);
        let route = DefaultTaskRouter
            .route(RoutingContext {
                task: Task::new("hi"),
                mode: TaskRouterMode::Mix,
                default_profile: default_profile.clone(),
                local_profile: None,
            })
            .await
            .unwrap();
        assert_eq!(route.profile_key, default_profile.key());
    }

    // -- manual sanity check against a real local model --

    /// Manual integration test requiring a locally running Ollama with
    /// `MANUAL_TEST_MODEL` pulled (defaults to `llama3.2` if unset).
    /// Sends one prompt that `personas/task-router-rules.md` calls out
    /// as `LOCAL` and one it calls out as `ESCALATE`, and checks the
    /// real model's own reply against `detect_escalation` -- this is
    /// how to sanity-check an edit to the rules file against real model
    /// behavior, not something CI runs. Never run in CI: `cargo test -p
    /// fleet-snowfluff-ai --test-threads=1 -- --ignored
    /// task_router_rules_live`.
    #[tokio::test]
    #[ignore = "requires a locally running Ollama with a model pulled"]
    async fn task_router_rules_live_classification_round_trip() {
        use futures_util::StreamExt;

        use crate::{provider::AiProvider, providers::Ollama};

        let model = std::env::var("MANUAL_TEST_MODEL").unwrap_or_else(|_| "llama3.2".into());
        let provider = Ollama::new(crate::providers::ollama::DEFAULT_BASE_URL, model);

        async fn classify(provider: &Ollama, user_message: &str) -> String {
            let messages = with_task_router_rules(
                vec![Message::system("You are a helpful assistant."), Message::user(user_message)],
                BUNDLED_DEFAULT_TASK_ROUTER_RULES,
            );
            let mut stream = provider.chat(messages).await.unwrap();
            let mut reply = String::new();
            while let Some(chunk) = stream.next().await {
                reply.push_str(&chunk.unwrap().delta);
                if detect_escalation(&reply) != EscalationDecision::Undecided {
                    break;
                }
            }
            reply
        }

        let local_reply = classify(&provider, "What is Rust ownership?").await;
        println!("LOCAL-case reply: {local_reply:?}");
        assert_ne!(
            detect_escalation(&local_reply),
            EscalationDecision::Escalate,
            "expected a simple knowledge question to stay local, got: {local_reply:?}"
        );

        let escalate_reply =
            classify(&provider, "Run cargo test in my project and fix any failures.").await;
        println!("ESCALATE-case reply: {escalate_reply:?}");
        assert_eq!(
            detect_escalation(&escalate_reply),
            EscalationDecision::Escalate,
            "expected a shell-execution request to escalate, got: {escalate_reply:?}"
        );
    }
}
