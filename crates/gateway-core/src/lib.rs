// Crate: gateway-core
// Purpose: shared types/utilities used by all other crates.
// Allowed deps: none (keep this crate minimal and reusable).
// Not allowed: axum/http server code, auth file IO, backend networking, logging sinks.

#![forbid(unsafe_code)]

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

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RequestId(pub String);

/// Wrapper to make it harder to accidentally print secrets.
/// (We’ll still avoid Debug/Display on secret-bearing types later.)
#[derive(Clone)]
pub struct Secret<T>(pub T);

impl<T> Secret<T> {
    pub fn expose(&self) -> &T {
        &self.0
    }
}

#[must_use]
pub fn ping() -> &'static str {
    "pong"
}
