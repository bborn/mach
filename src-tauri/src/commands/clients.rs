//! How a command reaches Google.
//!
//! Mach has five accounts, each with its own OAuth grant, and a single command
//! can span all of them (the stream is unified, so a 50-thread selection is not
//! necessarily one mailbox). So the dispatcher does not hold *a* client — it
//! holds something that can produce the right client for an account id.
//!
//! [`GoogleClients`] is that seam. It exists as a trait for the same reason
//! [`HttpTransport`] does: tests want to script the whole thing, and a later
//! unit will want to hand over clients that share one connection pool and one
//! refresh-aware token manager without the command layer knowing.

use std::collections::HashMap;
use std::sync::Arc;

use crate::google::calendar::CalendarClient;
use crate::google::gmail::GmailClient;
use crate::google::{
    HttpTransport, RetryPolicy, Sleeper, TokenProvider, CALENDAR_BASE_URL, GMAIL_BASE_URL,
};

use super::error::CommandError;

/// Produces per-account API clients.
pub trait GoogleClients: Send + Sync {
    fn gmail(&self, account_id: i64) -> Result<GmailClient, CommandError>;
    fn calendar(&self, account_id: i64) -> Result<CalendarClient, CommandError>;
}

/// The default implementation: one shared transport, one token provider per
/// account.
///
/// Sharing the transport is deliberate — five accounts against
/// `gmail.googleapis.com` should share one connection pool, not open five.
pub struct AccountClients {
    transport: Arc<dyn HttpTransport>,
    tokens: HashMap<i64, Arc<dyn TokenProvider>>,
    gmail_base_url: String,
    calendar_base_url: String,
    retry: Option<RetryPolicy>,
    sleeper: Option<Arc<dyn Sleeper>>,
}

impl AccountClients {
    pub fn new(transport: Arc<dyn HttpTransport>) -> Self {
        AccountClients {
            transport,
            tokens: HashMap::new(),
            gmail_base_url: GMAIL_BASE_URL.to_string(),
            calendar_base_url: CALENDAR_BASE_URL.to_string(),
            retry: None,
            sleeper: None,
        }
    }

    pub fn with_account(mut self, account_id: i64, tokens: Arc<dyn TokenProvider>) -> Self {
        self.tokens.insert(account_id, tokens);
        self
    }

    pub fn with_gmail_base_url(mut self, url: impl Into<String>) -> Self {
        self.gmail_base_url = url.into();
        self
    }

    pub fn with_calendar_base_url(mut self, url: impl Into<String>) -> Self {
        self.calendar_base_url = url.into();
        self
    }

    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = Some(retry);
        self
    }

    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = Some(sleeper);
        self
    }

    fn tokens_for(&self, account_id: i64) -> Result<Arc<dyn TokenProvider>, CommandError> {
        self.tokens
            .get(&account_id)
            .cloned()
            .ok_or(CommandError::UnknownAccount { account_id })
    }
}

impl GoogleClients for AccountClients {
    fn gmail(&self, account_id: i64) -> Result<GmailClient, CommandError> {
        let mut client = GmailClient::new(Arc::clone(&self.transport), self.tokens_for(account_id)?)
            .with_base_url(self.gmail_base_url.clone());
        if let Some(retry) = self.retry {
            client = client.with_retry_policy(retry);
        }
        if let Some(sleeper) = &self.sleeper {
            client = client.with_sleeper(Arc::clone(sleeper));
        }
        Ok(client)
    }

    fn calendar(&self, account_id: i64) -> Result<CalendarClient, CommandError> {
        let mut client =
            CalendarClient::new(Arc::clone(&self.transport), self.tokens_for(account_id)?)
                .with_base_url(self.calendar_base_url.clone());
        if let Some(retry) = self.retry {
            client = client.with_retry_policy(retry);
        }
        if let Some(sleeper) = &self.sleeper {
            client = client.with_sleeper(Arc::clone(sleeper));
        }
        Ok(client)
    }
}
