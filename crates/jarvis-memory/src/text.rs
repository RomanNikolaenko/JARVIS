//! Text helpers: token estimation, FTS5 query building, term overlap and
//! truncation.

use std::collections::HashSet;

/// Tokens a chat message adds besides its text (role markers, separators).
pub const MESSAGE_OVERHEAD_TOKENS: u32 = 4;

/// Rough token count without a tokenizer: about 4 bytes of UTF-8 per token
/// for English prose, at least one token per ~3 characters (code, numbers).
/// Cyrillic is 2 bytes per letter, so it is slightly overestimated, which is
/// the safe direction for context budgets.
pub fn estimate_tokens(text: &str) -> u32 {
    let bytes = text.len();
    let chars = text.chars().count();
    let estimate = bytes.div_ceil(4).max((chars * 10).div_ceil(32));
    u32::try_from(estimate).unwrap_or(u32::MAX)
}

/// Cuts `text` to about `max_tokens`, at a character boundary, adding "…".
pub fn truncate_to_tokens(text: &str, max_tokens: u32) -> String {
    if estimate_tokens(text) <= max_tokens {
        return text.to_owned();
    }
    // Shrink until it fits; starts from a generous guess.
    let mut chars: usize = (max_tokens as usize).saturating_mul(3);
    loop {
        let cut: String = text.chars().take(chars).collect();
        if chars == 0 || estimate_tokens(&cut) < max_tokens {
            return format!("{}…", cut.trim_end());
        }
        chars = chars * 9 / 10;
    }
}

/// Cuts `text` to `max_chars` characters, adding "…".
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max_chars).collect();
    format!("{}…", cut.trim_end())
}

/// Very common words that make full-text matches meaningless.
const STOPWORDS: &[&str] = &[
    // English
    "the",
    "and",
    "for",
    "are",
    "you",
    "your",
    "what",
    "how",
    "this",
    "that",
    "with",
    "was",
    "were",
    "is",
    "to",
    "of",
    "in",
    "on",
    "it",
    "an",
    "me",
    "my",
    "we",
    "be",
    "do",
    "does",
    "can",
    "about",
    "from",
    "have",
    "has",
    "will",
    "would",
    "please",
    "there",
    "they",
    "them",
    "which",
    "when",
    "where",
    "who",
    "why",
    "not",
    "but",
    "or",
    "if",
    "at",
    "by",
    "as",
    "so",
    "no",
    "yes",
    // Ukrainian
    "та",
    "що",
    "як",
    "це",
    "на",
    "до",
    "не",
    "ти",
    "ми",
    "ви",
    "він",
    "вона",
    "воно",
    "вони",
    "мені",
    "мене",
    "для",
    "про",
    "чи",
    "але",
    "так",
    "ні",
    "бо",
    "яка",
    "який",
    "яке",
    "які",
    "де",
    "коли",
    "там",
    "тут",
    "вже",
    "ще",
    "же",
    "або",
    "його",
    "її",
    "їх",
    "мій",
    "моя",
    "моє",
    "мої",
    "твій",
    "твоя",
    "цей",
    "ця",
    "ці",
    "той",
    "та",
    "те",
    "ті",
    "бути",
    "є",
    "буде",
    "будь",
    "ласка",
    "можеш",
    "можна",
    "треба",
    "дуже",
    "від",
    "за",
    "по",
    "при",
    "без",
    "під",
    "над",
    "із",
    "зі",
    "аби",
    "щоб",
    // Russian
    "что",
    "как",
    "это",
    "на",
    "не",
    "для",
    "про",
    "или",
    "его",
    "она",
    "они",
    "мне",
    "меня",
    "так",
    "уже",
    "еще",
    "где",
    "когда",
    "там",
    "тут",
    "был",
    "была",
    "было",
    "быть",
];

/// Normalized search terms of `text`: lowercase alphanumeric words of two or
/// more characters, without stop words, deduplicated, at most `limit`.
pub fn search_terms(text: &str, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut terms = Vec::new();
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 2)
    {
        if STOPWORDS.contains(&word.as_str()) || !seen.insert(word.clone()) {
            continue;
        }
        terms.push(word);
        if terms.len() == limit {
            break;
        }
    }
    terms
}

/// Crude stem for prefix matching of inflected words: words of seven or
/// more characters lose two trailing characters, six-character words one, so
/// "проєктом", "проєкту" and "проєкт" share the prefix "проєк".
pub fn stem(term: &str) -> String {
    let chars: Vec<char> = term.chars().collect();
    let keep = match chars.len() {
        n if n >= 7 => n - 2,
        6 => 5,
        n => n,
    };
    chars[..keep].iter().collect()
}

/// Shorter terms match whole words only ("hi" must not match "history").
const MIN_PREFIX_CHARS: usize = 4;

/// FTS5 `MATCH` expression for free text: stemmed terms as quoted prefix
/// queries joined with `OR`. `None` when nothing searchable remains. Terms
/// contain only alphanumeric characters, so user input cannot inject FTS5
/// syntax.
pub fn match_query(text: &str) -> Option<String> {
    let terms = search_terms(text, 12);
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|t| {
                let stem = stem(t);
                if stem.chars().count() >= MIN_PREFIX_CHARS {
                    format!("\"{stem}\"*")
                } else {
                    format!("\"{stem}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// Share of the query's terms that occur (as stems) in `text`, 0.0..=1.0.
pub fn term_overlap(query_terms: &[String], text: &str) -> f32 {
    if query_terms.is_empty() {
        return 0.0;
    }
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .collect();
    let found = query_terms
        .iter()
        .filter(|term| {
            let stem = stem(term);
            if stem.chars().count() >= MIN_PREFIX_CHARS {
                words.iter().any(|w| w.starts_with(&stem))
            } else {
                words.contains(&stem)
            }
        })
        .count();
    found as f32 / query_terms.len() as f32
}

/// Removes `<think>…</think>` blocks (the reasoning of thinking models) so
/// it is not sent back to a model. An unclosed block is cut to the end.
pub fn strip_reasoning(text: &str) -> std::borrow::Cow<'_, str> {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    if !text.contains(OPEN) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(CLOSE) {
            Some(end) => rest = &rest[start + end + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out.trim().to_owned())
}

/// `YYYY-MM-DD` (UTC) of a Unix timestamp in milliseconds.
pub fn format_date(ms: i64) -> String {
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_blocks_are_removed() {
        assert_eq!(strip_reasoning("plain"), "plain");
        assert_eq!(strip_reasoning("<think>hmm</think>\nAnswer"), "Answer");
        assert_eq!(strip_reasoning("A <think>x</think>B<think>y"), "A B");
    }

    #[test]
    fn dates_are_formatted_in_utc() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(1_700_000_000_000), "2023-11-14");
        assert_eq!(format_date(951_782_400_000), "2000-02-29");
    }

    #[test]
    fn token_estimates_are_conservative() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("Hello world, how are you?"), 8);
        // 29 Cyrillic characters: real tokenizers use ~10-14 tokens.
        assert!(estimate_tokens("Привіт, як справи з проєктом?") >= 13);
        // Code is denser than prose.
        assert!(estimate_tokens("fn main() { println!(\"hi\"); }") >= 10);
    }

    #[test]
    fn truncation_respects_the_budget() {
        let text = "слово ".repeat(500);
        let cut = truncate_to_tokens(&text, 50);
        assert!(estimate_tokens(&cut) <= 51, "{}", estimate_tokens(&cut));
        assert!(cut.ends_with('…'));
        assert_eq!(truncate_to_tokens("short", 50), "short");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
    }

    #[test]
    fn match_queries_are_safe_and_stemmed() {
        assert_eq!(match_query("the of and"), None);
        assert_eq!(
            match_query("Розкажи про проєкт Атлас"),
            Some("\"розка\"* OR \"проєк\"* OR \"атлас\"*".to_owned())
        );
        // FTS5 operators and quotes in user input become plain terms.
        let query = match_query("NEAR(\"x\" y) * OR AND -z col:value").expect("terms");
        assert!(!query.contains("NEAR("));
        assert!(!query.contains(':'));
        assert_eq!(query.matches('"').count() % 2, 0);
    }

    #[test]
    fn overlap_counts_stemmed_terms() {
        let terms = search_terms("проєкт Атлас дедлайн", 12);
        assert_eq!(terms.len(), 3);
        let text = "Дедлайн проєкту Атлас перенесли на п'ятницю";
        assert!((term_overlap(&terms, text) - 1.0).abs() < f32::EPSILON);
        assert!(term_overlap(&terms, "зовсім інша тема") < 0.01);
        let short = search_terms("hi", 12);
        assert!(term_overlap(&short, "history of HI") > 0.99);
        assert!(term_overlap(&short, "history") < 0.01);
        assert_eq!(match_query("hi").as_deref(), Some("\"hi\""));
    }
}
