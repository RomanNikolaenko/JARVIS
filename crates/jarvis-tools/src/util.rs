//! Helpers shared by the tools.

use serde::de::DeserializeOwned;

use crate::ToolError;

/// Parses a tool input object; unknown fields are rejected by the argument
/// types (`deny_unknown_fields`).
pub(crate) fn parse<T: DeserializeOwned>(input: &serde_json::Value) -> Result<T, ToolError> {
    serde_json::from_value(input.clone()).map_err(|err| ToolError::InvalidInput(err.to_string()))
}

pub(crate) fn to_args<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, ToolError> {
    serde_json::to_value(value).map_err(|err| ToolError::Failed(err.to_string()))
}

/// Cuts text to `max` characters with a note about the cut.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}\n… [truncated: {} more characters]", count - max)
}

/// Text decoded as UTF-8 (lossy), or an error for binary content.
pub(crate) fn text_of(bytes: &[u8]) -> Result<String, ToolError> {
    let head = &bytes[..bytes.len().min(8192)];
    if head.contains(&0) {
        return Err(ToolError::Failed(
            "the file looks binary; only text files can be read".to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// Simple glob match (`*` any characters, `?` one character),
/// case-insensitive.
pub(crate) fn glob_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let name: Vec<char> = name.to_lowercase().chars().collect();
    let (mut p, mut n) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = n;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            n = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// An optional path from the model: missing, empty or blank means the first
/// workspace folder (models often send `""` for it).
pub(crate) fn optional_path(path: Option<&str>) -> &str {
    path.map(str::trim).filter(|p| !p.is_empty()).unwrap_or(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_names() {
        assert!(glob_match("*.md", "README.md"));
        assert!(glob_match("main.?s", "main.rs"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("*.md", "notes.txt"));
        assert!(glob_match("*test*", "integration_TESTS.rs"));
    }

    #[test]
    fn binary_and_long_texts() {
        assert!(text_of(b"plain text").is_ok());
        assert!(text_of(&[0x50, 0x00, 0x01]).is_err());
        let cut = truncate(&"a".repeat(20), 5);
        assert!(cut.starts_with("aaaaa\n"));
        assert!(cut.contains("15 more"));
    }
}
