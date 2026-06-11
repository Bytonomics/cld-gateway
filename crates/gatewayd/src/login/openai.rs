#![forbid(unsafe_code)]

use crate::tui_login;
use std::io::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginSelection {
    Chatgpt,
    ApiKey,
}

/// `OpenAI` login flow: show TUI menu with `ChatGPT` OAuth or API key options
pub async fn login() -> Result<(), Box<dyn std::error::Error>> {
    let selection = tui_login::login_menu()?;
    match selection {
        LoginSelection::Chatgpt => login_with_chatgpt().await,
        LoginSelection::ApiKey => login_with_api_key(),
    }
}

/// Sign in with `ChatGPT` OAuth (browser-based)
async fn login_with_chatgpt() -> Result<(), Box<dyn std::error::Error>> {
    match gateway_auth_codex::login::login_with_chatgpt_and_write_default_auth_json().await {
        Ok(()) => {
            println!("\nLogin successful.\n");
            Ok(())
        }
        Err(err) => {
            eprintln!("\nLogin failed: {err}\n");
            Err(err.into())
        }
    }
}

/// Sign in with `OpenAI` API key (prompt-based)
fn login_with_api_key() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = prompt_api_key()?;
    gateway_auth_codex::write_openai_api_key_default_path(&api_key)?;
    println!("\nAPI key saved.\n");
    Ok(())
}

/// Prompt user to paste `OpenAI` API key
fn prompt_api_key() -> Result<String, Box<dyn std::error::Error>> {
    println!(
        "\nPaste your OpenAI API key. This stores your key for OpenAI-backed auth flows; /v1/models now reads ~/.claude_gateway/settings.json.\n"
    );
    print!("OPENAI_API_KEY: ");
    std::io::stdout().flush()?;

    let mut key = String::new();
    std::io::stdin().read_line(&mut key)?;
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("empty API key".into());
    }
    Ok(key)
}
