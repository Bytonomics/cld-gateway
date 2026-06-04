#![forbid(unsafe_code)]

/// Gemini login flow: not yet supported
pub fn login() -> Result<(), Box<dyn std::error::Error>> {
    Err("Gemini login is not yet supported. Please use 'cld-gateway login openai' for now.".into())
}
