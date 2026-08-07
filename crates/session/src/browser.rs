//! Browser-backed session bootstrap/refresh (feature `browser`).
//!
//! This is a fallback and session-management mechanism only — never on the
//! claim hot path. It opens a persistent Chromium profile via a Rust-native
//! CDP adapter, lets the owner log in manually, and never prints cookies.
//! No CAPTCHA-solving, stealth, or verification-bypass behavior lives here.

use std::path::PathBuf;
use std::time::Duration;

use chromiumoxide::{Browser, BrowserConfig};
use futures::StreamExt;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("browser launch failed: {0}")]
    Launch(String),
    #[error("browser navigation failed: {0}")]
    Navigate(String),
}

/// Open a persistent-profile browser at `login_url` and keep it open until the
/// owner presses Enter (manual login). Cookies persist in the profile dir; we
/// never read or print them here.
pub async fn bootstrap_login(profile_dir: PathBuf, login_url: &str) -> Result<(), BrowserError> {
    let config = BrowserConfig::builder()
        .user_data_dir(profile_dir)
        .with_head() // visible window so the owner can log in
        .build()
        .map_err(BrowserError::Launch)?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| BrowserError::Launch(e.to_string()))?;

    let handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let page = browser
        .new_page(login_url)
        .await
        .map_err(|e| BrowserError::Navigate(e.to_string()))?;
    page.wait_for_navigation()
        .await
        .map_err(|e| BrowserError::Navigate(e.to_string()))?;

    tracing::info!(
        event = "session_bootstrap_waiting",
        "log in in the opened browser, then press Enter in the terminal"
    );

    // Block on stdin so the operator can complete login interactively.
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);

    // Give the profile a moment to flush, then close cleanly.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = browser.close().await;
    let _ = handle.await;
    Ok(())
}
