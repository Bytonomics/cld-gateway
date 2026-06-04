#![forbid(unsafe_code)]

use std::io::Write as _;

/// Gemini login flow: API key only (no OAuth)
pub fn login() -> Result<(), Box<dyn std::error::Error>> {
    let _api_key = prompt_gemini_api_key()?;
    // Note: Gemini API key persistence will be implemented when Gemini auth crate is added.
    // For now, placeholder error.
    Err("Gemini auth persistence not yet implemented.".into())
}

/// Prompt user to paste Gemini API key
fn prompt_gemini_api_key() -> Result<String, Box<dyn std::error::Error>> {
    println!("\nPaste your Gemini API key.\n");
    print!("GEMINI_API_KEY: ");
    std::io::stdout().flush()?;

    let mut key = String::new();
    std::io::stdin().read_line(&mut key)?;
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("empty API key".into());
    }
    Ok(key)
}
