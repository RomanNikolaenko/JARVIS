//! English (Latin-script) words in Cyrillic, as Ukrainians pronounce them.
//!
//! A dictionary covers frequent words and names; acronyms are spelled with
//! English letter names; other words follow approximate spelling rules.

/// Frequent words, names and tech terms (lowercase keys).
const DICTIONARY: &[(&str, &str)] = &[
    ("and", "енд"),
    ("android", "андроїд"),
    ("angular", "анґуляр"),
    ("anthropic", "антропік"),
    ("app", "еп"),
    ("apple", "епл"),
    ("bluetooth", "блютуз"),
    ("cafe", "кафе"),
    ("cargo", "карґо"),
    ("chatgpt", "чат джі-пі-ті"),
    ("chrome", "хром"),
    ("claude", "клод"),
    ("codex", "кодекс"),
    ("docker", "докер"),
    ("email", "імейл"),
    ("exe", "екзе"),
    ("facebook", "фейсбук"),
    ("git", "ґіт"),
    ("github", "ґітхаб"),
    ("gitlab", "ґітлаб"),
    ("google", "ґуґл"),
    ("hello", "хелоу"),
    ("instagram", "інстаграм"),
    ("iphone", "айфон"),
    ("is", "із"),
    ("jarvis", "джарвіс"),
    ("java", "джава"),
    ("javascript", "джава скрипт"),
    ("json", "джейсон"),
    ("laptop", "лептоп"),
    ("linux", "лінукс"),
    ("macos", "мак-о-ес"),
    ("microsoft", "майкрософт"),
    ("node", "ноуд"),
    ("npm", "ен-пі-ем"),
    ("of", "ов"),
    ("offline", "офлайн"),
    ("ok", "окей"),
    ("ollama", "оллама"),
    ("online", "онлайн"),
    ("openai", "опен-ей-ай"),
    ("piper", "пайпер"),
    ("pnpm", "пі-ен-пі-ем"),
    ("powershell", "павер шел"),
    ("python", "пайтон"),
    ("react", "ріакт"),
    ("rust", "раст"),
    ("tauri", "таурі"),
    ("telegram", "телеграм"),
    ("the", "зе"),
    ("to", "ту"),
    ("typescript", "тайп скрипт"),
    ("ubuntu", "убунту"),
    ("whisper", "віспер"),
    ("wi-fi", "вай-фай"),
    ("wifi", "вай-фай"),
    ("windows", "віндовс"),
    ("youtube", "ютуб"),
];

/// English letter names, A to Z.
const LETTERS: [&str; 26] = [
    "ей",
    "бі",
    "сі",
    "ді",
    "і",
    "еф",
    "джі",
    "ейч",
    "ай",
    "джей",
    "кей",
    "ел",
    "ем",
    "ен",
    "оу",
    "пі",
    "к'ю",
    "ар",
    "ес",
    "ті",
    "ю",
    "ві",
    "дабл-ю",
    "екс",
    "вай",
    "зед",
];

/// Letter groups read as one sound, longest first.
const PATTERNS: &[(&str, &str)] = &[
    ("tion", "шн"),
    ("sion", "жн"),
    ("ture", "чер"),
    ("ough", "оу"),
    ("igh", "ай"),
    ("sch", "ск"),
    ("tch", "ч"),
    ("sh", "ш"),
    ("ch", "ч"),
    ("th", "т"),
    ("ph", "ф"),
    ("ck", "к"),
    ("qu", "кв"),
    ("wh", "в"),
    ("ng", "нг"),
    ("ee", "і"),
    ("ea", "і"),
    ("oo", "у"),
    ("ou", "ау"),
    ("ow", "оу"),
    ("ai", "ей"),
    ("ay", "ей"),
    ("ei", "ей"),
    ("ey", "ей"),
    ("oi", "ой"),
    ("oy", "ой"),
    ("au", "о"),
    ("aw", "о"),
    ("ew", "ю"),
    ("oa", "оу"),
    ("ie", "і"),
    ("ue", "ю"),
];

/// Whether `c` is a Latin letter, with or without diacritics.
pub(super) fn is_latin(c: char) -> bool {
    c.is_ascii_alphabetic() || fold(c).is_some()
}

/// Latin letters with diacritics as plain ASCII.
fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => "a",
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => "e",
        'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => "i",
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' | 'Ø' => "o",
        'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => "u",
        'ý' | 'ÿ' | 'Ý' => "y",
        'ñ' | 'Ñ' => "n",
        'ç' | 'Ç' => "c",
        'ß' => "ss",
        'æ' | 'Æ' => "ae",
        'œ' | 'Œ' => "oe",
        _ => return None,
    })
}

/// A Latin-script token (letters, inner hyphens, apostrophes and dots) in
/// Cyrillic. Dots (file names, domains) are read as «крапка».
pub(super) fn latin_word(token: &str) -> String {
    token
        .split('.')
        .filter(|part| !part.is_empty())
        .map(dotless_word)
        .collect::<Vec<_>>()
        .join(" крапка ")
}

fn dotless_word(token: &str) -> String {
    let mut plain = String::with_capacity(token.len());
    for c in token.chars() {
        match fold(c) {
            // Keep the case of the first folded letter for acronyms.
            Some(ascii) if c.is_uppercase() => plain.push_str(&ascii.to_ascii_uppercase()),
            Some(ascii) => plain.push_str(ascii),
            None if c == '\'' => {}
            None => plain.push(c),
        }
    }
    if let Some(word) = lookup(&plain) {
        return word.to_owned();
    }
    plain
        .split('-')
        .filter(|part| !part.is_empty())
        .map(part_word)
        .collect::<Vec<_>>()
        .join("-")
}

fn lookup(word: &str) -> Option<&'static str> {
    let key = word.to_ascii_lowercase();
    DICTIONARY
        .iter()
        .find(|(entry, _)| *entry == key)
        .map(|(_, spoken)| *spoken)
}

fn part_word(part: &str) -> String {
    if let Some(word) = lookup(part) {
        return word.to_owned();
    }
    let pieces = split_case(part);
    if pieces.len() > 1 {
        return pieces
            .iter()
            .map(|piece| piece_word(piece))
            .collect::<Vec<_>>()
            .join(" ");
    }
    piece_word(part)
}

fn piece_word(piece: &str) -> String {
    if let Some(word) = lookup(piece) {
        word.to_owned()
    } else if is_acronym(piece) {
        spell(piece)
    } else {
        rules(&piece.to_ascii_lowercase())
    }
}

/// One letter, or 2–6 capitals.
fn is_acronym(word: &str) -> bool {
    let len = word.chars().count();
    len == 1 || ((2..=6).contains(&len) && word.chars().all(|c| c.is_ascii_uppercase()))
}

fn spell(word: &str) -> String {
    word.bytes()
        .filter(u8::is_ascii_alphabetic)
        .map(|b| LETTERS[usize::from(b.to_ascii_lowercase() - b'a')])
        .collect::<Vec<_>>()
        .join("-")
}

/// Splits compounds: JavaScript → Java + Script, HTMLParser → HTML + Parser.
fn split_case(word: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let mut cuts = vec![0];
    for i in 1..chars.len() {
        let (at, c) = chars[i];
        let prev = chars[i - 1].1;
        let next_lower = chars
            .get(i + 1)
            .is_some_and(|(_, n)| n.is_ascii_lowercase());
        if c.is_ascii_uppercase()
            && (prev.is_ascii_lowercase() || (prev.is_ascii_uppercase() && next_lower))
        {
            cuts.push(at);
        }
    }
    cuts.push(word.len());
    cuts.windows(2).map(|w| &word[w[0]..w[1]]).collect()
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

fn is_vowel_or_y(c: char) -> bool {
    is_vowel(c) || c == 'y'
}

/// Approximate English pronunciation of a lowercase ASCII word.
fn rules(word: &str) -> String {
    let w: Vec<char> = word.chars().collect();
    let n = w.len();
    // A final "e" after a consonant is silent; after a single vowel and a
    // single consonant it makes that vowel long (file, home, update).
    let silent_e = n >= 3 && w[n - 1] == 'e' && !is_vowel(w[n - 2]);
    let long_at =
        (silent_e && is_vowel_or_y(w[n - 3]) && (n < 4 || !is_vowel(w[n - 4]))).then(|| n - 3);

    let mut out = String::with_capacity(n * 2);
    let mut i = 0;
    while i < n {
        if silent_e && i == n - 1 {
            break;
        }
        if i == 0 && (word.starts_with("kn") || word.starts_with("wr")) {
            i += 1;
            continue;
        }
        if long_at.is_none_or(|at| at != i)
            && let Some((pattern, spoken)) = PATTERNS.iter().find(|(p, _)| word[i..].starts_with(p))
        {
            out.push_str(spoken);
            i += pattern.len();
            continue;
        }
        let c = w[i];
        let prev = i.checked_sub(1).map(|j| w[j]);
        let next = w.get(i + 1).copied();
        let after_next = w.get(i + 2).copied();
        // Double consonants sound once (app, pull).
        if prev == Some(c) && !is_vowel(c) {
            i += 1;
            continue;
        }
        let long = long_at == Some(i);
        let spoken = match c {
            'a' if long => "ей",
            'a' => "а",
            'e' if long => "і",
            'e' => "е",
            'o' if long => "оу",
            'o' => "о",
            'u' if long => "ю",
            // Closed syllable (up, hub, update): «а»; open (user, music): «ю».
            'u' if next.is_none_or(|n| !is_vowel_or_y(n))
                && after_next.is_none_or(|a| !is_vowel_or_y(a)) =>
            {
                "а"
            }
            'u' => "ю",
            'y' if i == 0 && next.is_some_and(is_vowel) => "й",
            'y' if i == n - 1 && i > 0 => "і",
            'i' | 'y' if long => "ай",
            'i' | 'y' => i_sound(&out, next),
            'c' if next.is_some_and(|n| matches!(n, 'e' | 'i' | 'y')) => "с",
            'c' | 'k' | 'q' => "к",
            'g' if next == Some('e') && i + 2 == n => "дж",
            'g' => "ґ",
            's' if prev.is_some_and(is_vowel) && next.is_some_and(is_vowel_or_y) => "з",
            's' => "с",
            'x' => "кс",
            'b' => "б",
            'd' => "д",
            'f' => "ф",
            'h' => "х",
            'j' => "дж",
            'l' => "л",
            'm' => "м",
            'n' => "н",
            'p' => "п",
            'r' => "р",
            't' => "т",
            'v' | 'w' => "в",
            'z' => "з",
            _ => "",
        };
        out.push_str(spoken);
        i += 1;
    }
    out
}

/// «и» after д т з с ц ч ш ж р before a consonant (the Ukrainian «rule of
/// nine» for loanwords: скрипт, система), «і» otherwise.
fn i_sound(spoken_so_far: &str, next: Option<char>) -> &'static str {
    let after_nine = spoken_so_far
        .chars()
        .last()
        .is_some_and(|c| "дтзсцчшжр".contains(c));
    if after_nine && next.is_some_and(|n| !is_vowel_or_y(n)) {
        "и"
    } else {
        "і"
    }
}
