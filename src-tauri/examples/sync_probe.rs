//! Replays one account's incremental sweep against the real Gmail, read-only,
//! and says which request fails.
//!
//! The sync engine treats the whole sweep as one unit: a single failed fetch
//! discards it and leaves the watermark where it was. When an account stops
//! advancing, this is how to find the request that is stopping it without
//! touching the store or the mailbox.
//!
//! ```sh
//! set -a && source ../.env.local && set +a
//! cargo run --example sync_probe -- you@example.com <historyId>
//! ```

use std::sync::Arc;

use mach_lib::auth::http::GoogleTokenHttp;
use mach_lib::auth::tokens::{KeychainTokenStore, TokenManager};
use mach_lib::auth::ClientConfig;
use mach_lib::google::gmail::{GmailClient, HistoryListQuery, HistoryType, MessageFormat};
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
                .map_err(|e| GoogleError::Auth { message: e.to_string() })
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let email = args.next().ok_or("usage: sync_probe <email> <historyId>")?;
    let start = args.next().ok_or("usage: sync_probe <email> <historyId>")?;

    let manager = Arc::new(TokenManager::new(
        ClientConfig::from_env()?,
        GoogleTokenHttp::new(),
        KeychainTokenStore::default(),
    ));
    let provider = Arc::new(ManagedToken { manager, email });
    let gmail = GmailClient::new(Arc::new(ReqwestTransport::new()), provider);

    match gmail.labels_list("me").await {
        Ok(l) => println!("labels.list     ok ({})", l.len()),
        Err(e) => println!("labels.list     FAILED: {e:?}"),
    }

    let query = HistoryListQuery::new(&start)
        .history_types(HistoryType::all())
        .max_results(500);
    let sweep = match gmail.history_list_all("me", &query, None).await {
        Ok(s) => s,
        Err(e) => {
            println!("history.list    FAILED: {e:?}");
            return Ok(());
        }
    };
    let mut ids: Vec<String> = Vec::new();
    for r in &sweep.records {
        for m in r.messages_added.iter() {
            ids.push(m.message.id.clone());
        }
        for c in r.labels_added.iter().chain(r.labels_removed.iter()) {
            ids.push(c.message.id.clone());
        }
    }
    ids.sort();
    ids.dedup();
    println!(
        "history.list    ok: {} records, {} distinct messages, new watermark {:?}",
        sweep.records.len(),
        ids.len(),
        sweep.history_id
    );

    let mut failures = 0;
    for (i, id) in ids.iter().enumerate() {
        match gmail.messages_get("me", id, MessageFormat::Full).await {
            Ok(_) => {}
            Err(e) if e.is_not_found() => {}
            Err(e) => {
                failures += 1;
                println!("messages.get {id} FAILED after {i} ok: {e:?}");
                break;
            }
        }
        // The same pace the engine keeps, so the probe does not spend the
        // account's quota out from under the app.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if i % 200 == 199 {
            println!("  fetched {}/{}", i + 1, ids.len());
        }
    }
    println!("done: {failures} fetch failures");
    Ok(())
}
