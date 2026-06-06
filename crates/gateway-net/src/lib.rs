#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::time::Duration;
use url::Url;

const DEFAULT_ALLOWED_HOSTS: &[&str] = &["api.openai.com", "auth.openai.com", "chatgpt.com"];
const DENIED_HOST_SUFFIXES: &[&str] = &["anthropic.com", "claude.ai"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetworkPolicyError {
    #[error("invalid outbound URL `{url}`: {message}")]
    InvalidUrl { url: String, message: String },
    #[error("outbound network call to `{host}` is blocked: Anthropic endpoints are forbidden")]
    AnthropicEndpointBlocked { host: String },
    #[error("outbound network call to `{host}` is blocked: host is not in gateway allowlist")]
    HostNotAllowed { host: String },
    #[error("outbound network call with scheme `{scheme}` is blocked: only http/https are allowed")]
    SchemeNotAllowed { scheme: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayNetworkPolicy {
    allowed_hosts: BTreeSet<String>,
}

impl Default for GatewayNetworkPolicy {
    fn default() -> Self {
        let mut policy = Self::new(DEFAULT_ALLOWED_HOSTS.iter().copied());
        if let Ok(raw) = std::env::var("GATEWAY_ALLOWED_OUTBOUND_HOSTS") {
            policy.extend_allowed_hosts(raw.split(','));
        }
        policy
    }
}

impl GatewayNetworkPolicy {
    #[must_use]
    pub fn new<I, S>(hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut policy = Self {
            allowed_hosts: BTreeSet::new(),
        };
        policy.extend_allowed_hosts(hosts);
        policy
    }

    pub fn extend_allowed_hosts<I, S>(&mut self, hosts: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for host in hosts {
            let normalized = normalize_host(host.as_ref());
            if normalized.is_empty() || is_denied_host(&normalized) {
                continue;
            }
            self.allowed_hosts.insert(normalized);
        }
    }

    /// # Errors
    ///
    /// Returns an error when the URL is malformed, uses a non-HTTP scheme, targets Anthropic,
    /// or targets a host outside the allowlist.
    pub fn check_url_str(&self, url: &str) -> Result<(), NetworkPolicyError> {
        let parsed = Url::parse(url).map_err(|error| NetworkPolicyError::InvalidUrl {
            url: url.to_string(),
            message: error.to_string(),
        })?;
        self.check_url(&parsed)
    }

    /// # Errors
    ///
    /// Returns an error when the URL uses a non-HTTP scheme, targets Anthropic, or targets a host
    /// outside the allowlist.
    pub fn check_url(&self, url: &Url) -> Result<(), NetworkPolicyError> {
        let scheme = url.scheme();
        if !matches!(scheme, "http" | "https") {
            return Err(NetworkPolicyError::SchemeNotAllowed {
                scheme: scheme.to_string(),
            });
        }

        let host =
            url.host_str()
                .map(normalize_host)
                .ok_or_else(|| NetworkPolicyError::InvalidUrl {
                    url: url.to_string(),
                    message: "missing host".to_string(),
                })?;

        if is_denied_host(&host) {
            return Err(NetworkPolicyError::AnthropicEndpointBlocked { host });
        }

        if is_localhost(&host) || self.allowed_hosts.contains(&host) {
            return Ok(());
        }

        Err(NetworkPolicyError::HostNotAllowed { host })
    }
}

#[derive(Clone)]
pub struct GatewayHttpClient {
    http: reqwest::Client,
    policy: GatewayNetworkPolicy,
}

impl Default for GatewayHttpClient {
    fn default() -> Self {
        Self::new(GatewayNetworkPolicy::default())
    }
}

impl GatewayHttpClient {
    /// # Panics
    ///
    /// Panics if `reqwest` cannot build the client with the gateway redirect policy.
    #[must_use]
    pub fn new(policy: GatewayNetworkPolicy) -> Self {
        let redirect_policy = policy.clone();
        let default_redirect_policy = reqwest::redirect::Policy::default();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if let Err(error) = redirect_policy.check_url(attempt.url()) {
                    return attempt.error(error);
                }
                default_redirect_policy.redirect(attempt)
            }))
            .build()
            .expect("gateway HTTP client should build");

        Self { http, policy }
    }

    #[must_use]
    pub fn policy(&self) -> &GatewayNetworkPolicy {
        &self.policy
    }

    /// # Errors
    ///
    /// Returns an error if the target URL violates gateway outbound network policy.
    pub fn get(&self, url: &str) -> Result<GatewayRequestBuilder, NetworkPolicyError> {
        self.policy.check_url_str(url)?;
        Ok(GatewayRequestBuilder {
            inner: self.http.get(url),
        })
    }

    /// # Errors
    ///
    /// Returns an error if the target URL violates gateway outbound network policy.
    pub fn post(&self, url: &str) -> Result<GatewayRequestBuilder, NetworkPolicyError> {
        self.policy.check_url_str(url)?;
        Ok(GatewayRequestBuilder {
            inner: self.http.post(url),
        })
    }
}

pub struct GatewayRequestBuilder {
    inner: reqwest::RequestBuilder,
}

impl GatewayRequestBuilder {
    #[must_use]
    pub fn header(self, key: &'static str, value: &str) -> Self {
        Self {
            inner: self.inner.header(key, value),
        }
    }

    #[must_use]
    pub fn body(self, body: impl Into<reqwest::Body>) -> Self {
        Self {
            inner: self.inner.body(body),
        }
    }

    #[must_use]
    pub fn timeout(self, timeout: Duration) -> Self {
        Self {
            inner: self.inner.timeout(timeout),
        }
    }

    #[must_use]
    pub fn json<T: serde::Serialize + ?Sized>(self, json: &T) -> Self {
        Self {
            inner: self.inner.json(json),
        }
    }

    #[must_use]
    pub fn form<T: serde::Serialize + ?Sized>(self, form: &T) -> Self {
        Self {
            inner: self.inner.form(form),
        }
    }

    /// # Errors
    ///
    /// Returns an error if the HTTP request fails at the transport layer.
    pub async fn execute(self) -> Result<reqwest::Response, reqwest::Error> {
        self.inner.send().await
    }
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_matches(|ch| ch == '[' || ch == ']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn is_denied_host(host: &str) -> bool {
    DENIED_HOST_SUFFIXES
        .iter()
        .any(|denied| host == *denied || host.ends_with(&format!(".{denied}")))
}

fn is_localhost(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn blocks_anthropic_hosts_even_when_configured_as_allowed() {
        let policy = GatewayNetworkPolicy::new(["api.anthropic.com", "anthropic.com"]);
        let error = policy
            .check_url_str("https://api.anthropic.com/v1/messages")
            .expect_err("anthropic must be blocked");
        assert!(matches!(
            error,
            NetworkPolicyError::AnthropicEndpointBlocked { .. }
        ));
    }

    #[test]
    fn blocks_claude_hosts() {
        let policy = GatewayNetworkPolicy::default();
        let error = policy
            .check_url_str("https://console.claude.ai/")
            .expect_err("claude.ai must be blocked");
        assert!(matches!(
            error,
            NetworkPolicyError::AnthropicEndpointBlocked { .. }
        ));
    }

    #[test]
    fn allows_default_openai_and_chatgpt_hosts() {
        let policy = GatewayNetworkPolicy::default();
        policy
            .check_url_str("https://api.openai.com/v1/models")
            .expect("openai allowed");
        policy
            .check_url_str("https://auth.openai.com/oauth/token")
            .expect("auth allowed");
        policy
            .check_url_str("https://chatgpt.com/backend-api/codex/responses")
            .expect("chatgpt allowed");
    }

    #[test]
    fn allows_localhost_for_tests() {
        let policy = GatewayNetworkPolicy::default();
        policy
            .check_url_str("http://127.0.0.1:12345/backend-api/codex/responses")
            .expect("localhost allowed");
    }

    #[test]
    fn blocks_unconfigured_external_hosts() {
        let policy = GatewayNetworkPolicy::default();
        let error = policy
            .check_url_str("https://example.com/")
            .expect_err("unconfigured host must be blocked");
        assert!(matches!(error, NetworkPolicyError::HostNotAllowed { .. }));
    }

    #[tokio::test]
    async fn blocks_redirects_to_anthropic_hosts() {
        let server = match tiny_http::Server::http("127.0.0.1:0") {
            Ok(server) => server,
            Err(error) if error.to_string().contains("Operation not permitted") => return,
            Err(error) => panic!("server: {error}"),
        };
        let port = server.server_addr().to_ip().expect("tcp addr").port();
        let handle = std::thread::spawn(move || {
            let request = server.recv().expect("request");
            let location =
                tiny_http::Header::from_bytes("Location", "https://api.anthropic.com/v1/messages")
                    .expect("location header");
            let response = tiny_http::Response::empty(302).with_header(location);
            request.respond(response).expect("redirect response");
        });

        let error = GatewayHttpClient::default()
            .get(&format!("http://127.0.0.1:{port}/redirect"))
            .expect("localhost initial request allowed")
            .execute()
            .await
            .expect_err("redirect to anthropic must fail");
        assert!(error.is_redirect());
        handle.join().expect("server thread");
    }

    #[test]
    fn production_code_uses_gateway_http_client_for_sends() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("repo root");
        let crates_dir = repo_root.join("crates");
        let mut violations = Vec::new();
        collect_unchecked_http_uses(&crates_dir, &mut violations);
        assert!(
            violations.is_empty(),
            "unchecked outbound HTTP usage:\n{}",
            violations.join("\n")
        );
    }

    fn collect_unchecked_http_uses(dir: &Path, violations: &mut Vec<String>) {
        let entries = std::fs::read_dir(dir).expect("read dir");
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let path_str = path.to_string_lossy();
            if path_str.contains("/target/")
                || path_str.contains("/others/")
                || path_str.contains("/crates/gateway-net/")
            {
                continue;
            }
            if path.is_dir() {
                collect_unchecked_http_uses(&path, violations);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let content = std::fs::read_to_string(&path).expect("read source");
            for (index, line) in content.lines().enumerate() {
                if line.contains("reqwest::Client::new()")
                    || line.contains("reqwest::Client::builder()")
                    || line.contains("reqwest::get(")
                    || line.contains("reqwest::post(")
                    || line.contains(".send().await")
                    || line.trim() == ".send()"
                {
                    violations.push(format!("{}:{}", path.display(), index + 1));
                }
            }
        }
    }
}
