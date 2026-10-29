//! Native tool transport contracts; no tool execution or live model required.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use jarvis_ai::{
    AiError, AiProvider, ChatMessage, GenerationOptions, HttpProviderFactory, ProviderBackend,
    ProviderConfig, ProviderFactory, ProviderId, ToolChatRequest, ToolDefinition, ToolMessage,
};
use serde_json::{Value, json};
use support::{MockServer, Reply};

fn provider(server: &MockServer, ollama: bool) -> Arc<dyn AiProvider> {
    HttpProviderFactory::new()
        .create(
            &ProviderConfig {
                native_tool_models: vec![],
                id: ProviderId::new("tools-test"),
                backend: if ollama {
                    ProviderBackend::Ollama
                } else {
                    ProviderBackend::OpenAiCompatible
                },
                display_name: "Tools test".into(),
                base_url: server.url(if ollama { "" } else { "/v1" }),
                enabled: true,
            },
            Some("test-key"),
        )
        .unwrap()
}

fn request() -> ToolChatRequest {
    ToolChatRequest {
        model: "test-model".into(),
        messages: vec![ToolMessage::Chat(ChatMessage::user("Read a file"))],
        tools: vec![ToolDefinition {
            name: "read_file".into(),
            description: "Read a workspace file".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        }],
        options: GenerationOptions {
            temperature: Some(0.5),
            max_output_tokens: Some(64),
        },
    }
}

fn response(ollama: bool, arguments: Value) -> Value {
    if ollama {
        json!({"done":true,"done_reason":"stop","message":{"role":"assistant","content":"",
            "tool_calls":[{"function":{"name":"read_file","arguments":arguments}}]}})
    } else {
        json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
            "tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":arguments.to_string()}}]}}]})
    }
}

#[tokio::test]
async fn native_calls_and_results_round_trip_with_provider_specific_fields() {
    for ollama in [false, true] {
        let server = MockServer::start(move |_| {
            Reply::Json(200, response(ollama, json!({"path":"дані.txt"})))
        })
        .await;
        let provider = provider(&server, ollama);
        let answer = provider.chat_with_tools(request()).await.unwrap();
        assert_eq!(answer.calls.len(), 1);
        let call = answer.calls[0].clone();
        assert_eq!(call.name, "read_file");
        assert_eq!(call.arguments, json!({"path":"дані.txt"}));
        let mut followup = request();
        followup.messages.extend([
            ToolMessage::Assistant {
                content: answer.content,
                calls: answer.calls,
            },
            ToolMessage::Result {
                call: call.clone(),
                content: "File contents".into(),
            },
        ]);
        provider.chat_with_tools(followup).await.unwrap();
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        let sent = requests[1].json();
        assert_eq!(requests[1].method, "POST");
        assert_eq!(sent["stream"], false);
        assert_eq!(
            sent["tools"][0]["function"]["parameters"],
            request().tools[0].parameters
        );
        assert_eq!(sent["messages"][2]["role"], "tool");
        assert_eq!(sent["messages"][2]["content"], "File contents");
        if ollama {
            assert_eq!(requests[1].path, "/api/chat");
            assert_eq!(sent["options"]["num_predict"], 64);
            assert_eq!(sent["options"]["temperature"], 0.5);
            assert_eq!(
                sent["messages"][1]["tool_calls"][0]["function"]["arguments"],
                call.arguments
            );
            assert_eq!(sent["messages"][2]["tool_name"], call.name);
        } else {
            assert_eq!(requests[1].path, "/v1/chat/completions");
            assert_eq!(
                requests[1].authorization.as_deref(),
                Some("Bearer test-key")
            );
            assert_eq!(sent["max_tokens"], 64);
            assert_eq!(sent["temperature"], 0.5);
            assert_eq!(sent["parallel_tool_calls"], false);
            assert_eq!(
                sent["messages"][1]["tool_calls"][0]["function"]["arguments"],
                call.arguments.to_string()
            );
            assert_eq!(sent["messages"][2]["tool_call_id"], call.id);
        }
    }
}

#[tokio::test]
async fn final_text_is_returned_without_calls() {
    for ollama in [false, true] {
        let server = MockServer::start(move |_| Reply::Json(200, if ollama {
            json!({"done":true,"message":{"role":"assistant","content":"Готово"}})
        } else {
            json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"Готово"}}]})
        })).await;
        let result = provider(&server, ollama)
            .chat_with_tools(request())
            .await
            .unwrap();
        assert_eq!(result.content, "Готово");
        assert!(result.calls.is_empty());
    }
}

#[tokio::test]
async fn providers_without_native_tools_report_unsupported() {
    assert!(matches!(
        jarvis_ai::EchoProvider::new()
            .chat_with_tools(request())
            .await,
        Err(AiError::Unsupported {
            operation: "tool calling",
            ..
        })
    ));
}

#[tokio::test]
async fn invalid_json_arguments_and_duplicate_call_ids_are_rejected() {
    let valid = response(false, json!({}));
    let mut invalid_arguments = valid.clone();
    invalid_arguments["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] =
        json!("{");
    let mut duplicate = valid.clone();
    let call = valid["choices"][0]["message"]["tool_calls"][0].clone();
    duplicate["choices"][0]["message"]["tool_calls"] = json!([call, call]);
    for body in [invalid_arguments, duplicate] {
        let server = MockServer::start(move |_| Reply::Json(200, body.clone())).await;
        assert!(matches!(
            provider(&server, false).chat_with_tools(request()).await,
            Err(AiError::Protocol(_))
        ));
    }
}

#[tokio::test]
async fn incomplete_and_malformed_calls_are_rejected_without_retry() {
    for ollama in [false, true] {
        let mut incomplete = response(ollama, json!({}));
        if ollama {
            incomplete["done"] = json!(false);
        } else {
            incomplete["choices"][0]["finish_reason"] = json!("length");
        }
        let mut wrong_type = response(ollama, json!({}));
        let message = if ollama {
            &mut wrong_type["message"]
        } else {
            &mut wrong_type["choices"][0]["message"]
        };
        message["tool_calls"] = json!({});
        for body in [
            response(ollama, json!([])),
            incomplete,
            wrong_type,
            json!({"error":"bad"}),
        ] {
            let server = MockServer::start(move |_| Reply::Json(200, body.clone())).await;
            assert!(matches!(
                provider(&server, ollama).chat_with_tools(request()).await,
                Err(AiError::Protocol(_))
            ));
            assert_eq!(server.requests().len(), 1);
        }
    }
}

#[tokio::test]
async fn server_errors_are_not_retried() {
    for ollama in [false, true] {
        let server =
            MockServer::start(|_| Reply::Json(500, json!({"error":"runner unavailable"}))).await;
        assert!(
            provider(&server, ollama)
                .chat_with_tools(request())
                .await
                .is_err()
        );
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn invalid_tool_definitions_fail_before_network_access() {
    for ollama in [false, true] {
        let server = MockServer::start(|_| Reply::Json(500, json!({}))).await;
        for name in ["", "plugin.tool", &"a".repeat(65)] {
            let mut input = request();
            input.tools[0].name = name.into();
            assert!(matches!(
                provider(&server, ollama).chat_with_tools(input).await,
                Err(AiError::InvalidRequest(_))
            ));
        }
        let mut input = request();
        input.tools.push(input.tools[0].clone());
        assert!(matches!(
            provider(&server, ollama).chat_with_tools(input).await,
            Err(AiError::InvalidRequest(_))
        ));
        assert!(server.requests().is_empty());
    }
}

#[tokio::test]
async fn dropping_native_generation_closes_the_connection() {
    for ollama in [false, true] {
        let server = MockServer::start(|_| Reply::Stream {
            content_type: "application/json",
            chunks: vec![(Duration::ZERO, b"{".to_vec())],
            hang: true,
        })
        .await;
        let provider = provider(&server, ollama);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(200),
                provider.chat_with_tools(request())
            )
            .await
            .is_err()
        );
        assert!(server.wait_for_disconnect().await);
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn oversized_native_response_is_rejected() {
    let body = format!("{{\"padding\":\"{}\"}}", "x".repeat(4 * 1024 * 1024));
    let server =
        MockServer::start(move |_| Reply::stream("application/json", &body, 64 * 1024)).await;
    for ollama in [false, true] {
        assert!(matches!(
            provider(&server, ollama).chat_with_tools(request()).await,
            Err(AiError::Protocol(_))
        ));
    }
}
