//! End-to-end proof that authorization works against the real Google.
//!
//! Everything in `auth` is unit tested behind a fake transport, which proves
//! the logic but not the integration: whether the client is registered
//! correctly, whether the scopes are accepted, whether the loopback redirect is
//! allowed, whether a refresh token actually comes back.
//!
//! Run it with credentials in the environment:
//!
//! ```sh
//! set -a && source ../.env.local && set +a
//! cargo run --example oauth_smoke
//! ```
//!
//! It prints an authorization URL, waits for the redirect, and reports which
//! account consented and whether a refresh token was issued. Nothing is read
//! from the mailbox.

use mach_lib::auth::flow::{begin_authorization, complete_authorization};
use mach_lib::auth::http::GoogleTokenHttp;
use mach_lib::auth::tokens::KeychainTokenStore;
use mach_lib::auth::ClientConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ClientConfig::from_env()?;
    let http = GoogleTokenHttp::new();
    let store = KeychainTokenStore::default();

    let pending = begin_authorization(&config, None)?;

    println!("\nredirect_uri : {}", pending.redirect_uri());
    println!("\nOpen this URL to authorize:\n\n{}\n", pending.url);
    println!("(The app is unverified, so expect Google's warning screen —");
    println!(" click Advanced, then continue.)\n");
    println!("Waiting for the redirect...");

    let account = complete_authorization(pending, &config, &http, &store).await?;

    println!("\n  authorized  : {}", account.email);
    println!("  scopes      : {}", account.tokens.scope.as_deref().unwrap_or("(none returned)"));
    println!("  expires_at  : {}", account.tokens.expires_at);
    println!(
        "  refresh tok : {}",
        if account.tokens.refresh_token.is_some() {
            "issued and saved to the Keychain"
        } else {
            "NOT ISSUED — the app cannot stay signed in"
        }
    );

    Ok(())
}
