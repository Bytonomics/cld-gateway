#![forbid(unsafe_code)]

use gateway_core::{DEFAULT_BACKEND_MODEL, Secret};
use wiremock::matchers::{body_json, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn sends_required_headers_and_minimal_body() {
    if std::env::var("RUN_WIREMOCK").is_err() {
        return;
    }

    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .and(header_exists("authorization"))
        .and(header("chatgpt-account-id", "acct_test_123"))
        .and(header("openai-beta", "responses=experimental"))
        .and(header("originator", "codex_cli_rs"))
        .and(header("accept", "text/event-stream"))
        .and(body_json(serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "instructions": "You are a helpful assistant.",
            "input": [
                {
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": "hello" }
                    ]
                }
            ],
            "tools": [],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "store": false,
            "stream": true,
            "include": []
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let base_url = url::Url::parse(&server.uri()).expect("server url parse");
    let client =
        gateway_backend_codex::client::CodexBackendClient::default().with_base_url(&base_url);

    let req = gateway_backend_codex::types::CodexBackendRequest {
        access_token: Secret::new("access_test".to_string()),
        account_id: "acct_test_123".to_string(),
        model: DEFAULT_BACKEND_MODEL.to_string(),
        instructions: "You are a helpful assistant.".to_string(),
        input: vec![serde_json::json!({
            "role": "user",
            "content": [{ "type": "input_text", "text": "hello" }]
        })],
        tools: Vec::new(),
        tool_choice: "auto".to_string(),
        parallel_tool_calls: true,
        text: None,
        reasoning: None,
        previous_response_id: None,
        store: false,
        stream: true,
        include: Vec::new(),
        service_tier: None,
        client_metadata: None,
    };

    let res = client.send(&req).await.expect("send");
    assert_eq!(res.status, 200);
    assert_eq!(res.body, "ok");
}

#[tokio::test]
async fn sends_service_tier_when_configured() {
    if std::env::var("RUN_WIREMOCK").is_err() {
        return;
    }

    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .and(header_exists("authorization"))
        .and(body_json(serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "instructions": "You are a helpful assistant.",
            "input": [],
            "tools": [],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "store": false,
            "stream": true,
            "include": [],
            "service_tier": "priority"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let base_url = url::Url::parse(&server.uri()).expect("server url parse");
    let client =
        gateway_backend_codex::client::CodexBackendClient::default().with_base_url(&base_url);

    let req = gateway_backend_codex::types::CodexBackendRequest {
        access_token: Secret::new("access_test".to_string()),
        account_id: "acct_test_123".to_string(),
        model: DEFAULT_BACKEND_MODEL.to_string(),
        instructions: "You are a helpful assistant.".to_string(),
        input: Vec::new(),
        tools: Vec::new(),
        tool_choice: "auto".to_string(),
        parallel_tool_calls: true,
        text: None,
        reasoning: None,
        previous_response_id: None,
        store: false,
        stream: true,
        include: Vec::new(),
        service_tier: Some("priority".to_string()),
        client_metadata: None,
    };

    let res = client.send(&req).await.expect("send");
    assert_eq!(res.status, 200);
    assert_eq!(res.body, "ok");
}

#[tokio::test]
async fn sends_previous_response_id_when_provided() {
    if std::env::var("RUN_WIREMOCK").is_err() {
        return;
    }

    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .and(body_json(serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "instructions": "You are a helpful assistant.",
            "input": [],
            "tools": [],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "previous_response_id": "resp_123",
            "store": false,
            "stream": true,
            "include": []
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let base_url = url::Url::parse(&server.uri()).expect("server url parse");
    let client =
        gateway_backend_codex::client::CodexBackendClient::default().with_base_url(&base_url);

    let req = gateway_backend_codex::types::CodexBackendRequest {
        access_token: Secret::new("access_test".to_string()),
        account_id: "acct_test_123".to_string(),
        model: DEFAULT_BACKEND_MODEL.to_string(),
        instructions: "You are a helpful assistant.".to_string(),
        input: Vec::new(),
        tools: Vec::new(),
        tool_choice: "auto".to_string(),
        parallel_tool_calls: true,
        text: None,
        reasoning: None,
        previous_response_id: Some("resp_123".to_string()),
        store: false,
        stream: true,
        include: Vec::new(),
        service_tier: None,
        client_metadata: None,
    };

    let res = client.send(&req).await.expect("send");
    assert_eq!(res.status, 200);
    assert_eq!(res.body, "ok");
}
