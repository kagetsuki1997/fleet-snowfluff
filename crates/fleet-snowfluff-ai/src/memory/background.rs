//! Memory work the host does on a background model, off the chat's critical
//! path: noticing that the user asked to remember or corrected something and
//! saving it even when the assistant made no tool call, and deriving candidate
//! facts from older turns (extraction). Every save goes through
//! `memory::policy`, and every prompt here is English whatever language the
//! conversation is in.

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::Value;

use super::{
    policy::{self, SaveOutcome},
    Audience, Kind, MemoryError, MemoryService, NewMemory,
};
use crate::{message::Message, provider::AiProvider, sensitive};

/// One system-and-user exchange with a model, returning its text. Implemented
/// over a real provider by [`ProviderTextModel`]; tests use a scripted one.
#[async_trait]
pub trait TextModel: Send + Sync {
    async fn complete(&self, system: &str, user: &str) -> Result<String, String>;
}

pub struct ProviderTextModel {
    provider: Box<dyn AiProvider>,
}

impl ProviderTextModel {
    pub fn new(provider: Box<dyn AiProvider>) -> Self { Self { provider } }
}

#[async_trait]
impl TextModel for ProviderTextModel {
    async fn complete(&self, system: &str, user: &str) -> Result<String, String> {
        let mut stream = self
            .provider
            .chat(vec![Message::system(system), Message::user(user)])
            .await
            .map_err(|err| err.to_string())?;
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk.map_err(|err| err.to_string())?.delta);
        }
        Ok(text)
    }
}

/// Phrases that mean "remember this", per language. Matching is a cheap gate
/// for the model call, so a false positive costs one request and a miss costs
/// nothing but the backstop.
const REMEMBER_CUES: &[&str] = &[
    // English
    "remember that",
    "remember this",
    "please remember",
    "don't forget",
    "do not forget",
    "keep in mind",
    "make a note",
    // Traditional and Simplified Chinese
    "記住",
    "幫我記",
    "請記得",
    "別忘了",
    "不要忘記",
    "记住",
    "帮我记",
    "请记得",
    "别忘了",
    "不要忘记",
    // Japanese
    "覚えておいて",
    "覚えといて",
    "覚えてて",
    "忘れないで",
    // Korean
    "기억해",
    "잊지 마",
    "잊지마",
];

/// Phrases that suggest the user is correcting or updating something about
/// themselves.
const CORRECTION_CUES: &[&str] = &[
    // English
    "actually",
    "that's wrong",
    "that is wrong",
    "that's not right",
    "not anymore",
    "no longer",
    "i moved",
    "correction",
    "no, i",
    "i changed",
    // Traditional and Simplified Chinese
    "不對",
    "錯了",
    "更正",
    "其實我",
    "我現在",
    "已經不",
    "不再",
    "不对",
    "错了",
    "其实我",
    "我现在",
    "已经不",
    // Japanese
    "違う",
    "間違",
    "訂正",
    "今は",
    "もうやめ",
    "引っ越した",
    // Korean
    "아니야",
    "틀렸",
    "정정",
    "지금은",
    "이제는",
    "이사했",
];

fn contains_any(text: &str, cues: &[&str]) -> bool {
    let lower = text.to_lowercase().replace('’', "'");
    cues.iter().any(|cue| lower.contains(cue))
}

pub fn has_remember_cue(text: &str) -> bool { contains_any(text, REMEMBER_CUES) }

pub fn has_correction_cue(text: &str) -> bool { contains_any(text, CORRECTION_CUES) }

/// Finds the first JSON value of the wanted shape in a model's reply,
/// tolerating prose or code fences around it.
fn json_in(text: &str, open: char, close: char) -> Option<Value> {
    let start = text.find(open)?;
    let end = text.rfind(close)?;
    (end > start).then(|| serde_json::from_str(&text[start..=end]).ok()).flatten()
}

fn str_field(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn memory_from(value: &Value, text: String) -> NewMemory {
    let mut new = NewMemory::told(text);
    if let Some(kind) = str_field(value, "kind") {
        new.kind = Kind::parse(&kind);
    }
    if let Some(importance) = value.get("importance").and_then(Value::as_u64) {
        new.importance = importance.min(255) as u8;
    }
    if let Some(tags) = value.get("tags").and_then(Value::as_array) {
        new.tags = tags
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
    }
    new
}

const FIELDS: &str = "{\"text\": a short statement without the subject (at most 20 words), \
                      \"kind\": \"preference\" | \"life_context\" | \"shared_moment\" | \
                      \"other\", \"importance\": 1, 2 or 3, \"tags\": 2 to 4 keywords, in the \
                      user's language and in English}";

pub fn remember_system() -> String {
    format!(
        "The user asked the assistant to remember something. Return ONLY JSON of the form \
         {FIELDS} for the one thing they asked to be remembered, written in the user's language. \
         If they did not actually ask to remember a fact about themselves, return null."
    )
}

/// After a reply, saves what the user asked to remember when the assistant did
/// not already save it with its own tool call (`tool_saved`). Returns what was
/// saved, if anything.
pub async fn remember_after_reply(
    model: &dyn TextModel,
    service: &dyn MemoryService,
    user_message: &str,
    tool_saved: bool,
    persona_name: Option<&str>,
) -> Option<SaveOutcome> {
    if tool_saved || !has_remember_cue(user_message) {
        return None;
    }
    let reply = model.complete(&remember_system(), user_message).await.ok()?;
    let value = json_in(&reply, '{', '}')?;
    let text = str_field(&value, "text")?;
    let mut new = memory_from(&value, text);
    new.persona_name = persona_name.map(str::to_string);
    policy::save(service, new, None).await.ok()
}

pub fn correction_system() -> String {
    "The user may be correcting something stored about them. Decide which stored memory, if any, \
     the new fact replaces (same subject, now outdated). Return ONLY JSON: {\"replaces\": \"<id>\" \
     or null, \"text\": \"<the corrected memory, short, in the user's language>\"}. Only choose an \
     id from the list. If the message does not state a corrected fact about the user, return \
     {\"replaces\": null, \"text\": null}."
        .to_string()
}

/// How many stored memories are offered as candidates to replace.
const CANDIDATES: usize = 5;

/// On a correction cue, asks the model which of the closest stored memories the
/// new fact replaces and saves it, superseding that one. A reply naming an id
/// outside the offered list replaces nothing.
pub async fn correct_after_reply(
    model: &dyn TextModel,
    service: &dyn MemoryService,
    user_message: &str,
    persona_name: Option<&str>,
) -> Option<SaveOutcome> {
    if !has_correction_cue(user_message) {
        return None;
    }
    // Local audience: the user's own sensitive entries can be corrected too; the
    // text only goes to the background model the user already chose for this
    // work.
    let mut candidates: Vec<_> = service
        .search(user_message, CANDIDATES, Audience::Local)
        .await
        .ok()?
        .into_iter()
        .map(|hit| hit.record)
        .collect();
    // A corrected fact often shares no words with the old one ("Taichung" versus
    // "Taipei"), so the list is filled out with the best-ranked memories rather
    // than left to keyword luck.
    if candidates.len() < CANDIDATES {
        let ranked = super::rank::digest(
            service.active(Audience::Local).await.ok()?,
            chrono::Utc::now(),
            super::rank::DIGEST_BUDGET_TOKENS,
        );
        for record in ranked {
            if candidates.len() >= CANDIDATES {
                break;
            }
            if !candidates.iter().any(|c| c.id == record.id) {
                candidates.push(record);
            }
        }
    }
    let listing = if candidates.is_empty() {
        "(none)".to_string()
    } else {
        candidates.iter().map(|c| format!("- {}: {}", c.id, c.text)).collect::<Vec<_>>().join("\n")
    };
    let reply = model
        .complete(
            &correction_system(),
            &format!("New message: {user_message}\nStored memories:\n{listing}"),
        )
        .await
        .ok()?;
    let value = json_in(&reply, '{', '}')?;
    let text = str_field(&value, "text")?;
    let replaces =
        str_field(&value, "replaces").filter(|id| candidates.iter().any(|c| &c.id == id));
    let mut new = NewMemory::told(text);
    new.kind = Kind::Correction;
    new.persona_name = persona_name.map(str::to_string);
    policy::save(service, new, replaces.as_deref()).await.ok()
}

/// A fact derived from older conversation turns, not yet saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub kind: Kind,
    pub importance: u8,
    pub tags: Vec<String>,
}

pub fn extraction_system() -> String {
    format!(
        "From the conversation, list durable facts about the USER that would still be useful in \
         later conversations: preferences, pets, work, family, plans, habits, important \
         dates.\nReturn ONLY a JSON array, possibly empty. Each item: {FIELDS}.\nRules:\n- Do NOT \
         include anything about health, medication, money or debt, passwords or credentials, \
         sexuality, religion, or politics, even if it was mentioned.\n- Skip temporary states \
         (what they ate today, today's mood), one-off plans for the coming days or weeks, and \
         anything the assistant said.\n- Do not repeat anything listed under Already known.\n- If \
         nothing qualifies, return []."
    )
}

/// Reads the extraction reply leniently: anything that is not a JSON array
/// yields no candidates, an item without text is skipped, and every candidate
/// the sensitive-topic screen matches is dropped even though the prompt already
/// asked the model to leave such facts out.
pub fn parse_candidates(reply: &str) -> Vec<Candidate> {
    let Some(Value::Array(items)) = json_in(reply, '[', ']') else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| {
            let text = str_field(item, "text").filter(|text| text.chars().count() <= 200)?;
            if sensitive::is_sensitive(&text) {
                return None;
            }
            let new = memory_from(item, text.clone());
            if new.tags.iter().any(|tag| sensitive::is_sensitive(tag)) {
                return None;
            }
            Some(Candidate { text, kind: new.kind, importance: new.importance, tags: new.tags })
        })
        .collect()
}

/// Saves candidates as automatically derived memories through the write policy.
/// Returns how many were stored.
pub async fn save_candidates(
    service: &dyn MemoryService,
    candidates: Vec<Candidate>,
    source: Option<&str>,
    persona_name: Option<&str>,
) -> Result<usize, MemoryError> {
    let mut stored = 0;
    for candidate in candidates {
        let mut new = NewMemory::auto(candidate.text);
        new.kind = candidate.kind;
        new.importance = candidate.importance;
        new.tags = candidate.tags;
        new.source = source.map(str::to_string);
        new.persona_name = persona_name.map(str::to_string);
        if matches!(policy::save(service, new, None).await?, SaveOutcome::Saved(_)) {
            stored += 1;
        }
    }
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::memory::{fake::InMemoryMemory, Origin};

    struct Scripted {
        reply: Mutex<Option<Result<String, String>>>,
        seen: Mutex<Vec<(String, String)>>,
    }

    impl Scripted {
        fn replying(reply: &str) -> Self {
            Self { reply: Mutex::new(Some(Ok(reply.to_string()))), seen: Mutex::new(vec![]) }
        }

        fn calls(&self) -> usize { self.seen.lock().unwrap().len() }
    }

    #[async_trait]
    impl TextModel for Scripted {
        async fn complete(&self, system: &str, user: &str) -> Result<String, String> {
            self.seen.lock().unwrap().push((system.to_string(), user.to_string()));
            self.reply.lock().unwrap().take().unwrap_or(Err("no reply scripted".into()))
        }
    }

    #[test]
    fn remember_phrases_match_in_all_five_languages_and_ordinary_chat_does_not() {
        for yes in [
            "Please remember that I'm allergic to cats' hair — I mean I like cats",
            "Don't forget my sister's birthday is in March",
            "記住我喜歡無糖綠茶",
            "帮我记一下我下个月去京都",
            "私は猫が好きだから覚えておいてね",
            "내 생일은 3월이야, 기억해줘",
        ] {
            assert!(has_remember_cue(yes), "{yes}");
        }
        for no in [
            "How is the weather",
            "今天天氣真好",
            "今日はいい天気だね",
            "오늘 날씨 좋다",
            "I forgot my keys",
        ] {
            assert!(!has_remember_cue(no), "{no}");
        }
    }

    #[test]
    fn correction_phrases_match_in_all_five_languages_and_ordinary_chat_does_not() {
        for yes in [
            "No, I live in Taipei now. Actually I moved last year",
            "不對，我現在搬到台北住了",
            "不对，我现在搬到台北住了",
            "違うよ、今は台北に住んでるの",
            "아니야, 지금은 타이베이에 살아",
        ] {
            assert!(has_correction_cue(yes), "{yes}");
        }
        for no in ["I had pasta for lunch", "我家的貓叫麻糬", "うちの猫のモチ", "우리 고양이 모찌"]
        {
            assert!(!has_correction_cue(no), "{no}");
        }
    }

    #[tokio::test]
    async fn the_backstop_saves_what_was_asked_when_the_assistant_made_no_tool_call() {
        let service = InMemoryMemory::new();
        let model = Scripted::replying(
            r#"```json
{"text": "喜歡無糖綠茶", "kind": "preference", "importance": 2, "tags": ["綠茶", "green tea"]}
```"#,
        );
        let outcome =
            remember_after_reply(&model, &service, "記住我喜歡無糖綠茶", false, Some("Aemeath"))
                .await;
        assert!(matches!(outcome, Some(SaveOutcome::Saved(_))));
        let saved = service.list(None).await.unwrap().remove(0);
        assert_eq!(saved.origin, Origin::Told);
        assert_eq!(saved.kind, Kind::Preference);
        assert_eq!(saved.persona_name.as_deref(), Some("Aemeath"));
    }

    #[tokio::test]
    async fn the_backstop_does_nothing_when_the_tool_already_saved_it_or_there_is_no_cue() {
        let service = InMemoryMemory::new();
        let model = Scripted::replying(r#"{"text": "x"}"#);
        assert!(remember_after_reply(&model, &service, "Remember that I like tea", true, None)
            .await
            .is_none());
        assert!(remember_after_reply(&model, &service, "I like tea", false, None).await.is_none());
        assert_eq!(model.calls(), 0, "no model call without a reason");
        assert!(service.list(None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_backstop_never_writes_twice_for_the_same_fact() {
        let service = InMemoryMemory::new();
        service.add(NewMemory::told("Likes green tea")).await.unwrap();
        let model = Scripted::replying(r#"{"text": "likes GREEN tea"}"#);
        let outcome =
            remember_after_reply(&model, &service, "Remember that I like green tea", false, None)
                .await;
        assert!(matches!(outcome, Some(SaveOutcome::Duplicate(_))));
        assert_eq!(service.list(None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_model_that_returns_null_or_junk_saves_nothing() {
        let service = InMemoryMemory::new();
        for reply in ["null", "I cannot do that", "{\"text\": \"\"}"] {
            let model = Scripted::replying(reply);
            assert!(remember_after_reply(
                &model,
                &service,
                "Remember that I like tea",
                false,
                None
            )
            .await
            .is_none());
        }
        assert!(service.list(None).await.unwrap().is_empty());
    }

    async fn seeded() -> (InMemoryMemory, String) {
        let service = InMemoryMemory::new();
        let taichung = service.add(NewMemory::told("住在台中")).await.unwrap();
        service.add(NewMemory::told("喜歡喝綠茶")).await.unwrap();
        (service, taichung.id)
    }

    #[tokio::test]
    async fn a_correction_supersedes_the_chosen_memory_without_any_tool_call() {
        let (service, id) = seeded().await;
        let model = Scripted::replying(&format!(r#"{{"replaces": "{id}", "text": "住在台北"}}"#));
        let outcome = correct_after_reply(&model, &service, "不對，我現在搬到台北住了", None).await;
        assert!(
            matches!(outcome, Some(SaveOutcome::Superseded { ref old_id, .. }) if old_id == &id)
        );
        assert_eq!(service.history().await.unwrap()[0].id, id);
        let (_, user) = model.seen.lock().unwrap()[0].clone();
        assert!(
            user.contains(&id) && user.contains("住在台中"),
            "candidates with ids are offered: {user}"
        );
    }

    #[tokio::test]
    async fn a_correction_with_no_match_stores_a_new_memory_and_supersedes_nothing() {
        let (service, _) = seeded().await;
        let model = Scripted::replying(r#"{"replaces": null, "text": "住在台北"}"#);
        let outcome = correct_after_reply(&model, &service, "不對，我現在搬到台北住了", None).await;
        assert!(matches!(outcome, Some(SaveOutcome::Saved(_))));
        assert!(service.history().await.unwrap().is_empty());
        assert_eq!(service.list(None).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn an_id_outside_the_offered_list_is_ignored() {
        let (service, id) = seeded().await;
        let model = Scripted::replying(r#"{"replaces": "not-offered", "text": "住在台北"}"#);
        let outcome = correct_after_reply(&model, &service, "不對，我現在搬到台北住了", None).await;
        assert!(matches!(outcome, Some(SaveOutcome::Saved(_))));
        assert!(service.history().await.unwrap().is_empty());
        assert!(service.get(&id, Audience::Local).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn no_correction_cue_means_no_model_call() {
        let (service, _) = seeded().await;
        let model = Scripted::replying("{}");
        assert!(correct_after_reply(&model, &service, "今天天氣真好", None).await.is_none());
        assert_eq!(model.calls(), 0);
    }

    #[test]
    fn invalid_json_gives_no_candidates() {
        assert!(parse_candidates("sorry, nothing").is_empty());
        assert!(parse_candidates("[{\"text\": ").is_empty());
        assert!(parse_candidates("{\"text\": \"a\"}").is_empty());
    }

    #[test]
    fn valid_candidates_carry_text_kind_importance_and_tags() {
        let reply = r#"Here you go:
[{"text": "養了一隻叫麻糬的貓", "kind": "life_context", "importance": 3, "tags": ["貓", "cat"]},
 {"text": "", "kind": "other"}, {"kind": "other"}]"#;
        let candidates = parse_candidates(reply);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].kind, Kind::LifeContext);
        assert_eq!(candidates[0].importance, 3);
        assert_eq!(candidates[0].tags, vec!["貓", "cat"]);
    }

    #[test]
    fn a_candidate_about_a_sensitive_topic_is_discarded_even_if_the_model_returns_it() {
        let reply = r#"[{"text": "Takes antidepressant medication every day"}, {"text": "Has a cat named Mochi"},
                        {"text": "Likes tea", "tags": ["password"]}]"#;
        let candidates = parse_candidates(reply);
        assert_eq!(
            candidates.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["Has a cat named Mochi"]
        );
    }

    #[test]
    fn the_extraction_prompt_lists_the_excluded_categories() {
        let prompt = extraction_system();
        for category in [
            "health",
            "medication",
            "money or debt",
            "passwords",
            "sexuality",
            "religion",
            "politics",
        ] {
            assert!(prompt.contains(category), "{category}");
        }
    }

    #[tokio::test]
    async fn saved_candidates_are_auto_deduped_and_never_supersede() {
        let service = InMemoryMemory::new();
        let existing = service.add(NewMemory::told("Has a cat named Mochi")).await.unwrap();
        let candidates = parse_candidates(
            r#"[{"text": "has a cat named mochi"}, {"text": "Is a software engineer"}]"#,
        );
        let stored =
            save_candidates(&service, candidates, Some("session:1-4"), None).await.unwrap();
        assert_eq!(stored, 1);
        let all = service.list(None).await.unwrap();
        assert_eq!(all.len(), 2);
        let auto = all.iter().find(|m| m.text == "Is a software engineer").unwrap();
        assert_eq!(auto.origin, Origin::Auto);
        assert_eq!(auto.source.as_deref(), Some("session:1-4"));
        assert!(service.get(&existing.id, Audience::Local).await.unwrap().is_some());
        assert!(service.history().await.unwrap().is_empty());
    }
}
