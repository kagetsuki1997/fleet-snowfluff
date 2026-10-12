//! A deterministic screen for topics the assistant must not keep on its own
//! initiative: health and medication, money and debt, credentials, sexuality,
//! religion, and politics.
//!
//! The extraction and summary prompts both tell the model to leave these out,
//! but a measured run showed it cannot be relied on to: a medication fact
//! slipped into one extraction in fifteen, and every one of five summaries kept
//! it despite an explicit instruction (`design.md`, "Spike results"). So the
//! output of both is also passed through this check. It is a keyword screen,
//! not a classifier: it will occasionally drop a harmless sentence (a dentist
//! visit counts as health) and it cannot catch a euphemism, which is the right
//! trade for something that decides what is kept about a person. Text the user
//! explicitly asks to be remembered is not subject to it.

/// Substrings that mark a sentence as sensitive in any script. Matched
/// case-insensitively.
const SUBSTRINGS: &[&str] = &[
    // --- English: health and medication
    "medicat",
    "prescri",
    "diagnos",
    "therap",
    "anxiety",
    "depress",
    "psychiatr",
    "mental health",
    "panic attack",
    "surgery",
    "cancer",
    "diabet",
    "chronic",
    "illness",
    "disease",
    "symptom",
    "allerg",
    "hospital",
    "doctor",
    "dentist",
    "insomnia",
    "disorder",
    "antibiotic",
    // --- English: money and debt
    "debt",
    "loan",
    "mortgage",
    "salary",
    "income",
    "bankrupt",
    "credit card",
    "bank account",
    "paycheck",
    "savings",
    // --- English: credentials
    "password",
    "passcode",
    "api key",
    "secret key",
    "pin code",
    "social security",
    // --- English: sexuality, religion, politics
    "sexual",
    "lesbian",
    "bisexual",
    "transgender",
    "religio",
    "church",
    "mosque",
    "christian",
    "muslim",
    "buddhis",
    "atheis",
    "politic",
    "election",
    "democrat",
    "republican",
    // --- Traditional Chinese
    "吃藥",
    "服藥",
    "藥物",
    "焦慮",
    "憂鬱",
    "抑鬱",
    "躁鬱",
    "失眠",
    "診斷",
    "治療",
    "醫院",
    "手術",
    "癌",
    "糖尿",
    "疾病",
    "病症",
    "過敏",
    "心理諮商",
    "精神科",
    "身心科",
    "看醫生",
    "牙醫",
    "債",
    "貸款",
    "房貸",
    "薪水",
    "薪資",
    "年薪",
    "存款",
    "破產",
    "信用卡",
    "欠錢",
    "密碼",
    "金鑰",
    "私鑰",
    "性取向",
    "同性戀",
    "雙性戀",
    "跨性別",
    "宗教",
    "教會",
    "基督",
    "佛教",
    "穆斯林",
    "祈禱",
    "政治",
    "選舉",
    "投票",
    "民進黨",
    "國民黨",
    "共產黨",
    // --- Simplified Chinese
    "吃药",
    "服药",
    "药物",
    "焦虑",
    "抑郁",
    "失眠",
    "诊断",
    "治疗",
    "医院",
    "手术",
    "病症",
    "过敏",
    "心理咨询",
    "看医生",
    "牙医",
    "债",
    "贷款",
    "房贷",
    "工资",
    "存款",
    "破产",
    "欠钱",
    "密码",
    "密钥",
    "同性恋",
    "双性恋",
    "跨性别",
    "教会",
    "佛教",
    "祈祷",
    "选举",
    "民进党",
    "共产党",
    // --- Japanese
    "薬",
    "服薬",
    "不安症",
    "うつ病",
    "うつ状態",
    "鬱",
    "不眠",
    "診断",
    "治療",
    "病院",
    "手術",
    "がん",
    "糖尿病",
    "病気",
    "アレルギー",
    "カウンセリング",
    "精神科",
    "心療内科",
    "歯医者",
    "借金",
    "ローン",
    "給料",
    "年収",
    "貯金",
    "破産",
    "クレジットカード",
    "パスワード",
    "暗証番号",
    "同性愛",
    "性的指向",
    "宗教",
    "教会",
    "キリスト",
    "仏教",
    "イスラム",
    "祈り",
    "政治",
    "選挙",
    "投票",
    "自民党",
    // --- Korean
    "약을",
    "약 먹",
    "복용",
    "불안증",
    "우울",
    "조울",
    "불면",
    "진단",
    "치료",
    "병원",
    "수술",
    "당뇨",
    "질병",
    "알레르기",
    "정신과",
    "치과",
    "빚",
    "대출",
    "주택담보",
    "월급",
    "연봉",
    "파산",
    "신용카드",
    "비밀번호",
    "암호",
    "성적 지향",
    "동성애",
    "종교",
    "교회",
    "기독교",
    "불교",
    "이슬람",
    "기도",
    "정치",
    "선거",
    "투표",
];

/// Short Latin words that are only sensitive as whole words (a substring check
/// would flag "pillow", "vote" inside other words, and so on).
const WORDS: &[&str] = &[
    "pill", "pills", "gay", "vote", "voting", "voted", "tax", "taxes", "pin", "ssn", "pray",
    "prayer", "god", "bible", "quran",
];

fn contains_word(lowercase: &str, word: &str) -> bool {
    lowercase.split(|c: char| !c.is_alphanumeric()).any(|token| token == word)
}

/// Whether `text` touches a topic the assistant must not keep on its own
/// initiative.
pub fn is_sensitive(text: &str) -> bool {
    let lowercase = text.to_lowercase();
    SUBSTRINGS.iter().any(|term| lowercase.contains(term))
        || WORDS.iter().any(|word| contains_word(&lowercase, word))
}

/// Splits `text` into sentences, keeping each one's own terminator. A full stop
/// only ends a sentence when whitespace or the end of the text follows it, so
/// "3.5 hours" stays whole.
fn sentences(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (position, &(index, c)) in chars.iter().enumerate() {
        let ends = match c {
            '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n' | '．' => true,
            '.' => chars.get(position + 1).is_none_or(|&(_, next)| next.is_whitespace()),
            _ => false,
        };
        if ends {
            let end = index + c.len_utf8();
            parts.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

/// `text` with every sentence that touches a sensitive topic removed. The other
/// sentences are kept exactly as they were.
pub fn remove_sensitive_sentences(text: &str) -> String {
    let kept: String = sentences(text)
        .into_iter()
        .filter(|sentence| !is_sensitive(sentence))
        .collect::<Vec<_>>()
        .concat();
    kept.trim().to_string()
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct Fixture {
        language: String,
        memories: Vec<Memory>,
    }

    #[derive(Deserialize)]
    struct Memory {
        id: String,
        text: String,
    }

    fn fixture(json: &str) -> Fixture { serde_json::from_str(json).expect("fixture parses") }

    #[test]
    fn medication_and_anxiety_are_caught_in_every_supported_language() {
        let sensitive = [
            "The user has been taking medication for their anxiety.",
            "用戶提及正在接受焦慮治療，睡眠品質不佳。",
            "用户透露近期因焦虑情绪正在服药。",
            "最近不安症の薬を飲んでいて睡眠に影響が出ている。",
            "불안증 약을 복용하며 잠을 잘 못 자고 있다.",
        ];
        for sentence in sensitive {
            assert!(is_sensitive(sentence), "should be sensitive: {sentence}");
        }
    }

    #[test]
    fn the_other_sensitive_categories_are_caught() {
        let sensitive = [
            "They are paying off a large debt and a mortgage.",
            "My password is hunter2.",
            "He is planning to vote in the election.",
            "債務が多くて困っている。",
            "비밀번호를 바꿨다.",
            "信用卡還款日快到了。",
            "他是虔誠的基督徒。",
        ];
        for sentence in sensitive {
            assert!(is_sensitive(sentence), "should be sensitive: {sentence}");
        }
    }

    #[test]
    fn ordinary_sentences_are_not_flagged() {
        let ordinary = [
            "The user has a cat named Mochi.",
            "Works as a software engineer and has been working overtime.",
            "Planning a trip to Kyoto next month.",
            "用戶家中有貓，名為麻糬，近日食慾不佳。",
            "下个月计划前往京都旅行。",
            "猫のモチがごはんを食べない。",
            "다음 달 교토 여행을 계획 중이다.",
            "약속이 있어서 일찍 나갔다.",
            "I bought a pillow and a new keyboard.",
            "Syntax errors are annoying and the taxi was late.",
        ];
        for sentence in ordinary {
            assert!(!is_sensitive(sentence), "should not be sensitive: {sentence}");
        }
    }

    #[test]
    fn short_words_match_only_as_whole_words() {
        assert!(is_sensitive("take the pill every morning"));
        assert!(!is_sensitive("a soft pillow"));
        assert!(!is_sensitive("a taxi ride"));
        assert!(is_sensitive("file the taxes soon"));
    }

    #[test]
    fn a_sentence_about_medication_is_removed_and_its_neighbors_are_kept() {
        let summary = "The user has a cat named Mochi. They recently started taking medication \
                       for anxiety. They are a software engineer.";
        let cleaned = remove_sensitive_sentences(summary);
        assert!(!cleaned.contains("medication"));
        assert!(cleaned.contains("Mochi"));
        assert!(cleaned.contains("software engineer"));
    }

    #[test]
    fn chinese_sentences_split_on_ideographic_punctuation() {
        let summary =
            "用戶家中有貓，名為麻糬。用戶提及正在接受焦慮治療，睡眠品質不佳。用戶為軟體工程師。";
        let cleaned = remove_sensitive_sentences(summary);
        assert!(!cleaned.contains("焦慮"));
        assert!(cleaned.contains("麻糬"));
        assert!(cleaned.contains("軟體工程師"));
    }

    #[test]
    fn decimal_points_do_not_split_a_sentence() {
        assert_eq!(
            remove_sensitive_sentences("Runs 3.5 kilometers every morning."),
            "Runs 3.5 kilometers every morning."
        );
    }

    #[test]
    fn text_with_nothing_sensitive_is_returned_unchanged() {
        let text = "Likes green tea. Has a cat.";
        assert_eq!(remove_sensitive_sentences(text), text);
    }

    #[test]
    fn a_text_that_is_entirely_sensitive_becomes_empty() {
        assert_eq!(remove_sensitive_sentences("Takes medication daily."), "");
    }

    #[test]
    fn ordinary_fixture_memories_are_flagged_only_when_they_are_about_health() {
        // The two health-adjacent memories in every language (a peanut allergy and a
        // dentist appointment) are the only ones the screen should drop.
        let files = [
            include_str!("../tests/fixtures/memory/recall_en.json"),
            include_str!("../tests/fixtures/memory/recall_zh-Hant.json"),
            include_str!("../tests/fixtures/memory/recall_zh-Hans.json"),
            include_str!("../tests/fixtures/memory/recall_ja.json"),
            include_str!("../tests/fixtures/memory/recall_ko.json"),
        ];
        for json in files {
            let fixture = fixture(json);
            let flagged: Vec<&str> = fixture
                .memories
                .iter()
                .filter(|memory| is_sensitive(&memory.text))
                .map(|memory| memory.id.as_str())
                .collect();
            assert_eq!(
                flagged,
                vec!["m5", "m27"],
                "{}: unexpected flags {flagged:?}",
                fixture.language
            );
        }
    }
}
