//! Native, non-streaming tool turns. Returned calls are proposals only: the
//! caller must validate permissions and arguments before executing them.

use serde_json::{Value, json};

use crate::{AiError, ChatMessage, GenerationOptions};

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Provider call ID, or a request-local ID synthesized for Ollama.
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolMessage {
    Chat(ChatMessage),
    Assistant {
        content: String,
        calls: Vec<ToolCall>,
    },
    Result {
        call: ToolCall,
        content: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolChatRequest {
    pub model: String,
    pub messages: Vec<ToolMessage>,
    pub tools: Vec<ToolDefinition>,
    pub options: GenerationOptions,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolChatResponse {
    pub content: String,
    pub calls: Vec<ToolCall>,
}

#[derive(Clone, Copy)]
pub(crate) enum Dialect {
    OpenAi,
    Ollama,
}

impl ToolChatRequest {
    pub(crate) fn body(
        &self,
        dialect: Dialect,
        chat_message: fn(&ChatMessage) -> Value,
    ) -> Result<Value, AiError> {
        let mut names = std::collections::BTreeSet::new();
        for tool in &self.tools {
            if tool.name.is_empty()
                || tool.name.len() > 64
                || !tool
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                || !names.insert(&tool.name)
                || !tool.parameters.is_object()
            {
                return Err(AiError::InvalidRequest(
                    "invalid or duplicate tool definition".into(),
                ));
            }
        }
        let messages = self
            .messages
            .iter()
            .map(|message| match message {
                ToolMessage::Chat(message) => chat_message(message),
                ToolMessage::Assistant { content, calls } => json!({
                    "role": "assistant", "content": content,
                    "tool_calls": calls.iter().map(|call| match dialect {
                        Dialect::OpenAi => json!({"id": call.id, "type": "function", "function": {
                            "name": call.name, "arguments": call.arguments.to_string()
                        }}),
                        Dialect::Ollama => json!({"function": {
                            "name": call.name, "arguments": call.arguments
                        }}),
                    }).collect::<Vec<_>>()
                }),
                ToolMessage::Result { call, content } => match dialect {
                    Dialect::OpenAi => {
                        json!({"role": "tool", "tool_call_id": call.id, "content": content})
                    }
                    Dialect::Ollama => {
                        json!({"role": "tool", "tool_name": call.name, "content": content})
                    }
                },
            })
            .collect::<Vec<_>>();
        let mut body = json!({
            "model": self.model, "messages": messages, "stream": false,
            "tools": self.tools.iter().map(|tool| json!({"type": "function", "function": {
                "name": tool.name, "description": tool.description, "parameters": tool.parameters
            }})).collect::<Vec<_>>()
        });
        match dialect {
            Dialect::OpenAi => {
                body["parallel_tool_calls"] = json!(false);
                if let Some(value) = self.options.temperature {
                    body["temperature"] = json!(value);
                }
                if let Some(value) = self.options.max_output_tokens {
                    body["max_tokens"] = json!(value);
                }
            }
            Dialect::Ollama => {
                let mut options = json!({});
                if let Some(value) = self.options.temperature {
                    options["temperature"] = json!(value);
                }
                if let Some(value) = self.options.max_output_tokens {
                    options["num_predict"] = json!(value);
                }
                body["options"] = options;
            }
        }
        Ok(body)
    }
}

/// Bound the entire non-streaming response, including chunked bodies. No
/// generation retries; dropping this future closes the in-flight response.
pub(crate) async fn read_response(
    mut response: reqwest::Response,
    dialect: Dialect,
) -> Result<ToolChatResponse, AiError> {
    const MAX_BYTES: usize = 4 * 1024 * 1024;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| crate::http::map_transport_error(&err))?
    {
        if chunk.len() > MAX_BYTES.saturating_sub(bytes.len()) {
            return Err(AiError::Protocol("tool response exceeds 4 MiB".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| AiError::Protocol("invalid tool response JSON".into()))?;
    parse_response(&value, dialect)
}

fn parse_response(value: &Value, dialect: Dialect) -> Result<ToolChatResponse, AiError> {
    let invalid = || AiError::Protocol("invalid or incomplete tool response".into());
    if value.get("error").is_some_and(|v| !v.is_null()) {
        return Err(invalid());
    }
    let message = match dialect {
        Dialect::OpenAi => {
            let choices = value["choices"].as_array().ok_or_else(invalid)?;
            if choices.len() != 1 {
                return Err(invalid());
            }
            let choice = &choices[0];
            if !matches!(
                choice["finish_reason"].as_str(),
                Some("stop" | "tool_calls")
            ) {
                return Err(invalid());
            }
            &choice["message"]
        }
        Dialect::Ollama => {
            if value["done"] != true
                || !matches!(
                    value.get("done_reason").and_then(Value::as_str),
                    None | Some("stop")
                )
            {
                return Err(invalid());
            }
            &value["message"]
        }
    };
    if message["role"] != "assistant" {
        return Err(invalid());
    }
    let content = match message.get("content") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        _ => return Err(invalid()),
    };
    let raw_calls = match message.get("tool_calls") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(calls)) => calls.as_slice(),
        _ => return Err(invalid()),
    };
    let mut ids = std::collections::BTreeSet::new();
    let calls = raw_calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let function = &call["function"];
            let name = function["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(invalid)?
                .to_owned();
            let (id, arguments) = match dialect {
                Dialect::OpenAi => {
                    if call["type"] != "function" {
                        return Err(invalid());
                    }
                    let id = call["id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(invalid)?
                        .to_owned();
                    let arguments =
                        serde_json::from_str(function["arguments"].as_str().ok_or_else(invalid)?)
                            .map_err(|_| invalid())?;
                    (id, arguments)
                }
                Dialect::Ollama => (
                    format!("ollama_call_{index}"),
                    function["arguments"].clone(),
                ),
            };
            if !arguments.is_object() || !ids.insert(id.clone()) {
                return Err(invalid());
            }
            Ok(ToolCall {
                id,
                name,
                arguments,
            })
        })
        .collect::<Result<Vec<_>, AiError>>()?;
    if calls.is_empty() && content.trim().is_empty() {
        return Err(invalid());
    }
    Ok(ToolChatResponse { content, calls })
}
