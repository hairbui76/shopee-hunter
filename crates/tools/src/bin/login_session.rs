//! Owner-run session bootstrap (ROADMAP Phase 9).
//!
//! Opens a persistent Chromium profile so the owner can log in to Shopee
//! manually. Cookies persist inside the profile directory; this tool never
//! prints or extracts them. No verification challenge is bypassed.
//!
//! Build/run with the `browser` feature (requires a local Chromium):
//!   cargo run -p shopee-hunter-tools --bin login_session --features browser
//!
//! Configuration (env):
//!   SHOPEE_PROFILE_PATH  persistent browser profile directory
//!   SHOPEE_BASE_URL      defaults to https://shopee.vn

#[cfg(feature = "browser")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let profile = std::env::var("SHOPEE_PROFILE_PATH")
        .unwrap_or_else(|_| "/var/lib/shopee-hunter/browser-profile".to_string());
    let base_url =
        std::env::var("SHOPEE_BASE_URL").unwrap_or_else(|_| "https://shopee.vn".to_string());
    let login_url = format!("{}/buyer/login", base_url.trim_end_matches('/'));

    tracing::info!(
        event = "session_bootstrap_start",
        profile = %profile,
        "opening browser for manual login",
    );

    shopee_hunter_session::browser::bootstrap_login(PathBuf::from(&profile), &login_url).await?;

    tracing::info!(
        event = "session_bootstrap_done",
        "profile persisted; you can now start the service"
    );
    Ok(())
}

#[cfg(not(feature = "browser"))]
fn main() {
    eprintln!(
        "login_session requires the `browser` feature.\n\
         Run: cargo run -p shopee-hunter-tools --bin login_session --features browser"
    );
    std::process::exit(2);
}
