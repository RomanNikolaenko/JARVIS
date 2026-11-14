//! The blocks a mentor reply may carry for JARVIS: the lesson plan and a
//! change to write into a project file.
//!
//! ```text
//! <plan>…</plan>
//! <write path="src/app.ts" mode="create|replace">
//! <find>exact existing text</find>        (replace only)
//! <say>short explanation</say>
//! <code>
//! lines
//! </code>
//! … more say/code pairs …
//! </write>
//! ```

/// What JARVIS takes from a mentor reply.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct MentorReply {
    /// The lesson plan from the last `<plan>` block.
    pub(crate) plan: Option<String>,
    /// The change to write, or why the block was refused.
    pub(crate) write: Option<Result<WriteBlock, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteBlock {
    /// Relative to the project folder, with `/` separators.
    pub(crate) path: String,
    pub(crate) change: WriteChange,
    pub(crate) parts: Vec<WritePart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteChange {
    /// A new file with the code.
    Create,
    /// The code replaces this exact fragment of an existing file.
    Replace { find: String },
}

/// A piece of code and what JARVIS says before typing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WritePart {
    pub(crate) say: String,
    pub(crate) code: String,
}

impl WriteBlock {
    /// Turns the code (and the fragment to find) into CRLF line endings.
    pub(crate) fn use_crlf(&mut self) {
        let crlf = |text: &str| text.replace("\r\n", "\n").replace('\n', "\r\n");
        if let WriteChange::Replace { find } = &mut self.change {
            *find = crlf(find);
        }
        for part in &mut self.parts {
            part.code = crlf(&part.code);
        }
    }

    /// All the code, in order.
    pub(crate) fn code(&self) -> String {
        self.parts.iter().map(|p| p.code.as_str()).collect()
    }
}

pub(crate) fn parse_reply(text: &str) -> MentorReply {
    MentorReply {
        plan: last_plan(text),
        write: text.find("<write").map(|start| parse_write(&text[start..])),
    }
}

fn last_plan(text: &str) -> Option<String> {
    let start = text.rfind("<plan>")? + "<plan>".len();
    let end = text[start..].find("</plan>")? + start;
    let plan = text[start..end].trim();
    (!plan.is_empty()).then(|| plan.to_owned())
}

fn parse_write(text: &str) -> Result<WriteBlock, String> {
    let open_end = text.find('>').ok_or("the <write> tag is not closed")?;
    let open = &text[..open_end];
    let body_end = text
        .find("</write>")
        .ok_or("the write block has no </write>")?;
    if body_end < open_end {
        return Err("the write block is malformed".into());
    }
    let body = &text[open_end + 1..body_end];

    let path = attribute(open, "path").ok_or("the write block has no path")?;
    let path = check_path(&path)?;
    let change = match attribute(open, "mode").as_deref() {
        Some("create") => WriteChange::Create,
        Some("replace") => {
            let find = between(body, "<find>", "</find>")
                .map(strip_first_newline)
                .filter(|f| !f.trim().is_empty())
                .ok_or("a replace block needs a non-empty <find> fragment")?;
            WriteChange::Replace {
                find: find.to_owned(),
            }
        }
        Some(other) => return Err(format!("unknown write mode \"{other}\"")),
        None => return Err("the write block has no mode".into()),
    };
    let parts = parts(body)?;
    if parts.is_empty() {
        return Err("the write block has no <code>".into());
    }
    Ok(WriteBlock {
        path,
        change,
        parts,
    })
}

/// The say/code pairs in order; a code without a say is spoken silently.
fn parts(body: &str) -> Result<Vec<WritePart>, String> {
    // The fragment to find is not code to type.
    let mut rest = match body.find("</find>") {
        Some(end) => &body[end + "</find>".len()..],
        None => body,
    };
    let mut parts = Vec::new();
    let mut say = String::new();
    loop {
        let next_say = rest.find("<say>");
        let next_code = rest.find("<code>");
        match (next_say, next_code) {
            (Some(s), code) if code.is_none_or(|c| s < c) => {
                let end = rest[s..].find("</say>").ok_or("a <say> is not closed")? + s;
                say = rest[s + "<say>".len()..end].trim().to_owned();
                rest = &rest[end + "</say>".len()..];
            }
            (_, Some(c)) => {
                let end = rest[c..].find("</code>").ok_or("a <code> is not closed")? + c;
                let code = strip_first_newline(&rest[c + "<code>".len()..end]);
                parts.push(WritePart {
                    say: std::mem::take(&mut say),
                    code: code.to_owned(),
                });
                rest = &rest[end + "</code>".len()..];
            }
            _ => break,
        }
    }
    Ok(parts)
}

/// A relative path inside the project, with `/` separators.
fn check_path(path: &str) -> Result<String, String> {
    let path = path.trim().replace('\\', "/");
    let absolute = path.starts_with('/') || path.contains(':');
    let escapes = path.split('/').any(|part| part == "..");
    if path.is_empty() || absolute || escapes {
        return Err(format!(
            "\"{path}\" is not a path inside the project folder"
        ));
    }
    Ok(path)
}

/// `name="value"` or `name='value'` in a tag.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let start = tag.find(&format!("{name}="))? + name.len() + 1;
    let quote = tag[start..]
        .chars()
        .next()
        .filter(|q| *q == '"' || *q == '\'')?;
    let value_start = start + 1;
    let end = tag[value_start..].find(quote)? + value_start;
    Some(tag[value_start..end].to_owned())
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

/// Code starts on the line after its tag.
fn strip_first_newline(text: &str) -> &str {
    text.strip_prefix("\r\n")
        .or_else(|| text.strip_prefix('\n'))
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn parses_plan_and_create_block() {
        let reply = "Добре, почнемо.\n<plan>\nМета: TODO на Angular\n1. Компонент\n</plan>\n<write path=\"src/app/todo.ts\" mode=\"create\">\n<say>Спершу імпорт.</say>\n<code>\nimport { Component } from '@angular/core';\n\n</code>\n<say>Тепер клас.</say>\n<code>\nexport class Todo {}\n</code>\n</write>\nСкажи, коли збережеш.";
        let parsed = parse_reply(reply);
        assert_eq!(
            parsed.plan.as_deref(),
            Some("Мета: TODO на Angular\n1. Компонент")
        );
        let write = parsed.write.expect("write block").expect("valid");
        assert_eq!(write.path, "src/app/todo.ts");
        assert_eq!(write.change, WriteChange::Create);
        assert_eq!(write.parts.len(), 2);
        assert_eq!(write.parts[0].say, "Спершу імпорт.");
        assert_eq!(
            write.parts[0].code,
            "import { Component } from '@angular/core';\n\n"
        );
        assert_eq!(
            write.code(),
            "import { Component } from '@angular/core';\n\nexport class Todo {}\n"
        );
    }

    #[test]
    fn parses_replace_block_with_find() {
        let reply = "<write path='src/main.ts' mode='replace'>\n<find>\nconst a = 1;\n</find>\n<say>Змінимо значення.</say>\n<code>\nconst a = 2;\n</code>\n</write>";
        let write = parse_reply(reply).write.unwrap().unwrap();
        assert_eq!(
            write.change,
            WriteChange::Replace {
                find: "const a = 1;\n".into()
            }
        );
        assert_eq!(write.code(), "const a = 2;\n");
        assert!(parse_reply(reply).plan.is_none());
    }

    #[test]
    fn rejects_paths_outside_and_ambiguous_fragments() {
        let bad = [
            "<write path=\"/etc/passwd\" mode=\"create\"><say>x</say><code>\nx\n</code></write>",
            "<write path=\"C:\\Windows\\x.txt\" mode=\"create\"><say>x</say><code>\nx\n</code></write>",
            "<write path=\"../secret.txt\" mode=\"create\"><say>x</say><code>\nx\n</code></write>",
            "<write path=\"src/a.ts\" mode=\"replace\"><find></find><say>x</say><code>\nx\n</code></write>",
            "<write path=\"src/a.ts\" mode=\"create\"><say>x</say></write>",
            "<write path=\"src/a.ts\" mode=\"delete\"><say>x</say><code>\nx\n</code></write>",
            "<write mode=\"create\"><say>x</say><code>\nx\n</code></write>",
            "<write path=\"src/a.ts\" mode=\"create\"><say>x</say><code>\nx\n</code>",
        ];
        for reply in bad {
            assert!(
                matches!(parse_reply(reply).write, Some(Err(_))),
                "accepted: {reply}"
            );
        }
        assert!(parse_reply("Просто текст без блоків.").write.is_none());
    }

    #[test]
    fn only_the_last_plan_counts() {
        let reply = "<plan>old</plan>\nтекст\n<plan>\nnew\n</plan>";
        assert_eq!(parse_reply(reply).plan.as_deref(), Some("new"));
        assert!(parse_reply("<plan>  </plan>").plan.is_none());
    }
}
