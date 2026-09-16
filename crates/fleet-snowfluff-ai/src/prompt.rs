//! Prompt assembly: persona + a bounded recent-turn history window +
//! the new user message -> the final `Vec<Message>` every provider's
//! `build_request` consumes. Pure functions, no I/O, no network --
//! same testing philosophy as `fleet-snowfluff-core::config`.

use crate::{
    limits::CONTEXT_WINDOW_TURNS,
    message::Message,
    persona::{Language, Persona},
};

/// Returns only the most recent `max_turns` entries of `history`
/// (`ai-provider`'s "Bounded conversation context" requirement).
/// `Auto`-resolved language must already be a concrete [`Language`] by
/// the time it reaches here -- resolving `Auto` against the detected
/// system UI language is `fleet-snowfluff-core`'s job, done by the
/// caller before assembly, not this crate's.
pub fn cap_history(history: &[Message], max_turns: usize) -> &[Message] {
    if history.len() <= max_turns {
        history
    } else {
        &history[history.len() - max_turns..]
    }
}

fn language_name(language: Language) -> &'static str {
    match language {
        Language::ZhHant => "Traditional Chinese (繁體中文)",
        Language::ZhHans => "Simplified Chinese (简体中文)",
        Language::En => "English",
        Language::Ja => "Japanese (日本語)",
        Language::Ko => "Korean (한국어)",
    }
}

fn build_system_prompt(persona: &Persona, language: Language) -> String {
    let mut prompt =
        format!("You are {}.\n\n{}\n\n{}", persona.name, persona.personality, persona.speech_style);

    if !persona.emotional_core.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&persona.emotional_core);
    }

    if !persona.boundaries.is_empty() {
        prompt.push_str("\n\nBoundaries:\n");
        for boundary in &persona.boundaries {
            prompt.push_str("- ");
            prompt.push_str(boundary);
            prompt.push('\n');
        }
    }

    prompt.push_str("\nRespond only in ");
    prompt.push_str(language_name(language));
    prompt.push('.');
    prompt
}

/// Builds the final message list sent to a provider: a system prompt
/// derived from `persona`, that language's few-shot examples (never a
/// blend of languages, to avoid the "language leakage" problem the
/// persona file's own comments warn about for small local models), the
/// bounded recent-turn window from `history`, and finally the new user
/// message. `history` is assumed to already exclude failed turns
/// (`ai-chat`'s "Failure does not pollute future context" scenario) --
/// filtering that out is the caller's (app crate's) job when it reads
/// the session log, not this function's.
pub fn assemble_messages(
    persona: &Persona,
    language: Language,
    history: &[Message],
    user_message: &str,
) -> Vec<Message> {
    let mut messages = vec![Message::system(build_system_prompt(persona, language))];

    if let Some(examples) = persona.few_shot_examples.get(&language) {
        for example in examples {
            messages.push(Message::user(example.user.clone()));
            messages.push(Message::assistant(example.pet.clone()));
        }
    }

    messages.extend(cap_history(history, CONTEXT_WINDOW_TURNS).iter().cloned());
    messages.push(Message::user(user_message));
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persona::parse_persona;

    const PERSONA_YAML: &str = r#"
name: "Test"
response_language: "auto"
personality: "friendly"
speech_style: "short"
boundaries:
  - "no politics"
few_shot_examples:
  en:
    - user: "hi"
      pet: "hey there"
  ja:
    - user: "こんにちは"
      pet: "やあ"
"#;

    fn persona() -> Persona { parse_persona(PERSONA_YAML).unwrap() }

    #[test]
    fn cap_history_keeps_only_the_most_recent_turns() {
        let history: Vec<Message> = (0..50).map(|i| Message::user(format!("turn {i}"))).collect();
        let capped = cap_history(&history, 20);
        assert_eq!(capped.len(), 20);
        assert_eq!(capped.first().unwrap().content, "turn 30");
        assert_eq!(capped.last().unwrap().content, "turn 49");
    }

    #[test]
    fn cap_history_is_a_no_op_when_shorter_than_the_window() {
        let history = vec![Message::user("only one")];
        assert_eq!(cap_history(&history, 20).len(), 1);
    }

    #[test]
    fn assembled_messages_start_with_system_prompt_and_end_with_user_message() {
        let messages = assemble_messages(&persona(), Language::En, &[], "what's up");
        assert!(matches!(messages.first().unwrap().role, crate::message::Role::System));
        let last = messages.last().unwrap();
        assert!(matches!(last.role, crate::message::Role::User));
        assert_eq!(last.content, "what's up");
    }

    #[test]
    fn few_shot_examples_are_picked_for_the_requested_language_only() {
        let messages = assemble_messages(&persona(), Language::Ja, &[], "test");
        let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        assert!(contents.contains(&"こんにちは"), "Japanese examples must be included");
        assert!(
            !contents.contains(&"hi"),
            "English examples must not leak in for a Japanese request"
        );
    }

    #[test]
    fn full_session_history_longer_than_the_window_is_bounded_in_the_final_request() {
        let long_history: Vec<Message> =
            (0..(CONTEXT_WINDOW_TURNS * 3)).map(|i| Message::user(format!("turn {i}"))).collect();
        let messages = assemble_messages(&persona(), Language::En, &long_history, "final question");

        // system prompt (1) + english few-shot pair (2) + capped history + final user
        // message (1)
        let history_messages_included = messages.len() - 1 - 2 - 1;
        assert_eq!(history_messages_included, CONTEXT_WINDOW_TURNS);
    }
}
