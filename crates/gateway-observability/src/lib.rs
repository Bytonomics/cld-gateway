#![forbid(unsafe_code)]

#[must_use]
pub fn ping() -> &'static str {
    "pong"
}
