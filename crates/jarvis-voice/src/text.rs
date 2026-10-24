//! Text for speech: splits a streamed reply into sentences so speaking can
//! start before the reply is complete, and removes what should not be read
//! aloud (Markdown syntax, code blocks, links, reasoning blocks).

const MAX_SENTENCE_CHARS: usize = 280;

#[derive(Debug, Default)]
pub struct SentenceSplitter {
    buffer: String,
    in_code: bool,
    in_think: bool,
}

impl SentenceSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds streamed text; returns the sentences completed by it, ready
    /// to be spoken.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buffer.push_str(delta);
        let mut sentences = Vec::new();
        while let Some(end) = self.sentence_end() {
            let raw: String = self.buffer.drain(..end).collect();
            self.take(&raw, &mut sentences);
        }
        sentences
    }

    /// The rest of the reply.
    pub fn finish(&mut self) -> Vec<String> {
        let raw = std::mem::take(&mut self.buffer);
        let mut sentences = Vec::new();
        self.take(&raw, &mut sentences);
        self.in_code = false;
        self.in_think = false;
        sentences
    }

    /// Byte index just past the next sentence boundary in the buffer.
    fn sentence_end(&self) -> Option<usize> {
        let text = &self.buffer;
        let mut chars = text.char_indices().peekable();
        while let Some((index, c)) = chars.next() {
            let next = chars.peek().map(|&(_, n)| n);
            let boundary = match c {
                '\n' => true,
                '.' | '!' | '?' | '…' | ';' => next.is_some_and(char::is_whitespace),
                _ => false,
            };
            if boundary {
                return Some(index + c.len_utf8());
            }
        }
        // A very long run without punctuation: cut at a space.
        if text.chars().count() > MAX_SENTENCE_CHARS {
            let cut = text
                .char_indices()
                .take(MAX_SENTENCE_CHARS)
                .filter(|(_, c)| c.is_whitespace())
                .map(|(i, _)| i)
                .last()?;
            return Some(cut + 1);
        }
        None
    }

    fn take(&mut self, raw: &str, out: &mut Vec<String>) {
        for line in raw.split('\n') {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                self.in_code = !self.in_code;
                continue;
            }
            if trimmed.contains("<think>") {
                self.in_think = true;
            }
            if self.in_think {
                if trimmed.contains("</think>") {
                    self.in_think = false;
                }
                continue;
            }
            if self.in_code {
                continue;
            }
            let spoken = clean_for_speech(trimmed);
            if spoken.chars().any(char::is_alphanumeric) {
                out.push(spoken);
            }
        }
    }
}

/// Removes Markdown and links from one line.
pub fn clean_for_speech(line: &str) -> String {
    let mut text = line.trim_start();
    // Headings, quotes and list markers.
    text = text.trim_start_matches(['#', '>']).trim_start();
    for marker in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = text.strip_prefix(marker) {
            text = rest;
        }
    }
    let numbered = text
        .split_once(". ")
        .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .map(|(_, rest)| rest);
    let text = numbered.unwrap_or(text);

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        // [label](url) → label
        if let Some(stripped) = rest.strip_prefix('[')
            && let Some(close) = stripped.find("](")
            && let Some(end) = stripped[close + 2..].find(')')
        {
            out.push_str(&stripped[..close]);
            rest = &stripped[close + 2 + end + 1..];
            continue;
        }
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            rest = &rest[end..];
            continue;
        }
        let Some(c) = rest.chars().next() else { break };
        if !matches!(c, '*' | '_' | '`' | '|') {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_are_emitted_as_soon_as_they_end() {
        let mut s = SentenceSplitter::new();
        assert_eq!(s.push("Привіт"), Vec::<String>::new());
        assert_eq!(s.push("! Як "), vec!["Привіт!".to_owned()]);
        assert_eq!(s.push("справи? Добре"), vec!["Як справи?".to_owned()]);
        assert_eq!(s.finish(), vec!["Добре".to_owned()]);
    }

    #[test]
    fn markdown_code_and_reasoning_are_not_read() {
        let mut s = SentenceSplitter::new();
        let mut spoken = s
            .push("<think>let me see</think>\n## План\n- **Перше**: зробити [це](https://x.ua).\n");
        spoken
            .extend(s.push("```rust\nfn main() {}\n```\nГотово, див. https://example.com зараз. "));
        spoken.extend(s.finish());
        assert_eq!(
            spoken,
            vec![
                "План".to_owned(),
                "Перше: зробити це.".to_owned(),
                "Готово, див.".to_owned(),
                "зараз.".to_owned(),
            ]
        );
    }

    #[test]
    fn long_runs_are_cut_at_a_space() {
        let mut s = SentenceSplitter::new();
        let long = "слово ".repeat(80);
        let sentences = s.push(&long);
        assert!(!sentences.is_empty());
        assert!(
            sentences
                .iter()
                .all(|x| x.chars().count() <= MAX_SENTENCE_CHARS)
        );
    }

    #[test]
    fn numbered_lists_and_tables() {
        assert_eq!(clean_for_speech("1. Купити молоко"), "Купити молоко");
        assert_eq!(clean_for_speech("| a | b |"), "a b");
        assert_eq!(clean_for_speech("`cargo test` passes"), "cargo test passes");
    }
}
