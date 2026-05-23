#![forbid(unsafe_code)]

use gateway_core::Secret;
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
            "model": "gpt-5.2",
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
        model: "gpt-5.2".to_string(),
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
        store: false,
        stream: true,
        include: Vec::new(),
        max_output_tokens: None,
        temperature: None,
        top_p: None,
        client_metadata: None,
    };

    let res = client.send(&req).await.expect("send");
    assert_eq!(res.status, 200);
    assert_eq!(res.body, "ok");
}
