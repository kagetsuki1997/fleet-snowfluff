//! The memory tools: `memory_search` and `memory_get` read, `memory_remember`
//! and `memory_forget` write. None takes a scope or owner argument; whose
//! memories they touch, and whether sensitive entries are visible, is fixed
//! when the tool is built from the profile handling the request.
//!
//! The write tools (and `compact_context`) are not offered to delegated
//! sub-tasks, so a sub-task can look things up but never change what is
//! remembered. Saves go through `memory::policy`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{
    agent_tool::{PermissionTier, Tool, ToolContext, ToolError, ToolResult},
    memory::{
        policy::{self, SaveOutcome},
        Audience, Kind, MemoryRecord, MemoryService, NewMemory,
    },
    tool_provider::ToolDefinition,
};

pub const MEMORY_SEARCH_TOOL_NAME: &str = "memory_search";
pub const MEMORY_GET_TOOL_NAME: &str = "memory_get";
pub const MEMORY_REMEMBER_TOOL_NAME: &str = "memory_remember";
pub const MEMORY_FORGET_TOOL_NAME: &str = "memory_forget";

fn line(record: &MemoryRecord) -> String { format!("[{}] {}", record.id, record.text) }

fn string_arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())
}

pub struct MemorySearchTool {
    service: Arc<dyn MemoryService>,
    audience: Audience,
}

impl MemorySearchTool {
    pub fn new(service: Arc<dyn MemoryService>, audience: Audience) -> Self {
        Self { service, audience }
    }
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: MEMORY_SEARCH_TOOL_NAME.to_string(),
            description: "Looks up what you remember about the user, by keywords. Use it when the \
                          user refers to something from earlier conversations that is not already \
                          in your notes."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Keywords to look for." },
                    "limit": { "type": "integer", "description": "How many to return (default 5)." }
                },
                "required": ["query"]
            }),
        }
    }

    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Auto
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let query = string_arg(&args, "query")
            .ok_or_else(|| ToolError("missing \"query\" argument".to_string()))?;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5).clamp(1, 10) as usize;
        Ok(match self.service.search(query, limit, self.audience).await {
            Ok(hits) if hits.is_empty() => ToolResult::ok("Nothing remembered matches that."),
            Ok(hits) => ToolResult::ok(
                hits.iter().map(|hit| line(&hit.record)).collect::<Vec<_>>().join("\n"),
            ),
            Err(_) => ToolResult::error("Memory is not available right now."),
        })
    }
}

pub struct MemoryGetTool {
    service: Arc<dyn MemoryService>,
    audience: Audience,
}

impl MemoryGetTool {
    pub fn new(service: Arc<dyn MemoryService>, audience: Audience) -> Self {
        Self { service, audience }
    }
}

#[async_trait]
impl Tool for MemoryGetTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: MEMORY_GET_TOOL_NAME.to_string(),
            description: "Reads one remembered fact in full, by the id shown in search results."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
        }
    }

    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Auto
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let id = string_arg(&args, "id")
            .ok_or_else(|| ToolError("missing \"id\" argument".to_string()))?;
        Ok(match self.service.get(id, self.audience).await {
            Ok(Some(record)) => ToolResult::ok(format!(
                "{}\n(kind: {}, {} importance {}{})",
                line(&record),
                record.kind.as_str(),
                match record.origin {
                    crate::memory::Origin::Told => "told by the user,",
                    crate::memory::Origin::Auto => "noticed automatically,",
                },
                record.importance,
                if record.tags.is_empty() {
                    String::new()
                } else {
                    format!(", tags: {}", record.tags.join(", "))
                },
            )),
            Ok(None) => ToolResult::error("No remembered fact has that id."),
            Err(_) => ToolResult::error("Memory is not available right now."),
        })
    }
}

pub struct MemoryRememberTool {
    service: Arc<dyn MemoryService>,
    persona_name: Option<String>,
}

impl MemoryRememberTool {
    pub fn new(service: Arc<dyn MemoryService>, persona_name: Option<String>) -> Self {
        Self { service, persona_name }
    }
}

#[async_trait]
impl Tool for MemoryRememberTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: MEMORY_REMEMBER_TOOL_NAME.to_string(),
            description: "Saves one fact about the user for later conversations. Use it when the \
                          user asks you to remember something or corrects something you \
                          remembered. Write one short, neutral fact. To replace an earlier fact, \
                          pass its id as `supersedes`."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "The fact, in the user's language." },
                    "kind": {
                        "type": "string",
                        "enum": ["preference", "life_context", "correction", "shared_moment", "other"]
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Topic keywords, including English ones."
                    },
                    "importance": { "type": "integer", "description": "1 (minor) to 3 (important)." },
                    "supersedes": { "type": "string", "description": "Id of the fact this replaces." },
                    "sensitive": {
                        "type": "boolean",
                        "description": "True to keep it off cloud models."
                    }
                },
                "required": ["text"]
            }),
        }
    }

    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Confirm
    }

    fn available_to_children(&self) -> bool { false }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let text = string_arg(&args, "text")
            .ok_or_else(|| ToolError("missing \"text\" argument".to_string()))?;
        let mut new = NewMemory::told(text);
        new.persona_name = self.persona_name.clone();
        if let Some(kind) = string_arg(&args, "kind") {
            new.kind = Kind::parse(kind);
        }
        if let Some(tags) = args.get("tags").and_then(Value::as_array) {
            new.tags = tags
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(importance) = args.get("importance").and_then(Value::as_u64) {
            new.importance = importance.min(255) as u8;
        }
        new.sensitive = args.get("sensitive").and_then(Value::as_bool).unwrap_or(false);
        let supersedes = string_arg(&args, "supersedes");

        Ok(match policy::save(self.service.as_ref(), new, supersedes).await {
            Ok(SaveOutcome::Saved(record)) => {
                ToolResult::ok(format!("Remembered: {}", line(&record)))
            }
            Ok(SaveOutcome::Superseded { new, .. }) => {
                ToolResult::ok(format!("Updated. Now remembered: {}", line(&new)))
            }
            Ok(SaveOutcome::Duplicate(record)) => {
                ToolResult::ok(format!("Already remembered: {}", line(&record)))
            }
            Ok(SaveOutcome::Discarded) => ToolResult::error("There was nothing to save."),
            Err(_) => ToolResult::error("Memory is not available right now."),
        })
    }
}

pub struct MemoryForgetTool {
    service: Arc<dyn MemoryService>,
    audience: Audience,
}

impl MemoryForgetTool {
    pub fn new(service: Arc<dyn MemoryService>, audience: Audience) -> Self {
        Self { service, audience }
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: MEMORY_FORGET_TOOL_NAME.to_string(),
            description: "Permanently erases one remembered fact, by id. Use it when the user \
                          asks you to forget something. Pass the fact's text too so the user can \
                          see what is being erased."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "text": { "type": "string", "description": "The fact being erased." }
                },
                "required": ["id"]
            }),
        }
    }

    fn required_permission(&self, _args: &Value, _ctx: &ToolContext) -> PermissionTier {
        PermissionTier::Confirm
    }

    fn available_to_children(&self) -> bool { false }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let id = string_arg(&args, "id")
            .ok_or_else(|| ToolError("missing \"id\" argument".to_string()))?;
        // An id the profile may not see is treated as nonexistent.
        match self.service.get(id, self.audience).await {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(ToolResult::error("No remembered fact has that id.")),
            Err(_) => return Ok(ToolResult::error("Memory is not available right now.")),
        }
        Ok(match self.service.forget(id).await {
            Ok(true) => ToolResult::ok("Forgotten."),
            Ok(false) => ToolResult::error("No remembered fact has that id."),
            Err(_) => ToolResult::error("Memory is not available right now."),
        })
    }
}

/// The two read-only tools, for any agent including delegated sub-tasks and
/// mixed mode's local attempt.
pub fn read_tools(service: Arc<dyn MemoryService>, audience: Audience) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(MemorySearchTool::new(Arc::clone(&service), audience)),
        Arc::new(MemoryGetTool::new(service, audience)),
    ]
}

/// All four, for the top-level agent.
pub fn all_tools(
    service: Arc<dyn MemoryService>,
    audience: Audience,
    persona_name: Option<String>,
) -> Vec<Arc<dyn Tool>> {
    let mut tools = read_tools(Arc::clone(&service), audience);
    tools.push(Arc::new(MemoryRememberTool::new(Arc::clone(&service), persona_name)));
    tools.push(Arc::new(MemoryForgetTool::new(service, audience)));
    tools
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::{agent_runtime::ToolRegistry, memory::fake::InMemoryMemory};

    fn ctx() -> ToolContext {
        ToolContext {
            project_root: None,
            conversation_id: crate::conversation::ConversationId::from_session_path(Path::new(
                "/tmp/x",
            )),
        }
    }

    fn service() -> Arc<InMemoryMemory> { Arc::new(InMemoryMemory::new()) }

    async fn run(tool: &dyn Tool, args: Value) -> ToolResult {
        tool.execute(args, &ctx()).await.unwrap()
    }

    #[tokio::test]
    async fn remember_then_search_then_get_then_forget() {
        let svc = service();
        let tools = all_tools(svc.clone(), Audience::Local, Some("Aemeath".into()));
        let by_name =
            |name: &str| tools.iter().find(|t| t.definition().name == name).unwrap().clone();

        let saved = run(
            by_name("memory_remember").as_ref(),
            json!({"text": "Likes green tea", "tags": ["tea"]}),
        )
        .await;
        assert!(!saved.is_error, "{saved:?}");
        let record = svc.list(None).await.unwrap().remove(0);
        assert_eq!(record.persona_name.as_deref(), Some("Aemeath"));
        assert_eq!(record.origin, crate::memory::Origin::Told);

        let found = run(by_name("memory_search").as_ref(), json!({"query": "tea"})).await;
        assert!(found.content.contains(&record.id) && found.content.contains("green tea"));
        let got = run(by_name("memory_get").as_ref(), json!({"id": record.id})).await;
        assert!(got.content.contains("told by the user"));

        let gone = run(by_name("memory_forget").as_ref(), json!({"id": record.id})).await;
        assert!(!gone.is_error);
        assert!(svc.list(None).await.unwrap().is_empty());
        assert!(run(by_name("memory_forget").as_ref(), json!({"id": record.id})).await.is_error);
    }

    #[tokio::test]
    async fn a_correction_through_the_tool_supersedes_and_a_repeat_is_reported() {
        let svc = service();
        let remember = MemoryRememberTool::new(svc.clone(), None);
        run(&remember, json!({"text": "Lives in Taichung"})).await;
        let old = svc.list(None).await.unwrap().remove(0);
        let updated =
            run(&remember, json!({"text": "Lives in Taipei", "supersedes": old.id})).await;
        assert!(updated.content.starts_with("Updated"));
        assert_eq!(svc.history().await.unwrap().len(), 1);
        let again = run(&remember, json!({"text": "Lives in Taipei"})).await;
        assert!(again.content.starts_with("Already remembered"));
    }

    #[tokio::test]
    async fn a_cloud_profiles_tools_cannot_see_or_erase_a_sensitive_entry() {
        let svc = service();
        let secret = svc
            .add(NewMemory { sensitive: true, ..NewMemory::told("Takes a daily medication") })
            .await
            .unwrap();
        let tools = all_tools(svc.clone(), Audience::Cloud, None);
        for tool in &tools {
            match tool.definition().name.as_str() {
                "memory_search" => assert!(run(tool.as_ref(), json!({"query": "medication"}))
                    .await
                    .content
                    .starts_with("Nothing")),
                "memory_get" => {
                    assert!(run(tool.as_ref(), json!({"id": secret.id})).await.is_error)
                }
                "memory_forget" => {
                    assert!(run(tool.as_ref(), json!({"id": secret.id})).await.is_error)
                }
                _ => {}
            }
        }
        assert_eq!(svc.list(None).await.unwrap().len(), 1, "still there");
    }

    #[tokio::test]
    async fn a_sensitive_fact_the_user_asks_to_remember_is_saved_and_marked() {
        let svc = service();
        let remember = MemoryRememberTool::new(svc.clone(), None);
        let result =
            run(&remember, json!({"text": "Takes antidepressant medication every day"})).await;
        assert!(!result.is_error);
        let saved = svc.list(None).await.unwrap().remove(0);
        assert!(saved.sensitive, "told, sensitive-topic text defaults to hidden from cloud");
        let flagged =
            run(&remember, json!({"text": "Birthday is in March", "sensitive": true})).await;
        assert!(!flagged.is_error);
        assert!(svc.list(Some("birthday")).await.unwrap()[0].sensitive);
    }

    #[test]
    fn permissions_and_child_availability() {
        let tools = all_tools(service(), Audience::Local, None);
        for tool in &tools {
            let def = tool.definition();
            let tier = tool.required_permission(&json!({}), &ctx());
            match def.name.as_str() {
                "memory_search" | "memory_get" => {
                    assert_eq!(tier, PermissionTier::Auto);
                    assert!(tool.available_to_children());
                }
                "memory_remember" | "memory_forget" => {
                    assert_eq!(tier, PermissionTier::Confirm);
                    assert!(!tool.available_to_children());
                }
                other => panic!("unexpected tool {other}"),
            }
            let schema = def.parameters.to_string();
            for forbidden in ["scope", "owner", "persona", "namespace", "audience"] {
                assert!(!schema.contains(forbidden), "{} has a {forbidden} argument", def.name);
            }
        }
    }

    #[test]
    fn all_four_are_eager_and_a_child_registry_keeps_only_the_read_tools() {
        let registry = ToolRegistry::new(all_tools(service(), Audience::Local, None));
        let eager: Vec<String> = registry.eager_definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(eager.len(), 4);
        assert!(registry.stub_definitions().is_empty(), "never lazy");
        let child: Vec<String> =
            registry.for_child().eager_definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(child, vec!["memory_search", "memory_get"]);
    }

    #[tokio::test]
    async fn missing_arguments_are_reported_to_the_model() {
        let search = MemorySearchTool::new(service(), Audience::Local);
        assert!(search.execute(json!({}), &ctx()).await.is_err());
        let remember = MemoryRememberTool::new(service(), None);
        assert!(remember.execute(json!({"text": "  "}), &ctx()).await.is_err());
    }
}
