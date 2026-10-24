//! The wake word «Джарвіс» / "Jarvis" in transcribed speech.
//!
//! Speech recognition spells the name in many ways ("Джарвис", "Джервіс",
//! "Jarvis.", "Jervis"); a word matches when it is within a small edit
//! distance of one of the canonical spellings. The text after the wake word
//! is the command ("Джарвіс, яка погода?" → "яка погода?").

/// Canonical spellings and how many edits a match may differ by. English
/// gets one: "Davis" or "Jars" are two edits from "jarvis".
const CANONICAL: &[(&str, usize)] = &[("джарвіс", 2), ("джарвис", 2), ("jarvis", 1)];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeMatch {
    /// What was said after the wake word, trimmed (may be empty).
    pub command: String,
}

/// Finds the wake word among the first words of a transcript.
pub fn find(transcript: &str) -> Option<WakeMatch> {
    let words: Vec<&str> = transcript.split_whitespace().collect();
    // Only near the start: "the jarvis project is late" is not a call.
    let scan = words.len().min(3);
    for index in 0..scan {
        let single = normalize(words[index]);
        if is_wake_word(&single) {
            return Some(command_after(&words, index + 1));
        }
        // "Джар віс" split in two.
        if let Some(next) = words.get(index + 1) {
            let joined = format!("{single}{}", normalize(next));
            if is_wake_word(&joined) {
                return Some(command_after(&words, index + 2));
            }
        }
    }
    None
}

fn command_after(words: &[&str], from: usize) -> WakeMatch {
    let rest = words[from.min(words.len())..].join(" ");
    let command = rest
        .trim_start_matches(|c: char| {
            c.is_ascii_punctuation() || c == '—' || c == '–' || c.is_whitespace()
        })
        .trim()
        .to_owned();
    WakeMatch { command }
}

fn normalize(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphabetic())
        .flat_map(char::to_lowercase)
        .map(|c| if c == 'ї' { 'і' } else { c })
        .collect()
}

fn is_wake_word(word: &str) -> bool {
    let length = word.chars().count();
    // The "j"/"дж" sound is what sets the name apart ("Дарвін" is two edits away).
    if !(4..=10).contains(&length) || !word.contains(['ж', 'j']) {
        return false;
    }
    CANONICAL
        .iter()
        .any(|&(canonical, max)| levenshtein(word, canonical) <= max)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(text: &str) -> Option<String> {
        find(text).map(|m| m.command)
    }

    #[test]
    fn recognizes_common_spellings_and_extracts_the_command() {
        assert_eq!(
            command("Джарвіс, яка погода?").as_deref(),
            Some("яка погода?")
        );
        assert_eq!(
            command("Джарвис. Увімкни світло").as_deref(),
            Some("Увімкни світло")
        );
        assert_eq!(
            command("Jarvis, what time is it?").as_deref(),
            Some("what time is it?")
        );
        assert_eq!(
            command("Hey Jervis — open the notes").as_deref(),
            Some("open the notes")
        );
        assert_eq!(command("Жарвіс").as_deref(), Some(""));
        assert_eq!(command("Ну, Джар віс, привіт").as_deref(), Some("привіт"));
        assert_eq!(command("ДЖАРВІСЕ!").as_deref(), Some(""));
    }

    #[test]
    fn other_words_do_not_wake_it() {
        for text in [
            "Як справи?",
            "The service is down",
            "Джерело даних недоступне",
            "Харківська область",
            "Чарльз Дарвін писав про це",
            "Davis called",
            "Jars of jam",
            "jar",
            "",
            // Too far from the start to be a call.
            "We talked for an hour about Jarvis",
        ] {
            assert_eq!(find(text), None, "{text}");
        }
    }
}
