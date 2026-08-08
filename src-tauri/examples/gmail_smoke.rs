//! Proves the stored credential actually opens the mailbox.
//!
//! `oauth_smoke` proves authorization works. This proves the part that matters
//! every day after: that the refresh token survives in the Keychain, that it
//! mints a working access token without any user interaction, and that the
//! Gmail client talks to the real API through it.
//!
//! Read-only — it counts things and lists label names. Nothing is modified.
//!
//! ```sh
//! set -a && source ../.env.local && set +a
//! cargo run --example gmail_smoke -- you@example.com
//! ```

use std::sync::Arc;

use mach_lib::auth::http::GoogleTokenHttp;
use mach_lib::auth::tokens::{KeychainTokenStore, TokenManager};
use mach_lib::auth::ClientConfig;
use mach_lib::google::gmail::GmailClient;
use mach_lib::google::{BoxFuture, GoogleError, ReqwestTransport, TokenProvider};

/// Bridges `auth`'s TokenManager to `google`'s TokenProvider.
///
/// The two units were built independently against this seam and had never been
/// connected before — this adapter is the whole integration, and the fact that
/// it is nine lines is the point of the seam.
struct ManagedToken {
    manager: Arc<TokenManager<GoogleTokenHttp, KeychainTokenStore>>,
    email: String,
}

impl TokenProvider for ManagedToken {
    fn access_token<'a>(&'a self) -> BoxFuture<'a, Result<String, GoogleError>> {
        Box::pin(async move {
            self.manager
                .access_token(&self.email)
                .await
                .map(|s| s.expose().to_string())
                .map_err(|e| GoogleError::Auth { message: e.to_string() })
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let email = std::env::args()
        .nth(1)
        .ok_or("usage: gmail_smoke <account-email>")?;

    let config = ClientConfig::from_env()?;
    let manager = Arc::new(TokenManager::new(
        config,
        GoogleTokenHttp::new(),
        KeychainTokenStore::default(),
    ));

    let provider = Arc::new(ManagedToken {
        manager: manager.clone(),
        email: email.clone(),
    });

    println!("refreshing from the Keychain (no browser)...");
    let gmail = GmailClient::new(Arc::new(ReqwestTransport::new()), provider);

    let profile = gmail.get_profile("me").await?;
    println!("\n  account       : {}", profile.email_address);
    println!("  messages      : {}", profile.messages_total.unwrap_or(-1));
    println!("  threads       : {}", profile.threads_total.unwrap_or(-1));
    println!("  historyId     : {}", profile.history_id);

    let labels = gmail.labels_list("me").await?;
    println!("  labels        : {}", labels.len());
    let mut names: Vec<_> = labels.iter().map(|l| l.name.as_str()).collect();
    names.sort_unstable();
    println!("  sample        : {}", names.iter().take(8).cloned().collect::<Vec<_>>().join(", "));

    Ok(())
}
