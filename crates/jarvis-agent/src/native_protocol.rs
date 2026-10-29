//! Bridge between native provider messages and the provider-independent
//! journal. Only normalized actions reach the orchestrator's policy path.

use jarvis_ai::{ChatMessage, ChatRole, ToolCall, ToolChatResponse, ToolDefinition, ToolMessage};
use jarvis_tools::ToolSpec;
use serde_json::json;

use crate::protocol::{Reply, parse_reply};

pub(crate) struct NativeTurn {
    pub specs: Vec<ToolSpec>,
    names: Vec<String>,
}

impl NativeTurn {
    pub fn new(tools: &[ToolSpec]) -> Self {
        let names = tools.iter().map(|t| t.name.clone()).collect();
        let specs = tools
            .iter()
            .enumerate()
            .map(|(index, tool)| {
                let mut spec = tool.clone();
                // Plugin names may contain dots or exceed the provider's name
                // limit. Aliases are scoped to this request and resolved before
                // policy evaluation; original names are persisted in the journal.
                spec.name = format!("jarvis_tool_{index}");
                spec.description = format!("{}: {}", tool.name, tool.description);
                spec
            })
            .collect();
        Self { specs, names }
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.specs
            .iter()
            .map(|spec| ToolDefinition {
                name: spec.name.clone(),
                description: spec.description.clone(),
                parameters: spec.parameters.clone(),
            })
            .collect()
    }

    pub fn schema_chars(&self) -> usize {
        self.specs
            .iter()
            .map(|spec| {
                spec.parameters.to_string().len() + spec.description.len() + spec.name.len()
            })
            .sum()
    }

    pub fn messages(&self, messages: Vec<ChatMessage>) -> Vec<ToolMessage> {
        let mut result = Vec::new();
        let mut messages = messages.into_iter().peekable();
        while let Some(message) = messages.next() {
            if message.role == ChatRole::Assistant
                && messages
                    .peek()
                    .is_some_and(|next| next.role == ChatRole::User)
                && let Ok(Reply::Action {
                    thought,
                    tool,
                    input,
                }) = parse_reply(&message.content)
            {
                let index = result.len();
                let name = self
                    .names
                    .iter()
                    .position(|name| name == &tool)
                    .map(|i| self.specs[i].name.clone())
                    // Removed tools remain historical data, never advertised
                    // as callable tools in this request.
                    .unwrap_or_else(|| format!("historical_tool_{index}"));
                let call = ToolCall {
                    id: format!("history_{index}"),
                    name,
                    arguments: input,
                };
                result.push(ToolMessage::Assistant {
                    content: thought.unwrap_or_default(),
                    calls: vec![call.clone()],
                });
                if let Some(observation) = messages.next() {
                    result.push(ToolMessage::Result {
                        call,
                        content: observation.content,
                    });
                }
            } else if message.role == ChatRole::Assistant
                && let Ok(Reply::Final { answer, .. }) = parse_reply(&message.content)
            {
                result.push(ToolMessage::Chat(ChatMessage::new(
                    ChatRole::Assistant,
                    answer,
                )));
            } else {
                result.push(ToolMessage::Chat(message));
            }
        }
        result
    }

    pub fn normalize(&self, response: ToolChatResponse) -> String {
        let invalid = || {
            json!({"invalid_native_reply": "Use exactly one advertised function with object arguments, or a non-empty final answer. No action was executed."}).to_string()
        };
        match response.calls.as_slice() {
            [] if !response.content.trim().is_empty() => {
                json!({"final": response.content}).to_string()
            }
            [call] if call.arguments.is_object() => {
                let Some(index) = self.specs.iter().position(|spec| spec.name == call.name) else {
                    return invalid();
                };
                json!({"tool": self.names[index], "input": call.arguments,
                    "thought": if response.content.is_empty() { None } else { Some(response.content) }
                }).to_string()
            }
            _ => invalid(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_aliases_round_trip_and_removed_tools_stay_historical() {
        let mut specs = jarvis_tools::ToolRegistry::builtin().specs();
        specs.truncate(1);
        specs[0].name = format!("plugin.{}", "long_name".repeat(10));
        specs[0].plugin = Some("example".into());
        let turn = NativeTurn::new(&specs);
        let name = &turn.definitions()[0].name;
        assert_eq!(name, "jarvis_tool_0");
        let reply = turn.normalize(ToolChatResponse {
            content: String::new(),
            calls: vec![ToolCall {
                id: "provider_id".into(),
                name: name.clone(),
                arguments: json!({"path":"a.txt"}),
            }],
        });
        assert!(
            matches!(parse_reply(&reply), Ok(Reply::Action { tool, .. }) if tool == specs[0].name)
        );
        let history = vec![
            ChatMessage::new(ChatRole::Assistant, reply),
            ChatMessage::user("saved output"),
        ];
        let removed = NativeTurn::new(&[]);
        assert!(removed.definitions().is_empty());
        let messages = removed.messages(history);
        let ToolMessage::Assistant { calls, .. } = &messages[0] else {
            panic!("assistant call")
        };
        let ToolMessage::Result { call, content } = &messages[1] else {
            panic!("result")
        };
        assert_eq!(calls, std::slice::from_ref(call));
        assert_eq!(content, "saved output");
        assert!(
            parse_reply(&removed.normalize(ToolChatResponse {
                content: String::new(),
                calls: calls.clone()
            }))
            .is_err()
        );
    }

    #[test]
    fn verifier_feedback_and_final_answers_remain_text_messages() {
        let turn = NativeTurn::new(&[]);
        let messages = turn.messages(vec![
            ChatMessage::new(ChatRole::Assistant, r#"{"final":"an answer"}"#),
            ChatMessage::user("Check this again"),
        ]);
        assert_eq!(
            messages,
            vec![
                ToolMessage::Chat(ChatMessage::new(ChatRole::Assistant, "an answer")),
                ToolMessage::Chat(ChatMessage::user("Check this again")),
            ]
        );
    }
}
