//! End-to-end checks of the model-dependent memory paths against a real local
//! Ollama, on a Traditional Chinese conversation. Run by hand:
//!
//! `cargo test -p fleet-snowfluff-ai --test memory_live -- --ignored
//! --nocapture`
//!
//! Set `OLLAMA_MODEL` to use a model other than `qwen3:14b`.

use fleet_snowfluff_ai::{
    memory::{
        background::{self, ProviderTextModel},
        extraction::extract_after_compaction,
        fake::InMemoryMemory,
        policy::SaveOutcome,
        Audience, MemoryService, NewMemory, Origin,
    },
    message::Message,
    Ollama,
};

fn model() -> ProviderTextModel {
    let name = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen3:14b".to_string());
    ProviderTextModel::new(Box::new(Ollama::new("http://localhost:11434", name)))
}

fn conversation() -> Vec<Message> {
    [
        ("user", "我家的貓叫麻糬，今天又不肯吃飯"),
        ("assistant", "咦～牠是不是又挑食了"),
        ("user", "可能吧，我下班回家才發現。我是做軟體工程師的，最近都加班"),
        ("assistant", "辛苦了～"),
        ("user", "對了我下個月要去京都玩"),
        ("assistant", "哇，好羨慕！"),
        ("user", "其實我最近在吃藥治療焦慮，睡不太好"),
        ("assistant", "要多休息喔"),
        ("user", "今天天氣真好"),
    ]
    .into_iter()
    .map(|(role, text)| if role == "user" { Message::user(text) } else { Message::assistant(text) })
    .collect()
}

#[tokio::test]
#[ignore = "requires a locally running Ollama with a model pulled"]
async fn extraction_over_a_real_chat_keeps_facts_and_drops_sensitive_ones() {
    let service = InMemoryMemory::new();
    let stored =
        extract_after_compaction(&model(), &service, &conversation(), "live:0-9", Some("Aemeath"))
            .await
            .expect("the model answered");
    let all = service.list(None).await.unwrap();
    for memory in &all {
        println!(
            "extracted: {} ({:?}, importance {})",
            memory.text, memory.kind, memory.importance
        );
    }
    assert!(stored >= 1, "at least one durable fact should be found");
    assert!(all.iter().all(|m| m.origin == Origin::Auto));
    assert!(
        all.iter().all(|m| !m.text.contains('藥') && !m.text.contains("焦慮")),
        "nothing about the medication may be saved: {all:?}"
    );
    assert!(
        all.iter()
            .any(|m| m.text.contains("麻糬") || m.text.contains("京都") || m.text.contains("軟體")),
        "one of the plain facts should be present: {all:?}"
    );
}

#[tokio::test]
#[ignore = "requires a locally running Ollama with a model pulled"]
async fn a_correction_supersedes_without_the_assistant_calling_any_tool() {
    let service = InMemoryMemory::new();
    let old = service.add(NewMemory::told("住在台中")).await.unwrap();
    service.add(NewMemory::told("喜歡喝綠茶")).await.unwrap();
    service.add(NewMemory::told("養了一隻貓")).await.unwrap();
    let outcome =
        background::correct_after_reply(&model(), &service, "不對，我現在搬到台北住了", None)
            .await
            .expect("a correction cue was present");
    println!("outcome: {outcome:?}");
    match outcome {
        SaveOutcome::Superseded { old_id, new } => {
            assert_eq!(old_id, old.id);
            assert!(new.text.contains("台北"));
        }
        other => panic!("expected the old residence to be replaced, got {other:?}"),
    }
    let active = service.active(Audience::Local).await.unwrap();
    assert!(active.iter().all(|m| m.id != old.id));
    assert_eq!(service.history().await.unwrap().len(), 1);
}

#[tokio::test]
#[ignore = "requires a locally running Ollama with a model pulled"]
async fn asking_to_remember_saves_a_fact_even_with_no_tool_call() {
    let service = InMemoryMemory::new();
    let outcome = background::remember_after_reply(
        &model(),
        &service,
        "幫我記一下，我喜歡無糖綠茶",
        false,
        Some("Aemeath"),
    )
    .await
    .expect("a remember cue was present");
    println!("outcome: {outcome:?}");
    assert!(matches!(outcome, SaveOutcome::Saved(_)));
    let saved = service.list(None).await.unwrap();
    assert_eq!(saved.len(), 1);
    assert!(saved[0].text.contains("綠茶"), "{saved:?}");
    assert_eq!(saved[0].origin, Origin::Told);
}
