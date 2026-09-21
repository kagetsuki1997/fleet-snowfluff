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
    message::ProviderError,
    settings::{ProfileKey, ProviderProfile, TaskRouterMode},
};

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
}
