//! Deriving memories from the turns a compaction just folded into the summary.
//! Runs once per compaction, on the same background model, and only ever
//! proposes: every candidate goes through the sensitive-topic screen and the
//! write policy, is saved active and marked `auto`, and never replaces
//! an existing memory.

use super::{
    background::{self, TextModel},
    rank, Audience, MemoryError, MemoryService,
};
use crate::message::{Message, Role};

/// How many known facts are listed so the model does not repeat them.
const KNOWN_FACTS_BUDGET_TOKENS: usize = 400;

fn transcript(folded: &[Message]) -> String {
    folded
        .iter()
        .filter(|message| matches!(message.role, Role::User | Role::Assistant))
        .map(|message| {
            let speaker = if message.role == Role::User { "user" } else { "assistant" };
            format!("{speaker}: {}", message.content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extracts and saves memories from `folded`. Returns how many were stored. An
/// error from the model or the store is returned for logging; the caller treats
/// it as "no memories this time" because the compaction itself has already
/// succeeded.
pub async fn extract_after_compaction(
    model: &dyn TextModel,
    service: &dyn MemoryService,
    folded: &[Message],
    source: &str,
    persona_name: Option<&str>,
) -> Result<usize, String> {
    let transcript = transcript(folded);
    if transcript.trim().is_empty() {
        return Ok(0);
    }
    let known = known_facts(service).await.map_err(|err| err.to_string())?;
    let user = format!("Already known:\n{known}\n\nConversation:\n{transcript}");
    let reply = model.complete(&background::extraction_system(), &user).await?;
    let candidates = background::parse_candidates(&reply);
    background::save_candidates(service, candidates, Some(source), persona_name)
        .await
        .map_err(|err| err.to_string())
}

async fn known_facts(service: &dyn MemoryService) -> Result<String, MemoryError> {
    let active = service.active(Audience::Local).await?;
    let ranked = rank::digest(active, chrono::Utc::now(), KNOWN_FACTS_BUDGET_TOKENS);
    Ok(if ranked.is_empty() {
        "(nothing yet)".to_string()
    } else {
        ranked.iter().map(rank::render_line).collect::<Vec<_>>().join("\n")
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::memory::{fake::InMemoryMemory, NewMemory, Origin};

    struct Scripted(Mutex<Option<Result<String, String>>>, Mutex<Vec<String>>);

    impl Scripted {
        fn replying(reply: &str) -> Self {
            Self(Mutex::new(Some(Ok(reply.to_string()))), Mutex::new(vec![]))
        }

        fn failing() -> Self {
            Self(Mutex::new(Some(Err("model down".into()))), Mutex::new(vec![]))
        }
    }

    #[async_trait]
    impl TextModel for Scripted {
        async fn complete(&self, _system: &str, user: &str) -> Result<String, String> {
            self.1.lock().unwrap().push(user.to_string());
            self.0.lock().unwrap().take().unwrap_or(Err("no reply".into()))
        }
    }

    fn folded() -> Vec<Message> {
        vec![
            Message::user("我家的貓叫麻糬"),
            Message::assistant("好可愛"),
            Message::user("我最近在吃藥治療焦慮"),
        ]
    }

    #[tokio::test]
    async fn candidates_are_stored_active_with_the_auto_badge() {
        let service = InMemoryMemory::new();
        let model = Scripted::replying(
            r#"[{"text": "養了一隻叫麻糬的貓", "kind": "life_context", "importance": 3, "tags": ["貓", "cat"]}]"#,
        );
        let stored =
            extract_after_compaction(&model, &service, &folded(), "s.jsonl:0-3", Some("Aemeath"))
                .await
                .unwrap();
        assert_eq!(stored, 1);
        let memory = service.list(None).await.unwrap().remove(0);
        assert_eq!(memory.origin, Origin::Auto);
        assert_eq!(memory.source.as_deref(), Some("s.jsonl:0-3"));
        assert_eq!(memory.status, crate::memory::Status::Active);
        let sent = model.1.lock().unwrap()[0].clone();
        assert!(sent.contains("麻糬") && sent.starts_with("Already known:"));
    }

    #[tokio::test]
    async fn duplicates_are_skipped_and_nothing_is_superseded() {
        let service = InMemoryMemory::new();
        let existing = service.add(NewMemory::told("養了一隻叫麻糬的貓")).await.unwrap();
        let model =
            Scripted::replying(r#"[{"text": "養了一隻叫麻糬的貓"}, {"text": "工作是軟體工程師"}]"#);
        let stored =
            extract_after_compaction(&model, &service, &folded(), "s", None).await.unwrap();
        assert_eq!(stored, 1);
        assert_eq!(service.list(None).await.unwrap().len(), 2);
        assert!(service.history().await.unwrap().is_empty());
        assert!(service.get(&existing.id, Audience::Local).await.unwrap().is_some());
        let sent = model.1.lock().unwrap()[0].clone();
        assert!(sent.contains("- 養了一隻叫麻糬的貓"), "known facts are listed: {sent}");
    }

    #[tokio::test]
    async fn a_medication_candidate_is_dropped_even_when_the_model_returns_it() {
        let service = InMemoryMemory::new();
        let model =
            Scripted::replying(r#"[{"text": "正在吃藥治療焦慮"}, {"text": "養了一隻叫麻糬的貓"}]"#);
        extract_after_compaction(&model, &service, &folded(), "s", None).await.unwrap();
        let texts: Vec<String> =
            service.list(None).await.unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(texts, vec!["養了一隻叫麻糬的貓"]);
    }

    #[tokio::test]
    async fn a_failed_model_call_is_an_error_for_the_caller_and_stores_nothing() {
        let service = InMemoryMemory::new();
        let result =
            extract_after_compaction(&Scripted::failing(), &service, &folded(), "s", None).await;
        assert!(result.is_err());
        assert!(service.list(None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn garbage_from_the_model_stores_nothing() {
        let service = InMemoryMemory::new();
        let model = Scripted::replying("I'm sorry, I can't help with that.");
        assert_eq!(
            extract_after_compaction(&model, &service, &folded(), "s", None).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn an_empty_fold_makes_no_model_call() {
        let service = InMemoryMemory::new();
        let model = Scripted::replying("[]");
        assert_eq!(extract_after_compaction(&model, &service, &[], "s", None).await.unwrap(), 0);
        assert!(model.1.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_sensitive_fact_the_user_asked_to_remember_is_saved_by_the_tool_but_never_by_extraction(
    ) {
        use crate::{
            agent_tool::{Tool, ToolContext},
            native_tools::memory_tools::MemoryRememberTool,
        };
        let service = std::sync::Arc::new(InMemoryMemory::new());
        let remember = MemoryRememberTool::new(service.clone(), None);
        let ctx = ToolContext {
            project_root: None,
            conversation_id: crate::conversation::ConversationId::from_session_path(
                std::path::Path::new("/tmp/x"),
            ),
        };
        remember.execute(serde_json::json!({"text": "正在吃藥治療焦慮"}), &ctx).await.unwrap();
        assert_eq!(service.list(None).await.unwrap().len(), 1, "saved on request");

        let model = Scripted::replying(r#"[{"text": "正在吃藥治療焦慮"}]"#);
        let stored =
            extract_after_compaction(&model, service.as_ref(), &folded(), "s", None).await.unwrap();
        assert_eq!(stored, 0);
        assert_eq!(service.list(None).await.unwrap().len(), 1);
    }
}
