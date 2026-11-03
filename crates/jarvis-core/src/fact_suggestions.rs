//! The request that asks a conversation's model for lasting facts the user
//! stated about themselves, and the parser of its reply. The scheduler that
//! sends it lives in `memory`.

use jarvis_ai::{ChatMessage, ChatRole};
use jarvis_memory::{MessageRole, StoredMessage, text};

/// Suggestions kept from one reply.
pub(crate) const MAX_SUGGESTIONS: usize = 5;
/// Conversation text sent with one request, in estimated tokens.
pub(crate) const INPUT_TOKENS: u32 = 3_000;
pub(crate) const MAX_OUTPUT_TOKENS: u32 = 400;
const MIN_CHARS: usize = 3;
const MAX_CHARS: usize = 300;

const SYSTEM_PROMPT: &str = "You find lasting facts that the user stated about themselves or \
their life in a conversation with JARVIS, their assistant.

Rules:
- Use only lines that start with \"User:\". Lines from JARVIS are context, never a source.
- A fact stays true for weeks or longer: name, family, work, home, pets, health, \
preferences, long-term plans. Skip one-off states (\"I'm hungry now\"), questions, \
requests and anything JARVIS said.
- Never include passwords, keys, tokens, card or document numbers, or other secrets.
- Write each fact as one short sentence about the user in the third person, in the \
language the user wrote in (for example \"Користувач працює вчителем.\").
- At most 5 facts.

Reply with exactly one JSON object and nothing else: {\"facts\": [\"...\"]}. \
When there is nothing to keep, reply {\"facts\": []}.";

const HEADER: &str = "Messages of the conversation, oldest first. Only lines that start \
with \"User:\" are sources.\n\n";

pub(crate) struct ExtractionRequest {
    pub messages: Vec<ChatMessage>,
    /// `seq` of the newest message the request covers.
    pub checked_until: i64,
}

/// The request for the messages not yet looked at, or `None` when there is
/// no user message among them. The newest messages that fit into
/// [`INPUT_TOKENS`] are sent; a newest message larger than that is cut.
pub(crate) fn extraction_request(messages: &[StoredMessage]) -> Option<ExtractionRequest> {
    let newest = messages.last()?;
    let mut lines = Vec::new();
    let mut budget = INPUT_TOKENS;
    for message in messages.iter().rev() {
        let content = message.content.trim();
        if content.is_empty() {
            continue;
        }
        let label = match message.role {
            MessageRole::User => "User",
            MessageRole::Assistant => "JARVIS (context only)",
        };
        let line = format!("{label}: {content}");
        let tokens = text::estimate_tokens(&line);
        if tokens <= budget {
            budget -= tokens;
            lines.push(line);
            continue;
        }
        if lines.is_empty() {
            lines.push(text::truncate_to_tokens(&line, budget));
        }
        break;
    }
    if !lines.iter().any(|line| line.starts_with("User: ")) {
        return None;
    }
    lines.reverse();
    Some(ExtractionRequest {
        messages: vec![
            ChatMessage::new(ChatRole::System, SYSTEM_PROMPT),
            ChatMessage::user(format!("{HEADER}{}", lines.join("\n\n"))),
        ],
        checked_until: newest.seq,
    })
}

/// The suggested facts of a reply, or `None` when it holds no JSON object
/// with a `facts` list. Reasoning blocks, code fences and text around the
/// object are ignored; items outside the length bounds are dropped.
pub(crate) fn parse_suggestions(reply: &str) -> Option<Vec<String>> {
    let reply = text::strip_reasoning(reply);
    for (start, _) in reply.match_indices('{') {
        let mut values =
            serde_json::Deserializer::from_str(&reply[start..]).into_iter::<serde_json::Value>();
        let Some(Ok(value)) = values.next() else {
            continue;
        };
        let Some(items) = value.get("facts").and_then(serde_json::Value::as_array) else {
            continue;
        };
        return Some(
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|item| (MIN_CHARS..=MAX_CHARS).contains(&item.chars().count()))
                .take(MAX_SUGGESTIONS)
                .map(str::to_owned)
                .collect(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_memory::MessageStatus;
    use uuid::Uuid;

    fn message(seq: i64, role: MessageRole, content: &str) -> StoredMessage {
        StoredMessage {
            id: Uuid::new_v4(),
            seq,
            role,
            content: content.to_owned(),
            status: MessageStatus::Complete,
            provider_id: None,
            model: None,
            tokens: text::estimate_tokens(content),
            created_at_ms: 0,
        }
    }

    #[test]
    fn only_user_messages_are_sources() {
        let request = extraction_request(&[
            message(3, MessageRole::User, "Я працюю вчителем."),
            message(4, MessageRole::Assistant, "Чудово!"),
        ])
        .expect("a user message");
        assert_eq!(request.checked_until, 4);
        assert_eq!(request.messages[0].role, ChatRole::System);
        let system = &request.messages[0].content;
        assert!(
            system.contains("the user stated about themselves"),
            "{system}"
        );
        assert!(system.contains("{\"facts\": []}"), "{system}");
        let user = &request.messages[1].content;
        assert!(user.contains("User: Я працюю вчителем."), "{user}");
        assert!(user.contains("JARVIS (context only): Чудово!"), "{user}");
    }

    #[test]
    fn a_message_larger_than_the_bound_is_cut_not_skipped() {
        let long = "a".repeat(20_000);
        let request =
            extraction_request(&[message(7, MessageRole::User, &long)]).expect("cut, not skipped");
        assert_eq!(request.checked_until, 7);
        let user = &request.messages[1].content;
        assert!(user.contains("User: aaa"), "the message is still there");
        assert!(user.chars().count() < 13_000, "{}", user.chars().count());
    }

    #[test]
    fn older_messages_beyond_the_bound_are_left_out() {
        let old = "b".repeat(9_000);
        let request = extraction_request(&[
            message(1, MessageRole::User, &old),
            message(2, MessageRole::User, &old),
            message(3, MessageRole::User, "Я маю кота."),
        ])
        .expect("request");
        let user = &request.messages[1].content;
        assert!(user.contains("User: Я маю кота."));
        assert_eq!(user.matches("User: bbb").count(), 1, "only what fits");
        assert_eq!(request.checked_until, 3);
    }

    #[test]
    fn no_user_messages_means_no_request() {
        assert!(extraction_request(&[message(1, MessageRole::Assistant, "Привіт!")]).is_none());
        assert!(extraction_request(&[]).is_none());
    }

    #[test]
    fn replies_in_fences_or_after_reasoning_are_parsed() {
        let reply =
            "<think>hm</think>\n```json\n{\"facts\": [\"Користувач має кота Мурчика.\"]}\n```";
        assert_eq!(
            parse_suggestions(reply),
            Some(vec!["Користувач має кота Мурчика.".to_owned()])
        );
        assert_eq!(
            parse_suggestions("Here you go: {\"facts\": [\" Likes tea. \"]} Done."),
            Some(vec!["Likes tea.".to_owned()])
        );
    }

    #[test]
    fn parsing_bounds_items() {
        let seven: Vec<String> = (1..=7).map(|n| format!("Fact number {n}.")).collect();
        let reply = serde_json::json!({ "facts": seven }).to_string();
        assert_eq!(
            parse_suggestions(&reply).expect("facts").len(),
            MAX_SUGGESTIONS
        );
        let reply =
            serde_json::json!({ "facts": ["ab", "x".repeat(301), 42, "Has a dog."] }).to_string();
        assert_eq!(
            parse_suggestions(&reply),
            Some(vec!["Has a dog.".to_owned()])
        );
        assert_eq!(parse_suggestions("not json"), None);
        assert_eq!(parse_suggestions("{\"answer\": 1}"), None);
        assert_eq!(parse_suggestions("{\"facts\": []}"), Some(vec![]));
    }
}
