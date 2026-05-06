#![forbid(unsafe_code)]

#[test]
fn refresh_response_parses_required_fields() {
    let body = r#"{
        "access_token": "a",
        "refresh_token": "b",
        "id_token": "c"
    }"#;

    let parsed: gateway_auth_codex::oauth::RefreshResponse =
        serde_json::from_str(body).expect("parse refresh response");
    assert_eq!(parsed.access_token.as_deref(), Some("a"));
    assert_eq!(parsed.refresh_token.as_deref(), Some("b"));
    assert_eq!(parsed.id_token.as_deref(), Some("c"));
}
