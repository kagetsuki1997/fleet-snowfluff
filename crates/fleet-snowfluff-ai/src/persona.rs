//! Persona file format and load/fallback behavior
//! (`ai-persona`'s "Bundled default persona" and "Graceful fallback on
//! parse failure"). Parsing is deliberately all-or-nothing -- unlike
//! `fleet-snowfluff-core::config`'s field-by-field sanitization, a
//! persona file either parses cleanly or the whole file is treated as
//! broken and the bundled default is used instead, with the failure
//! reason surfaced to the caller so it can show a warning
//! (`ai-persona`'s parse-failure scenario).

use std::collections::HashMap;

use serde::Deserialize;

/// The bundled default persona's YAML source, embedded at compile
/// time -- same `include_str!` pattern as
/// `fleet-snowfluff-core::i18n`'s locale dictionaries. Path is relative
/// to this file: `src/` -> crate root -> `crates/` -> repo root.
pub const BUNDLED_DEFAULT_PERSONA_YAML: &str = include_str!("../../../personas/aemeath.yaml");

/// A concrete response language -- distinct from `ResponseLanguage`,
/// which also has an `Auto` variant. `assemble_messages` (see
/// `prompt.rs`) always takes one of these, since resolving `Auto` to a
/// concrete language depends on the detected system UI language, which
/// is `fleet-snowfluff-core`'s concern, not this crate's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub enum Language {
    #[serde(rename = "zh-Hant")]
    ZhHant,
    #[serde(rename = "zh-Hans")]
    ZhHans,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "ja")]
    Ja,
    #[serde(rename = "ko")]
    Ko,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ResponseLanguage {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "zh-Hant")]
    ZhHant,
    #[serde(rename = "zh-Hans")]
    ZhHans,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "ja")]
    Ja,
    #[serde(rename = "ko")]
    Ko,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FewShotExample {
    pub user: String,
    pub pet: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Persona {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub response_language: ResponseLanguage,
    pub personality: String,
    pub speech_style: String,
    #[serde(default)]
    pub emotional_core: String,
    #[serde(default)]
    pub boundaries: Vec<String>,
    #[serde(default)]
    pub few_shot_examples: HashMap<Language, Vec<FewShotExample>>,
    #[serde(default)]
    pub emotion_tags: Vec<String>,
}

/// A persona file that failed to parse -- carries the underlying parser
/// message so it can be shown verbatim in the AI settings tab
/// (`ai-persona`'s "Malformed edit" scenario).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaParseError(pub String);

impl std::fmt::Display for PersonaParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

impl std::error::Error for PersonaParseError {}

pub fn parse_persona(yaml: &str) -> Result<Persona, PersonaParseError> {
    serde_saphyr::from_str(yaml).map_err(|e| PersonaParseError(e.to_string()))
}

/// The outcome of loading a persona: the persona to actually use, plus
/// a warning if the user's own file existed but failed to parse (in
/// which case `persona` is the bundled default, not `None`/empty --
/// chat must keep working per `ai-persona`'s "SHALL NOT crash or
/// disable chat").
pub struct PersonaLoadResult {
    pub persona: Persona,
    pub warning: Option<PersonaParseError>,
}

/// Pure version of persona loading, taking the bundled default's text
/// explicitly rather than reaching for the `include_str!` constant, so
/// tests aren't coupled to the real file's current content.
///
/// `user_yaml` is `None` when no user persona file exists yet
/// (`ai-persona`'s "No user persona file present" scenario); `Some` for
/// both the malformed and the successfully-parsed cases.
pub fn load_persona(user_yaml: Option<&str>, bundled_default_yaml: &str) -> PersonaLoadResult {
    let bundled_default =
        || parse_persona(bundled_default_yaml).expect("bundled default persona must always parse");

    match user_yaml {
        None => PersonaLoadResult { persona: bundled_default(), warning: None },
        Some(text) => match parse_persona(text) {
            Ok(persona) => PersonaLoadResult { persona, warning: None },
            Err(err) => PersonaLoadResult { persona: bundled_default(), warning: Some(err) },
        },
    }
}

/// Convenience wrapper over [`load_persona`] using the compiled-in
/// bundled default.
pub fn load_persona_or_bundled_default(user_yaml: Option<&str>) -> PersonaLoadResult {
    load_persona(user_yaml, BUNDLED_DEFAULT_PERSONA_YAML)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_MINIMAL: &str = r#"
name: "Test"
response_language: "auto"
personality: "friendly"
speech_style: "short"
few_shot_examples:
  en:
    - user: "hi"
      pet: "hi there"
"#;

    #[test]
    fn bundled_default_persona_parses() {
        parse_persona(BUNDLED_DEFAULT_PERSONA_YAML)
            .expect("the committed personas/aemeath.yaml must always be valid");
    }

    #[test]
    fn valid_minimal_persona_parses() {
        let persona = parse_persona(VALID_MINIMAL).unwrap();
        assert_eq!(persona.name, "Test");
        assert_eq!(persona.response_language, ResponseLanguage::Auto);
        assert!(persona.few_shot_examples.contains_key(&Language::En));
    }

    #[test]
    fn no_user_file_uses_bundled_default_with_no_warning() {
        let result = load_persona(None, VALID_MINIMAL);
        assert_eq!(result.persona.name, "Test");
        assert!(result.warning.is_none());
    }

    #[test]
    fn malformed_user_file_falls_back_to_default_with_warning() {
        let result = load_persona(Some("not: valid: yaml: : :"), VALID_MINIMAL);
        assert_eq!(result.persona.name, "Test", "must fall back to the bundled default");
        assert!(result.warning.is_some(), "the parse failure must be surfaced");
    }

    #[test]
    fn corrected_user_file_recovers_with_no_warning() {
        let corrected = r#"
name: "User's Persona"
response_language: "en"
personality: "custom"
speech_style: "custom"
"#;
        let result = load_persona(Some(corrected), VALID_MINIMAL);
        assert_eq!(result.persona.name, "User's Persona");
        assert!(result.warning.is_none(), "a valid file must not carry a stale warning");
    }
}
