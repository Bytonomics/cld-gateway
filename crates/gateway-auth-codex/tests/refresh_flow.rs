#![forbid(unsafe_code)]

use std::path::Path;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture_auth_json() -> &'static str {
    r#"{
  "tokens": {
    "access_token": "eyJhbGciOiJub25lIn0.eyJleHAiOjEwMDB9.",
    "refresh_token": "refresh_test_token",
    "id_token": null,
    "account_id": "acct_test_123"
  }
}"#
}

// NOTE: In the Codex CLI sandbox environment, binding local TCP ports may be restricted, which
// breaks wiremock. Keep this test for local dev/CI environments where loopback binding works.
#[tokio::test]
async fn refresh_updates_file_and_returns_snapshot() {
    // This test requires binding a local TCP port for Wiremock. In some sandboxed environments
    // (including some Codex runs), loopback port binding is blocked. To keep `make check` green
    // everywhere while preserving the full integration test, only run it when explicitly enabled.
    //
    // Run with: `RUN_WIREMOCK=1 cargo test -p gateway-auth-codex --test refresh_flow`
    if std::env::var("RUN_WIREMOCK").is_err() {
        return;
    }

    let mock = MockServer::start().await;
    let token_url = Url::parse(&format!("{}/oauth/token", mock.uri())).expect("url parse");

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "eyJhbGciOiJub25lIn0.eyJleHAiOjIwMDAwMDAwMDB9.",
            "refresh_token": "refresh_rotated_token",
            "id_token": "id_token_new"
        })))
        .mount(&mock)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let auth_path = dir.path().join("auth.json");
    std::fs::write(&auth_path, fixture_auth_json()).expect("write fixture");

    let manager = gateway_auth_codex::CodexAuthManager::default().with_token_url(&token_url);
    let snap = manager
        .refresh_and_persist(Path::new(&auth_path))
        .await
        .expect("refresh");

    assert_eq!(snap.account_id, "acct_test_123");
    assert_eq!(snap.expires_at_unix_seconds, Some(2_000_000_000));

    let updated = std::fs::read_to_string(&auth_path).expect("read updated file");
    assert!(updated.contains("refresh_rotated_token"));
    assert!(updated.contains("id_token_new"));
}
