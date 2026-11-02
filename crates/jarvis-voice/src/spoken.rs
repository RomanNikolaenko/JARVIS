//! Text for Ukrainian voices.
//!
//! The Ukrainian Piper voice reads Cyrillic letters and a little punctuation
//! only; Piper drops every other character, so digits, symbols and
//! Latin-script words vanish from the speech. [`for_ukrainian_voice`] spells
//! them out in Cyrillic first.

mod latin;

/// Digit runs longer than this are read digit by digit (phone numbers, IDs).
const MAX_NUMBER_DIGITS: usize = 9;

const UNITS: [&str; 10] = [
    "нуль",
    "один",
    "два",
    "три",
    "чотири",
    "п'ять",
    "шість",
    "сім",
    "вісім",
    "дев'ять",
];
const TEENS: [&str; 10] = [
    "десять",
    "одинадцять",
    "дванадцять",
    "тринадцять",
    "чотирнадцять",
    "п'ятнадцять",
    "шістнадцять",
    "сімнадцять",
    "вісімнадцять",
    "дев'ятнадцять",
];
const TENS: [&str; 10] = [
    "",
    "",
    "двадцять",
    "тридцять",
    "сорок",
    "п'ятдесят",
    "шістдесят",
    "сімдесят",
    "вісімдесят",
    "дев'яносто",
];
const HUNDREDS: [&str; 10] = [
    "",
    "сто",
    "двісті",
    "триста",
    "чотириста",
    "п'ятсот",
    "шістсот",
    "сімсот",
    "вісімсот",
    "дев'ятсот",
];
/// Scale words by power of a thousand: forms for 1, 2–4 and 5+, and whether
/// the noun is feminine (одна тисяча, дві тисячі).
const SCALES: [(&str, &str, &str, bool); 4] = [
    ("тисяча", "тисячі", "тисяч", true),
    ("мільйон", "мільйони", "мільйонів", false),
    ("мільярд", "мільярди", "мільярдів", false),
    ("трильйон", "трильйони", "трильйонів", false),
];

/// Rewrites `text` so a Ukrainian voice can read all of it.
pub fn for_ukrainian_voice(text: &str) -> String {
    let chars: Vec<char> = text
        .chars()
        .map(|c| match c {
            '’' | 'ʼ' => '\'',
            // The typographic minus.
            '−' => '-',
            _ => c,
        })
        .collect();
    let mut out = String::with_capacity(text.len() * 2);
    let mut currency = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let starts_number = c.is_ascii_digit()
            || (c == '-'
                && chars.get(i + 1).is_some_and(char::is_ascii_digit)
                && (i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '('));
        if starts_number {
            let (words, next) = read_number(&chars, i, currency);
            currency = false;
            push_word(&mut out, &words);
            i = next;
            continue;
        }
        // A Latin letter typed inside a Ukrainian word (кiлька, Kиїв).
        let near_cyrillic = |j: usize| chars.get(j).is_some_and(|n| is_cyrillic(*n));
        if let Some(cyrillic) = latin::look_alike(c)
            && ((i > 0 && near_cyrillic(i - 1)) || near_cyrillic(i + 1))
        {
            out.push(cyrillic);
            i += 1;
            continue;
        }
        if latin::is_latin(c) {
            // Letters with inner hyphens, apostrophes and dots: USB-C, Wi-Fi,
            // don't, setup.exe.
            let mut end = i + 1;
            while end < chars.len() {
                let joins = matches!(chars[end], '-' | '\'' | '.')
                    && chars.get(end + 1).is_some_and(|&n| latin::is_latin(n));
                if latin::is_latin(chars[end]) || joins {
                    end += 1;
                } else {
                    break;
                }
            }
            let token: String = chars[i..end].iter().collect();
            push_word(&mut out, &latin::latin_word(&token));
            i = end;
            continue;
        }
        // A hyphen between a word and a number (GPT-4) is not read.
        if c == '-'
            && chars.get(i + 1).is_some_and(char::is_ascii_digit)
            && i > 0
            && chars[i - 1].is_alphabetic()
        {
            out.push(' ');
            i += 1;
            continue;
        }
        match c {
            '$' if chars.get(i + 1).is_some_and(char::is_ascii_digit) => currency = true,
            '+' => push_word(&mut out, "плюс"),
            '=' => push_word(&mut out, "дорівнює"),
            '&' => push_word(&mut out, "і"),
            '@' => push_word(&mut out, "ет"),
            '°' => {
                push_word(&mut out, "градусів");
                if let Some(scale) = temperature_scale(chars.get(i + 1)) {
                    push_word(&mut out, scale);
                    i += 1;
                }
            }
            '#' | '№' => push_word(&mut out, "номер"),
            '%' => push_word(&mut out, "відсотків"),
            '/' | '_' => out.push(' '),
            _ => out.push(c),
        }
        i += 1;
    }
    tidy(&out)
}

/// «за Цельсієм» after "°C", «за Фаренгейтом» after "°F".
fn temperature_scale(letter: Option<&char>) -> Option<&'static str> {
    match letter {
        Some('C' | 'С') => Some("за Цельсієм"),
        Some('F') => Some("за Фаренгейтом"),
        _ => None,
    }
}

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04ff}').contains(&c)
}

/// Adds a spoken word with spaces around it.
fn push_word(out: &mut String, word: &str) {
    out.push(' ');
    out.push_str(word);
    out.push(' ');
}

/// Collapses spaces and removes those before punctuation and inside
/// brackets.
fn tidy(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(joined.len());
    for c in joined.chars() {
        if c == ' ' && (out.ends_with('(') || out.ends_with('[')) {
            continue;
        }
        if matches!(c, ',' | '.' | '!' | '?' | ':' | ';' | ')' | ']') && out.ends_with(' ') {
            out.pop();
        }
        out.push(c);
    }
    out
}

/// Reads the number expression starting at `start` (a digit or a minus
/// sign before one) and returns its words and the index after it.
fn read_number(chars: &[char], start: usize, currency: bool) -> (String, usize) {
    let digits_at = |i: usize| {
        chars[i.min(chars.len())..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count()
    };
    let mut words = Vec::new();
    let mut i = start;
    if chars[i] == '-' {
        words.push("мінус".to_owned());
        i += 1;
    }
    let head_len = digits_at(i);
    let mut integer: String = chars[i..i + head_len].iter().collect();
    i += head_len;

    // Time: 10:30.
    if head_len <= 2 && chars.get(i) == Some(&':') && digits_at(i + 1) == 2 {
        let minutes: String = chars[i + 1..i + 3].iter().collect();
        words.push(integer_words(&integer));
        words.push(integer_words(&minutes));
        return (words.join(" "), i + 3);
    }

    // Thousands groups: 1 000 000, 1,000,000.
    // A number never starts with a zero group: "0,125" is a decimal.
    if head_len <= 3 && integer != "0" {
        while let Some(&separator) = chars.get(i)
            && matches!(separator, ' ' | '\u{a0}' | '\u{2009}' | '\u{202f}' | ',')
            && digits_at(i + 1) == 3
        {
            integer.extend(&chars[i + 1..i + 4]);
            i += 4;
        }
    }
    let mut parts = vec![integer_words(&integer)];
    let mut fraction = false;
    // Decimals and versions: 3,5 · 3.5 · 1.2.3.
    while let Some(&separator) = chars.get(i)
        && matches!(separator, '.' | ',')
        && digits_at(i + 1) > 0
    {
        let len = digits_at(i + 1);
        let part: String = chars[i + 1..i + 1 + len].iter().collect();
        parts.push(
            if separator == ',' {
                "кома"
            } else {
                "крапка"
            }
            .to_owned(),
        );
        parts.push(integer_words(&part));
        fraction = true;
        i += 1 + len;
    }
    words.extend(parts);

    // Units after the number.
    let after_space = if chars.get(i) == Some(&' ') { i + 1 } else { i };
    if chars.get(after_space) == Some(&'%') {
        words.push(if fraction {
            "відсотка".to_owned()
        } else {
            plural(&integer, "відсоток", "відсотки", "відсотків").to_owned()
        });
        i = after_space + 1;
    } else if chars.get(after_space) == Some(&'°') {
        words.push(if fraction {
            "градуса".to_owned()
        } else {
            plural(&integer, "градус", "градуси", "градусів").to_owned()
        });
        i = after_space + 1;
        if let Some(scale) = temperature_scale(chars.get(i)) {
            words.push(scale.to_owned());
            i += 1;
        }
    }
    if currency {
        words.push(if fraction {
            "долара".to_owned()
        } else {
            plural(&integer, "долар", "долари", "доларів").to_owned()
        });
    }
    (words.join(" "), i)
}

/// A digit string in words: a number, or digit by digit when it is long or
/// starts with zero.
fn integer_words(digits: &str) -> String {
    if digits.len() > MAX_NUMBER_DIGITS || (digits.len() > 1 && digits.starts_with('0')) {
        digits
            .bytes()
            .map(|d| UNITS[usize::from(d - b'0')])
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        number_words(digits)
    }
}

/// The noun form after a number: 1 відсоток, 2–4 відсотки, 5+ відсотків.
fn plural<'a>(digits: &str, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let tail: u32 = digits
        .get(digits.len().saturating_sub(2)..)
        .and_then(|t| t.parse().ok())
        .unwrap_or(0);
    match (tail % 10, tail % 100) {
        (_, 11..=14) => many,
        (1, _) => one,
        (2..=4, _) => few,
        _ => many,
    }
}

/// A non-negative integer in words (nominative).
pub(crate) fn number_words(digits: &str) -> String {
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return UNITS[0].to_owned();
    }
    // Groups of three digits, lowest first.
    let groups: Vec<usize> = digits
        .as_bytes()
        .rchunks(3)
        .map(|chunk| chunk.iter().fold(0, |n, d| n * 10 + usize::from(d - b'0')))
        .collect();
    let mut words = Vec::new();
    for (power, &group) in groups.iter().enumerate().rev() {
        if group == 0 {
            continue;
        }
        let feminine = power > 0 && SCALES.get(power - 1).is_some_and(|s| s.3);
        let hundreds = group / 100;
        let rest = group % 100;
        if hundreds > 0 {
            words.push(HUNDREDS[hundreds]);
        }
        match rest {
            0 => {}
            1..=9 => words.push(match (rest, feminine) {
                (1, true) => "одна",
                (2, true) => "дві",
                _ => UNITS[rest],
            }),
            10..=19 => words.push(TEENS[rest - 10]),
            _ => {
                words.push(TENS[rest / 10]);
                if rest % 10 > 0 {
                    words.push(match (rest % 10, feminine) {
                        (1, true) => "одна",
                        (2, true) => "дві",
                        (unit, _) => UNITS[unit],
                    });
                }
            }
        }
        if power > 0 {
            let (one, few, many, _) = SCALES[(power - 1).min(SCALES.len() - 1)];
            words.push(plural(&group.to_string(), one, few, many));
        }
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_become_words() {
        assert_eq!(for_ukrainian_voice("версія 3"), "версія три");
        assert_eq!(
            for_ukrainian_voice("2026 року"),
            "дві тисячі двадцять шість року"
        );
        assert_eq!(
            for_ukrainian_voice("1 000 000 і 1,000,000"),
            "один мільйон і один мільйон"
        );
        assert_eq!(for_ukrainian_voice("-5"), "мінус п'ять");
        assert_eq!(
            for_ukrainian_voice("3,5 і 3.5"),
            "три кома п'ять і три крапка п'ять"
        );
        assert_eq!(for_ukrainian_voice("о 10:30"), "о десять тридцять");
        assert_eq!(for_ukrainian_voice("0"), "нуль");
        assert_eq!(
            for_ukrainian_voice("11, 12, 19, 40, 90, 100, 215, 1001"),
            "одинадцять, дванадцять, дев'ятнадцять, сорок, дев'яносто, сто, двісті п'ятнадцять, одна тисяча один"
        );
        // A space before three digits separates thousands in Ukrainian.
        assert_eq!(
            for_ukrainian_voice("2 500 гривень"),
            "дві тисячі п'ятсот гривень"
        );
        assert_eq!(number_words("1000000000"), "один мільярд");
        assert_eq!(number_words("22"), "двадцять два");
    }

    #[test]
    fn percentages_agree_with_the_number() {
        assert_eq!(for_ukrainian_voice("1%"), "один відсоток");
        assert_eq!(for_ukrainian_voice("3%"), "три відсотки");
        assert_eq!(for_ukrainian_voice("50 %"), "п'ятдесят відсотків");
        assert_eq!(for_ukrainian_voice("12%"), "дванадцять відсотків");
        assert_eq!(for_ukrainian_voice("22%."), "двадцять два відсотки.");
    }

    #[test]
    fn temperatures_and_degrees() {
        assert_eq!(
            for_ukrainian_voice("+15°C"),
            "плюс п'ятнадцять градусів за Цельсієм"
        );
        assert_eq!(for_ukrainian_voice("1°"), "один градус");
        assert_eq!(
            for_ukrainian_voice("−3 °C"),
            "мінус три градуси за Цельсієм"
        );
        assert_eq!(
            for_ukrainian_voice("72°F"),
            "сімдесят два градуси за Фаренгейтом"
        );
        assert_eq!(
            for_ukrainian_voice("36,6°"),
            "тридцять шість кома шість градуса"
        );
    }

    #[test]
    fn numbers_signs_and_separators() {
        assert_eq!(for_ukrainian_voice("№5"), "номер п'ять");
        assert_eq!(
            for_ukrainian_voice("1\u{202f}500 грн"),
            "одна тисяча п'ятсот грн"
        );
        assert_eq!(for_ukrainian_voice("1\u{2009}500"), "одна тисяча п'ятсот");
    }

    #[test]
    fn latin_look_alikes_inside_ukrainian_words() {
        // «i», «o», «a», «e», «c», «p» typed in the Latin layout.
        assert_eq!(for_ukrainian_voice("кiлька днiв"), "кілька днів");
        assert_eq!(for_ukrainian_voice("Kиїв"), "Київ");
        assert_eq!(for_ukrainian_voice("cпасибi"), "спасибі");
    }

    #[test]
    fn decimals_below_one_keep_their_comma() {
        assert_eq!(for_ukrainian_voice("0,125"), "нуль кома сто двадцять п'ять");
        assert_eq!(
            for_ukrainian_voice("-0,125"),
            "мінус нуль кома сто двадцять п'ять"
        );
    }

    #[test]
    fn long_digit_runs_and_versions() {
        assert_eq!(for_ukrainian_voice("1.2.3"), "один крапка два крапка три");
        assert_eq!(
            for_ukrainian_voice("0501234567"),
            "нуль п'ять нуль один два три чотири п'ять шість сім"
        );
    }

    #[test]
    fn symbols_and_apostrophes() {
        assert_eq!(for_ukrainian_voice("a+b"), for_ukrainian_voice("a плюс b"));
        assert_eq!(for_ukrainian_voice("20°"), "двадцять градусів");
        assert_eq!(for_ukrainian_voice("п’ять і пʼять"), "п'ять і п'ять");
        assert_eq!(for_ukrainian_voice("так/ні"), "так ні");
        assert_eq!(for_ukrainian_voice("x=1 & y"), "екс дорівнює один і вай");
        assert_eq!(for_ukrainian_voice("$5"), "п'ять доларів");
    }

    #[test]
    fn dictionary_words() {
        assert_eq!(
            for_ukrainian_voice("Запусти Python і відкрий GitHub"),
            "Запусти пайтон і відкрий ґітхаб"
        );
        assert_eq!(
            for_ukrainian_voice("Windows, Google, Claude, email"),
            "віндовс, ґуґл, клод, імейл"
        );
        assert_eq!(for_ukrainian_voice("Wi-Fi"), "вай-фай");
    }

    #[test]
    fn acronyms_are_spelled() {
        assert_eq!(for_ukrainian_voice("API"), "ей-пі-ай");
        assert_eq!(for_ukrainian_voice("USB і PDF"), "ю-ес-бі і пі-ді-еф");
        assert_eq!(for_ukrainian_voice("GPT-4"), "джі-пі-ті чотири");
        assert_eq!(for_ukrainian_voice("ChatGPT"), "чат джі-пі-ті");
        assert_eq!(for_ukrainian_voice("USB-C"), "ю-ес-бі-сі");
    }

    #[test]
    fn compounds_split_at_case_and_digits() {
        assert_eq!(for_ukrainian_voice("JavaScript"), "джава скрипт");
        assert_eq!(for_ukrainian_voice("v2"), "ві два");
    }

    #[test]
    fn other_words_follow_spelling_rules() {
        assert_eq!(for_ukrainian_voice("update"), "апдейт");
        assert_eq!(for_ukrainian_voice("file"), "файл");
        assert_eq!(for_ukrainian_voice("cargo test"), "карґо тест");
        assert_eq!(for_ukrainian_voice("café"), "кафе");
        assert_eq!(for_ukrainian_voice("user"), "юзер");
        assert_eq!(for_ukrainian_voice("system"), "систем");
        assert_eq!(for_ukrainian_voice("home"), "хоум");
    }

    #[test]
    fn brackets_keep_their_spacing() {
        assert_eq!(
            for_ukrainian_voice("[echo] (GitHub, 2 рази)"),
            "[ечо] (ґітхаб, два рази)"
        );
    }

    #[test]
    fn file_names_and_domains() {
        assert_eq!(
            for_ukrainian_voice("Відкрий setup.exe."),
            "Відкрий сетап крапка екзе."
        );
        assert_eq!(for_ukrainian_voice("PowerShell"), "павер шел");
    }

    #[test]
    fn cyrillic_text_is_untouched() {
        assert_eq!(for_ukrainian_voice("УВАГА, ЗСУ!"), "УВАГА, ЗСУ!");
    }

    #[test]
    fn the_voice_can_read_every_output_character() {
        // The symbol set of uk_UA-ukrainian_tts-medium (Piper lowercases).
        let readable = |c: char| {
            c.is_whitespace()
                || "абвгґдеєжзиіїйклмнопрстуфхцчшщьюя'-,.!?:;—"
                    .contains(c.to_lowercase().next().unwrap_or(c))
        };
        for text in [
            "Python 3.12, GitHub API, 50% & USB-C @home #1",
            "Ok, iPhone 15 Pro за $999",
            "Naïve façade: Größe, piñata.",
        ] {
            let out = for_ukrainian_voice(text);
            assert!(out.chars().all(readable), "{out}");
        }
    }

    #[test]
    fn ukrainian_text_without_digits_is_unchanged() {
        assert_eq!(
            for_ukrainian_voice("Привіт, як справи?"),
            "Привіт, як справи?"
        );
    }
}
