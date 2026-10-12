//! Turning text into the pieces the keyword index is built from.
//!
//! There is no embedding model, so recall depends on how text is split.
//! SQLite's own tokenizers are not enough: the unicode tokenizer treats a run
//! of Chinese as one word, and the trigram tokenizer cannot match anything
//! under three characters while many Chinese words have two. So text is split
//! by script here, before SQLite sees it: runs of Han, kana, and Hangul become
//! overlapping two-character pieces (bigrams), Latin runs become lower-cased
//! words, and Traditional Chinese is folded to Simplified so a memory written
//! in one script is found by a query in the other. The original text is always
//! kept as written; only the index sees the folded form.
//!
//! Measured on the checked-in five-language fixtures (see `design.md`, "Spike
//! results"): the same scheme found the right memory for 20 of 20 English, 15
//! of 16 Traditional and Simplified Chinese, 13 of 13 Japanese, and 15 of 15
//! Korean ordinary queries, and folding raised cross-script recall from
//! 17 of 24 to 19 of 24.

use super::t2s_table::TRADITIONAL_TO_SIMPLIFIED;

/// Words too common to tell memories apart. Without this list English recall
/// fell from 22 of 22 to 11 of 22 on a deliberately crowded corpus.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "if", "of", "to", "in", "on", "at", "for", "with", "by",
    "from", "as", "is", "are", "was", "were", "be", "been", "am", "do", "does", "did", "you",
    "your", "i", "me", "my", "we", "our", "it", "its", "this", "that", "these", "those", "what",
    "which", "who", "how", "when", "where", "why", "can", "could", "should", "would", "will",
    "shall", "may", "might", "not", "no", "so", "than", "then", "too", "very", "just", "about",
    "into", "out", "up", "down", "over", "under", "again", "here", "there", "some", "any", "each",
    "more", "most", "other", "such", "only", "own", "same", "also",
];

/// Han, kana, and Hangul: the scripts split into bigrams rather than words.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF   // Han
        | 0x3040..=0x30FF | 0x31F0..=0x31FF                    // hiragana, katakana
        | 0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F  // Hangul
    )
}

fn to_simplified(c: char) -> char {
    TRADITIONAL_TO_SIMPLIFIED
        .binary_search_by_key(&c, |&(traditional, _)| traditional)
        .map_or(c, |index| TRADITIONAL_TO_SIMPLIFIED[index].1)
}

/// Folds full-width ASCII forms to plain ASCII, lower-cases, and maps
/// Traditional Chinese characters to Simplified. The form the index and every
/// query use.
pub fn normalize(text: &str) -> String {
    text.chars()
        .map(|c| match c as u32 {
            0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            0x3000 => ' ',
            _ => c,
        })
        .flat_map(char::to_lowercase)
        .map(to_simplified)
        .collect()
}

/// The index tokens for `text`: bigrams over each CJK run, words for Latin
/// runs. A CJK run of one character yields no token (see [`single_cjk_chars`]).
pub fn tokens(text: &str) -> Vec<String> {
    let normalized = normalize(text);
    let chars: Vec<char> = normalized.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_cjk(c) {
            let mut j = i;
            while j < chars.len() && is_cjk(chars[j]) {
                j += 1;
            }
            for pair in chars[i..j].windows(2) {
                out.push(pair.iter().collect());
            }
            i = j;
        } else if c.is_alphanumeric() {
            let mut j = i;
            while j < chars.len() && chars[j].is_alphanumeric() && !is_cjk(chars[j]) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if !STOPWORDS.contains(&word.as_str()) {
                out.push(word);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// The CJK characters that stand alone as a run of exactly one (a lone 猫 in a
/// query). They produce no bigram, so they are matched by scanning the
/// normalized text instead.
pub fn single_cjk_chars(text: &str) -> Vec<char> {
    let normalized = normalize(text);
    let chars: Vec<char> = normalized.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if is_cjk(chars[i]) {
            let mut j = i;
            while j < chars.len() && is_cjk(chars[j]) {
                j += 1;
            }
            if j - i == 1 {
                out.push(chars[i]);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// The text stored in the keyword index for a memory: its tokens, then its
/// tags' tokens, space separated so SQLite's plain tokenizer keeps each piece
/// whole.
pub fn index_text(text: &str, tags: &[String]) -> String {
    let mut pieces = tokens(text);
    for tag in tags {
        pieces.extend(tokens(tag));
    }
    pieces.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_character_chinese_word_is_a_token_of_its_own() {
        assert!(tokens("明天有考試").contains(&"考试".to_string()));
        assert_eq!(tokens("考試"), vec!["考试".to_string()]);
    }

    #[test]
    fn traditional_and_simplified_text_produce_the_same_tokens() {
        assert_eq!(tokens("學習日文"), tokens("学习日文"));
        assert_eq!(normalize("學習"), "学习");
    }

    #[test]
    fn a_latin_word_inside_chinese_text_is_its_own_token() {
        assert_eq!(
            tokens("我在用 notion"),
            vec!["我在".to_string(), "在用".to_string(), "notion".to_string()]
        );
    }

    #[test]
    fn japanese_kana_and_kanji_are_split_into_bigrams() {
        let t = tokens("今日は緑茶");
        for expected in ["今日", "日は", "は绿", "绿茶"] {
            assert!(t.contains(&expected.to_string()), "missing {expected} in {t:?}");
        }
    }

    #[test]
    fn korean_hangul_is_split_into_bigrams() {
        assert_eq!(tokens("고양이"), vec!["고양".to_string(), "양이".to_string()]);
    }

    #[test]
    fn punctuation_ends_a_run_so_no_bigram_spans_it() {
        let t = tokens("喝茶，好累");
        assert!(t.contains(&"喝茶".to_string()) && t.contains(&"好累".to_string()));
        assert!(!t.contains(&"茶好".to_string()));
    }

    #[test]
    fn a_lone_chinese_character_is_reported_for_the_substring_fallback() {
        assert_eq!(single_cjk_chars("猫"), vec!['猫']);
        assert_eq!(single_cjk_chars("喝茶，猫"), vec!['猫']);
        assert!(
            single_cjk_chars("考試").is_empty(),
            "a two-character run is covered by its bigram"
        );
        assert!(tokens("猫").is_empty());
    }

    #[test]
    fn english_stopwords_are_dropped_and_words_lower_cased() {
        assert_eq!(
            tokens("What is The Weather TODAY"),
            vec!["weather".to_string(), "today".to_string()]
        );
    }

    #[test]
    fn full_width_latin_and_digits_fold_to_plain_ascii() {
        assert_eq!(
            tokens("ＭａｃＢｏｏｋ　２０２５"),
            vec!["macbook".to_string(), "2025".to_string()]
        );
    }

    #[test]
    fn non_chinese_text_is_not_altered_by_the_folding() {
        assert_eq!(normalize("Hello 世界 안녕"), "hello 世界 안녕");
    }

    #[test]
    fn folding_a_simplified_text_changes_nothing() {
        assert_eq!(normalize("学习日语"), "学习日语");
    }

    #[test]
    fn index_text_joins_the_text_tokens_and_the_tags_tokens() {
        let indexed = index_text("喜歡喝綠茶", &["green tea".to_string(), "tea".to_string()]);
        assert!(indexed.contains("喜欢"));
        assert!(indexed.contains("green"));
        assert!(indexed.ends_with("tea"));
    }

    #[test]
    fn empty_and_punctuation_only_text_has_no_tokens() {
        assert!(tokens("").is_empty());
        assert!(tokens("！？，。").is_empty());
    }
}
