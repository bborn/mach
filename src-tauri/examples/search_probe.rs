//! Measures what the sync and search design actually costs on a real mailbox.
//!
//! The spec picks a 12-month backfill window and a local-FTS + server-fallback
//! search. Both were reasoned about rather than measured. This answers, for a
//! real account:
//!
//!   * how many messages actually fall in each candidate window
//!   * whether Gmail's server-side `q=` search is fast enough to be the
//!     fallback tier
//!   * what a search costs when the results are not in the local store
//!
//! Read-only.
//!
//! ```sh
//! set -a && source ../.env.local && set +a
//! cargo run --example search_probe -- you@example.com
//! ```

use std::sync::Arc;
use std::time::Instant;

use mach_lib::auth::http::GoogleTokenHttp;
use mach_lib::auth::tokens::{KeychainTokenStore, TokenManager};
use mach_lib::auth::ClientConfig;
use mach_lib::google::gmail::{GmailClient, MessagesListQuery};
use mach_lib::google::{BoxFuture, GoogleError, ReqwestTransport, TokenProvider};

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
                .map_err(|e| GoogleError::Auth {
                    message: e.to_string(),
                })
        })
    }
}

/// Counts by walking pages of ids. Gmail's `resultSizeEstimate` is famously
/// approximate, so this pages for real, capped so the probe stays cheap.
async fn count_matching(
    gmail: &GmailClient,
    query: &str,
    cap: usize,
) -> Result<(usize, bool), GoogleError> {
    let q = MessagesListQuery::new().q(query).max_results(500);
    let ids = gmail.messages_list_all("me", &q, Some(cap)).await?;
    let hit_cap = ids.len() >= cap;
    Ok((ids.len(), hit_cap))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let email = std::env::args()
        .nth(1)
        .ok_or("usage: search_probe <account-email>")?;

    let config = ClientConfig::from_env()?;
    let manager = Arc::new(TokenManager::new(
        config,
        GoogleTokenHttp::new(),
        KeychainTokenStore::default(),
    ));
    let provider = Arc::new(ManagedToken {
        manager,
        email: email.clone(),
    });
    let gmail = GmailClient::new(Arc::new(ReqwestTransport::new()), provider);

    let profile = gmail.get_profile("me").await?;
    println!("\nmailbox: {} messages\n", profile.messages_total.unwrap_or(-1));

    println!("--- how big is each candidate backfill window? ---");
    for window in ["newer_than:1m", "newer_than:3m", "newer_than:1y", "newer_than:2y"] {
        let started = Instant::now();
        let (n, capped) = count_matching(&gmail, window, 20_000).await?;
        println!(
            "  {:<16} {}{:<7}  ({} ms to enumerate)",
            window,
            if capped { ">" } else { " " },
            n,
            started.elapsed().as_millis()
        );
    }

    println!("\n--- is server-side search fast enough to be the fallback tier? ---");
    for q in [
        "from:stripe",
        "has:attachment invoice",
        "subject:invoice older_than:2y",
        "soccer practice",
    ] {
        let started = Instant::now();
        let query = MessagesListQuery::new().q(q).max_results(25);
        let page = gmail.messages_list_page("me", &query, None).await?;
        println!(
            "  {:<32} {:>3} ids in {:>4} ms",
            q,
            page.items.len(),
            started.elapsed().as_millis()
        );
    }

    println!("\n--- what does hydrating a page of results cost? ---");
    let query = MessagesListQuery::new().q("from:stripe").max_results(10);
    let page = gmail.messages_list_page("me", &query, None).await?;
    let started = Instant::now();
    for item in page.items.iter().take(10) {
        let _ = gmail
            .messages_get_metadata("me", &item.id, &["From", "Subject", "Date"])
            .await?;
    }
    println!(
        "  10 x messages.get(metadata) = {} ms  ({} ms each)",
        started.elapsed().as_millis(),
        started.elapsed().as_millis() / 10
    );

    Ok(())
}
