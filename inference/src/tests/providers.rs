use super::*;

// Wire shapes from provider docs and their SDK serializers, without calling providers:
// https://api-docs.deepseek.com/guides/anthropic_api/
// https://github.com/OpenRouterTeam/ai-sdk-provider/tree/main/src
// https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/provider/transform.ts
fn requests() -> Vec<(&'static str, Protocol, Value)> {
    vec![
        (
            "/deepseek/v1/messages",
            Protocol::Messages,
            json!({
                "model": "deepseek-flash", "max_tokens": 4096,
                "thinking": {"type": "enabled", "budget_tokens": 2048},
                "output_config": {"effort": "high"},
                "metadata": {"user_id": "local-session"},
                "system": "edit the code", "temperature": 1, "top_p": 0.95,
                "tools": [{"name": "edit", "description": "edit a file", "input_schema": {"type": "object"}}],
                "tool_choice": {"type": "auto", "disable_parallel_tool_use": false},
                "messages": [
                    {"role": "user", "content": "fix it"},
                    {"role": "assistant", "content": [
                        {"type": "thinking", "thinking": "inspect first", "signature": ""},
                        {"type": "tool_use", "id": "call_1", "name": "edit", "input": {"path": "main.rs"}}
                    ]},
                    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "done", "is_error": false}]}
                ]
            }),
        ),
        (
            "/opencode-go/v1/messages",
            Protocol::Messages,
            json!({
                "model": "claude-haiku-5-5", "max_tokens": 4096,
                "thinking": {"type": "adaptive", "display": "summarized", "block_binding": {"prefix_mismatch_behavior": "drop_block"}},
                "output_config": {"effort": "high"},
                "system": [{"type": "text", "text": "edit the code", "cache_control": {"type": "ephemeral", "ttl": "1h"}}],
                "tools": [{"name": "edit", "input_schema": {"type": "object"}}],
                "messages": [
                    {"role": "assistant", "content": [{"type": "thinking", "thinking": "plan", "signature": "opaque"}, {"type": "tool_use", "id": "call_1", "name": "edit", "input": {}}]},
                    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": [{"type": "text", "text": "done"}]}]}
                ]
            }),
        ),
        (
            "/opencode-go/v1/chat/completions",
            Protocol::Completions,
            json!({
                "model": "glm-5.3", "max_tokens": 4096, "reasoning_effort": "high",
                "thinking": {"type": "enabled", "clear_thinking": false},
                "chat_template_args": {"enable_thinking": true},
                "user": "local-session", "verbosity": "low", "temperature": 0.7,
                "tools": [{"type": "function", "function": {"name": "edit", "parameters": {"type": "object"}}}],
                "messages": [
                    {"role": "assistant", "content": null, "reasoning_content": "plan", "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "edit", "arguments": "{}"}}]},
                    {"role": "tool", "tool_call_id": "call_1", "content": "done"}
                ]
            }),
        ),
        (
            "/openrouter/v1/chat/completions",
            Protocol::Completions,
            json!({
                "model": "anthropic/claude-sonnet", "max_tokens": 4096,
                "reasoning": {"effort": "high", "enabled": true, "exclude": false},
                "usage": {"include": true}, "include_reasoning": true,
                "cache_control": {"type": "ephemeral"},
                "top_k": 40, "min_p": 0, "top_a": 0, "repetition_penalty": 1,
                "logprobs": true, "top_logprobs": 2, "logit_bias": {"42": -1},
                "tools": [{"type": "function", "function": {"name": "edit", "parameters": {"type": "object"}}}],
                "messages": [
                    {"role": "system", "content": [{"type": "text", "text": "edit the code", "cache_control": {"type": "ephemeral", "ttl": "5m"}}]},
                    {"role": "user", "content": [{"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}, "cache_control": {"type": "ephemeral"}}]},
                    {"role": "assistant", "content": null, "reasoning": "plan", "reasoning_details": [
                        {"type": "reasoning.summary", "summary": "plan", "id": null, "format": "anthropic-claude-v1", "index": 0},
                        {"type": "reasoning.text", "text": null, "signature": null, "format": null, "index": 1},
                        {"type": "reasoning.encrypted", "data": "opaque", "id": "reasoning_1", "index": 2}
                    ], "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "edit", "arguments": "{}"}}], "cache_control": {"type": "ephemeral"}},
                    {"role": "tool", "tool_call_id": "call_1", "content": "done", "cache_control": {"type": "ephemeral"}},
                    {"role": "assistant", "content": "finished", "reasoning_details": []}
                ]
            }),
        ),
    ]
}

#[tokio::test]
async fn provider_tuning_and_reasoning_history_survive_authorization_and_forwarding() {
    let origin = Origin::new(StatusCode::OK, r#"{"ok":true}"#).await;
    let requests = requests();
    let routes = requests
        .iter()
        .map(|(path, protocol, body)| {
            (
                (*path).into(),
                Route {
                    endpoint: Arc::new(Endpoint::for_test(
                        &origin.url,
                        "provider-secret",
                        Vec::new(),
                    )),
                    models: Arc::new(HashSet::from([body["model"].as_str().unwrap().to_owned()])),
                    protocol: *protocol,
                },
            )
        })
        .collect();
    let gateway = Arc::new(Gateway {
        policy: Policy::new(routes),
        client: Client::new(Duration::from_secs(5)).unwrap(),
        max_body: 1024 * 1024,
        max_response_body: 1024 * 1024,
    });
    for (index, (path, _, mut body)) in requests.into_iter().enumerate() {
        let reply = exchange(gateway.clone(), &request(path, &body.to_string())).await;
        assert!(
            reply.starts_with(b"HTTP/1.1 200"),
            "{path}: {}",
            String::from_utf8_lossy(&reply)
        );
        body["stream"] = json!(false);
        assert_eq!(origin.requests.lock().unwrap()[index].body, body, "{path}");
    }
}

#[test]
fn provider_option_nullability_matches_the_sdk() {
    for binding in [
        json!({}),
        json!({"prefix_mismatch_behavior": null}),
        json!({"prefix_mismatch_behavior": "error"}),
    ] {
        let mut body = messages();
        body["thinking"] = json!({"type": "enabled", "block_binding": binding});
        assert!(protocols::messages(&body).is_ok());
    }
    let mut body = completions();
    body["reasoning"] = json!({"effort": null, "exclude": null, "context": "all_turns"});
    body["messages"][0]["reasoning_details"] = json!(null);
    assert!(protocols::completions(&body).is_ok());
    body["reasoning"] = json!({"max_tokens": 2048, "enabled": true, "exclude": false});
    assert!(protocols::completions(&body).is_ok());
}

#[tokio::test]
async fn new_provider_fields_cannot_smuggle_capabilities_or_routing() {
    let origin = Origin::new(StatusCode::OK, "unused").await;
    let gateway = gateway(&origin.url, &origin.url, &origin.url);
    for (key, value) in [
        ("reasoning", json!({"mode": "pro"})),
        ("reasoning", json!({"effort": 1})),
        ("reasoning", json!({"enabled": "yes"})),
        ("reasoning", json!({"max_tokens": 0})),
        ("reasoning", json!({"tools": [{"type": "web_search"}]})),
        ("usage", json!({"include": "yes"})),
        ("usage", json!({"include": true, "url": "https://evil"})),
        (
            "chat_template_args",
            json!({"enable_thinking": true, "tools": []}),
        ),
        ("logit_bias", json!({"42": "yes"})),
        ("plugins", json!([{"id": "web"}])),
        ("models", json!(["unapproved-model"])),
        ("provider", json!({"order": ["unapproved"]})),
    ] {
        let mut body = completions();
        body[key] = value;
        let reply = exchange(
            gateway.clone(),
            &request("/opencode-go/v1/chat/completions", &body.to_string()),
        )
        .await;
        assert!(reply.starts_with(b"HTTP/1.1 400"), "accepted {key}: {body}");
    }
    for detail in [
        json!({"type": "reasoning.text", "text": {"url": "https://evil"}}),
        json!({"type": "reasoning.text", "text": "ok", "tools": []}),
        json!({"type": "reasoning.encrypted"}),
        json!({"type": "reasoning.summary", "summary": null}),
        json!({"type": "web_search", "data": "opaque"}),
    ] {
        let mut body = completions();
        body["messages"][0]["reasoning_details"] = json!([detail]);
        let reply = exchange(
            gateway.clone(),
            &request("/opencode-go/v1/chat/completions", &body.to_string()),
        )
        .await;
        assert!(reply.starts_with(b"HTTP/1.1 400"), "accepted {body}");
    }
    let mut body = messages();
    body["thinking"] = json!({"type": "enabled", "block_binding": {"prefix_mismatch_behavior": "drop_block", "url": "https://evil"}});
    let reply = exchange(
        gateway,
        &request("/opencode-go/v1/messages", &body.to_string()),
    )
    .await;
    assert!(reply.starts_with(b"HTTP/1.1 400"));
    assert!(origin.requests.lock().unwrap().is_empty());
}
