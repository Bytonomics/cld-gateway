#![forbid(unsafe_code)]

use std::path::Path;

fn fixture_path() -> &'static Path {
    Path::new("tests/fixtures/auth.json")
}

#[test]
fn loads_snapshot_and_extracts_exp() {
    let snap = gateway_auth_codex::load_codex_auth(fixture_path()).expect("load snapshot");
    assert_eq!(snap.account_id, "acct_test_123");
    assert!(snap.has_access_token);
    assert!(snap.has_refresh_token);
    assert_eq!(snap.expires_at_unix_seconds, Some(2_000_000_000));
}

#[test]
fn errors_do_not_leak_tokens() {
    let err = gateway_auth_codex::load_codex_auth(Path::new("does-not-exist"))
        .expect_err("expected io error");
    let s = err.to_string();
    assert!(
        !s.contains("eyJ"),
        "error message should not contain token-like substrings"
    );
}
