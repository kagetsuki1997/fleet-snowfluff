//! Behavior every [`MemoryService`] must have. Run against the in-memory fake
//! and the SQLite store, so the fake used in other tests cannot drift from the
//! real thing.

use super::*;

fn tagged(text: &str, tags: &[&str]) -> NewMemory {
    NewMemory { tags: tags.iter().map(|t| t.to_string()).collect(), ..NewMemory::told(text) }
}

fn sensitive(text: &str) -> NewMemory { NewMemory { sensitive: true, ..NewMemory::told(text) } }

async fn texts(service: &dyn MemoryService, query: &str, audience: Audience) -> Vec<String> {
    service
        .search(query, 10, audience)
        .await
        .unwrap()
        .into_iter()
        .map(|hit| hit.record.text)
        .collect()
}

pub(crate) async fn run<S: MemoryService>(make: impl Fn() -> S) {
    a_memory_round_trips_through_add_and_get(&make()).await;
    search_matches_text_and_attached_tags(&make()).await;
    search_matches_across_traditional_and_simplified(&make()).await;
    a_small_store_still_finds_a_true_match(&make()).await;
    superseding_hides_the_old_memory_but_keeps_it_in_history(&make()).await;
    superseding_something_missing_or_already_replaced_saves_nothing(&make()).await;
    forgetting_removes_every_trace(&make()).await;
    erasing_everything_includes_superseded_history(&make()).await;
    a_cloud_audience_never_sees_a_sensitive_memory(&make()).await;
    editing_changes_the_text_and_reindexes_it(&make()).await;
    editing_pinned_and_sensitive_and_a_missing_id(&make()).await;
    the_settings_list_filters_by_text_and_shows_sensitive_ones(&make()).await;
    importance_is_clamped_to_one_through_three(&make()).await;
    the_persona_name_is_recorded_but_never_filters(&make()).await;
    marking_used_records_when_a_memory_was_shown(&make()).await;
}

async fn a_memory_round_trips_through_add_and_get(service: &dyn MemoryService) {
    let a = service.add(tagged("Likes unsweetened green tea", &["tea"])).await.unwrap();
    let b = service.add(NewMemory::auto("Has a cat named Mochi")).await.unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.status, Status::Active);
    assert_eq!(a.origin, Origin::Told);
    assert_eq!(b.origin, Origin::Auto);
    let fetched = service.get(&a.id, Audience::Local).await.unwrap().unwrap();
    assert_eq!(fetched, a);
    assert!(service.get("missing", Audience::Local).await.unwrap().is_none());
}

async fn search_matches_text_and_attached_tags(service: &dyn MemoryService) {
    service.add(tagged("喜歡喝無糖綠茶", &["green tea", "tea"])).await.unwrap();
    service.add(NewMemory::told("養了一隻叫麻糬的貓")).await.unwrap();
    service.add(NewMemory::told("工作是軟體工程師")).await.unwrap();
    assert_eq!(texts(service, "今天想喝綠茶", Audience::Local).await, vec!["喜歡喝無糖綠茶"]);
    assert_eq!(texts(service, "green tea", Audience::Local).await, vec!["喜歡喝無糖綠茶"]);
    assert_eq!(texts(service, "麻糬", Audience::Local).await, vec!["養了一隻叫麻糬的貓"]);
}

async fn search_matches_across_traditional_and_simplified(service: &dyn MemoryService) {
    service.add(NewMemory::told("学习日语")).await.unwrap();
    service.add(NewMemory::told("明天要去京都旅行")).await.unwrap();
    assert_eq!(texts(service, "學習", Audience::Local).await, vec!["学习日语"]);
}

async fn a_small_store_still_finds_a_true_match(service: &dyn MemoryService) {
    // With one or two memories a ranking score is tiny; a fixed cutoff would
    // discard the match.
    service.add(NewMemory::told("Has a cat named Mochi")).await.unwrap();
    assert_eq!(
        texts(service, "Mochi is not eating", Audience::Local).await,
        vec!["Has a cat named Mochi"]
    );
    service.add(NewMemory::told("Works as a software engineer")).await.unwrap();
    assert_eq!(
        texts(service, "Mochi is not eating", Audience::Local).await,
        vec!["Has a cat named Mochi"]
    );
}

async fn superseding_hides_the_old_memory_but_keeps_it_in_history(service: &dyn MemoryService) {
    let old = service.add(NewMemory::told("住在台中")).await.unwrap();
    service.add(NewMemory::told("喜歡喝無糖綠茶")).await.unwrap();
    let new = service.supersede(&old.id, NewMemory::told("住在台北")).await.unwrap().unwrap();

    assert!(texts(service, "台中", Audience::Local).await.is_empty());
    assert_eq!(texts(service, "台北", Audience::Local).await, vec!["住在台北"]);
    let active = service.active(Audience::Local).await.unwrap();
    assert!(active.iter().all(|r| r.id != old.id));
    assert!(active.iter().any(|r| r.id == new.id));

    let history = service.history().await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, old.id);
    assert_eq!(history[0].status, Status::Superseded);
    assert_eq!(history[0].superseded_by.as_deref(), Some(new.id.as_str()));
    assert!(service.get(&old.id, Audience::Local).await.unwrap().is_some(), "still viewable by id");
}

async fn superseding_something_missing_or_already_replaced_saves_nothing(
    service: &dyn MemoryService,
) {
    assert!(service.supersede("missing", NewMemory::told("x")).await.unwrap().is_none());
    let old = service.add(NewMemory::told("住在台中")).await.unwrap();
    service.supersede(&old.id, NewMemory::told("住在台北")).await.unwrap().unwrap();
    let count_before = service.active(Audience::Local).await.unwrap().len();
    assert!(service.supersede(&old.id, NewMemory::told("住在高雄")).await.unwrap().is_none());
    assert_eq!(service.active(Audience::Local).await.unwrap().len(), count_before);
}

async fn forgetting_removes_every_trace(service: &dyn MemoryService) {
    let memory = service.add(tagged("喜歡喝無糖綠茶", &["tea"])).await.unwrap();
    service.add(NewMemory::told("工作是軟體工程師")).await.unwrap();
    let old = service.add(NewMemory::told("住在台中")).await.unwrap();
    service.supersede(&old.id, NewMemory::told("住在台北")).await.unwrap();

    assert!(service.forget(&memory.id).await.unwrap());
    assert!(service.get(&memory.id, Audience::Local).await.unwrap().is_none());
    assert!(texts(service, "綠茶", Audience::Local).await.is_empty());
    assert!(texts(service, "tea", Audience::Local).await.is_empty());
    assert!(service.list(None).await.unwrap().iter().all(|r| r.id != memory.id));
    assert!(!service.forget(&memory.id).await.unwrap(), "forgetting twice is a no-op");

    assert!(service.forget(&old.id).await.unwrap());
    assert!(service.history().await.unwrap().is_empty(), "a superseded memory can be erased too");
}

async fn erasing_everything_includes_superseded_history(service: &dyn MemoryService) {
    let old = service.add(NewMemory::told("住在台中")).await.unwrap();
    service.supersede(&old.id, NewMemory::told("住在台北")).await.unwrap();
    service.add(NewMemory::told("工作是軟體工程師")).await.unwrap();

    assert_eq!(service.erase_all().await.unwrap(), 3);
    assert!(service.active(Audience::Local).await.unwrap().is_empty());
    assert!(service.history().await.unwrap().is_empty());
    assert!(texts(service, "台北", Audience::Local).await.is_empty());
}

async fn a_cloud_audience_never_sees_a_sensitive_memory(service: &dyn MemoryService) {
    let secret = service.add(sensitive("Takes a daily medication")).await.unwrap();
    service.add(NewMemory::told("Likes green tea")).await.unwrap();

    assert!(texts(service, "medication", Audience::Cloud).await.is_empty());
    assert!(service.get(&secret.id, Audience::Cloud).await.unwrap().is_none());
    assert!(service.active(Audience::Cloud).await.unwrap().iter().all(|r| r.id != secret.id));

    assert_eq!(
        texts(service, "medication", Audience::Local).await,
        vec!["Takes a daily medication"]
    );
    assert!(service.get(&secret.id, Audience::Local).await.unwrap().is_some());
}

async fn editing_changes_the_text_and_reindexes_it(service: &dyn MemoryService) {
    let memory = service.add(tagged("Likes tea", &["drink"])).await.unwrap();
    service.add(NewMemory::told("Works as a software engineer")).await.unwrap();
    let edited = service
        .edit(&memory.id, MemoryEdit { text: Some("Likes coffee".into()), ..Default::default() })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(edited.text, "Likes coffee");
    assert!(
        texts(service, "tea", Audience::Local).await.is_empty(),
        "the old words no longer match"
    );
    assert_eq!(texts(service, "coffee", Audience::Local).await, vec!["Likes coffee"]);
    assert_eq!(
        texts(service, "drink", Audience::Local).await,
        vec!["Likes coffee"],
        "tags survive"
    );

    service
        .edit(&memory.id, MemoryEdit { tags: Some(vec!["beverage".into()]), ..Default::default() })
        .await
        .unwrap();
    assert!(texts(service, "drink", Audience::Local).await.is_empty());
    assert_eq!(texts(service, "beverage", Audience::Local).await, vec!["Likes coffee"]);
}

async fn editing_pinned_and_sensitive_and_a_missing_id(service: &dyn MemoryService) {
    let memory = service.add(NewMemory::told("Has a cat")).await.unwrap();
    let pinned = service
        .edit(&memory.id, MemoryEdit { pinned: Some(true), ..Default::default() })
        .await
        .unwrap()
        .unwrap();
    assert!(pinned.pinned);
    let hidden = service
        .edit(&memory.id, MemoryEdit { sensitive: Some(true), ..Default::default() })
        .await
        .unwrap()
        .unwrap();
    assert!(hidden.sensitive);
    assert!(service.get(&memory.id, Audience::Cloud).await.unwrap().is_none());
    assert!(service.edit("missing", MemoryEdit::default()).await.unwrap().is_none());
}

async fn the_settings_list_filters_by_text_and_shows_sensitive_ones(service: &dyn MemoryService) {
    service.add(NewMemory::told("Likes green tea")).await.unwrap();
    service.add(sensitive("Takes a daily medication")).await.unwrap();
    assert_eq!(
        service.list(None).await.unwrap().len(),
        2,
        "the user's own list includes sensitive ones"
    );
    let filtered = service.list(Some("TEA")).await.unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].text, "Likes green tea");
}

async fn importance_is_clamped_to_one_through_three(service: &dyn MemoryService) {
    let high = service.add(NewMemory { importance: 9, ..NewMemory::told("a") }).await.unwrap();
    let low = service.add(NewMemory { importance: 0, ..NewMemory::told("b") }).await.unwrap();
    assert_eq!(high.importance, 3);
    assert_eq!(low.importance, 1);
}

async fn the_persona_name_is_recorded_but_never_filters(service: &dyn MemoryService) {
    let memory = service
        .add(NewMemory {
            persona_name: Some("Aemeath".into()),
            ..NewMemory::told("Likes green tea")
        })
        .await
        .unwrap();
    assert_eq!(memory.persona_name.as_deref(), Some("Aemeath"));
    assert_eq!(texts(service, "green tea", Audience::Local).await, vec!["Likes green tea"]);
}

async fn marking_used_records_when_a_memory_was_shown(service: &dyn MemoryService) {
    let memory = service.add(NewMemory::told("Likes tea")).await.unwrap();
    assert!(memory.last_used_at.is_none());
    service.mark_used(std::slice::from_ref(&memory.id)).await.unwrap();
    let after = service.get(&memory.id, Audience::Local).await.unwrap().unwrap();
    assert!(after.last_used_at.is_some());
}
