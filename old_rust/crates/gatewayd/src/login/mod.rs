#![forbid(unsafe_code)]

use crate::Vendor;

pub mod gemini;
pub mod openai;

/// Dispatch to vendor-specific login implementation
pub async fn run_login(vendor: Vendor) -> Result<(), Box<dyn std::error::Error>> {
    match vendor {
        Vendor::OpenAI => openai::login().await,
        Vendor::Gemini => gemini::login(),
    }
}
