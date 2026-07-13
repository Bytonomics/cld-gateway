// Crate: gateway-core
// Purpose: shared types/utilities used by all other crates.
// Allowed deps: none (keep this crate minimal and reusable).
// Not allowed: axum/http server code, auth file IO, backend networking, logging sinks.

#![forbid(unsafe_code)]

pub mod config;

pub const DEFAULT_BACKEND_MODEL: &str = "gpt-5.6-sol";
pub const UNSUPPORTED_BACKEND_MODELS: &[&str] = &["gpt-5.2", "gpt-5.3-codex", "gpt-5.4"];

#[derive(Debug)]
pub enum Error {
    InvalidInputError(&'static str),
    IoError(std::io::Error),
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::IoError(err)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[must_use]
pub fn format_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut current = error.source();

    while let Some(source) = current {
        let source_message = source.to_string();
        if !source_message.is_empty() {
            message.push_str(": caused by: ");
            message.push_str(&source_message);
        }
        current = source.source();
    }

    message
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RequestId(pub String);

/// Wrapper to make it harder to accidentally print secrets.
/// (We’ll still avoid Debug/Display on secret-bearing types later.)
#[derive(Clone)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    #[must_use]
    pub fn new(value: T) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &T {
        &self.0
    }
}

#[must_use]
pub fn ping() -> &'static str {
    "pong"
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::fmt;

    use super::format_error_chain;

    #[derive(Debug)]
    struct LeafError;

    impl fmt::Display for LeafError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("leaf failure")
        }
    }

    impl std::error::Error for LeafError {}

    #[derive(Debug)]
    struct RootError(LeafError);

    impl fmt::Display for RootError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("root failure")
        }
    }

    impl std::error::Error for RootError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn error_chain_includes_nested_sources() {
        let err = RootError(LeafError);
        assert_eq!(
            format_error_chain(&err),
            "root failure: caused by: leaf failure"
        );
        assert!(err.source().is_some());
    }
}
