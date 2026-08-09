//! Gmail REST client.
//!
//! Covers exactly the endpoints Mach's sync engine and command layer need:
//! list, get, modify, send, history, labels, attachments, profile.
//!
//! Authentication is not this module's job — a [`TokenProvider`] supplies the
//! bearer token, and an [`HttpTransport`] does the I/O.

use std::sync::Arc;

use serde_json::json;

use super::types::{
    encode_base64url, AttachmentBody, Draft, HistoryListResponse, HistorySweep, Label,
    LabelsListResponse, Message, MessageRef, MessagesListResponse, Profile, ThreadsListResponse,
};
use super::{
    GoogleError, HttpMethod, HttpTransport, Page, RestClient, RetryPolicy, Sleeper, TokenProvider,
    GMAIL_BASE_URL,
};

/// The `format` parameter of `users.messages.get`.
///
/// `Metadata` is enough for a list row (headers, labels, no bodies) and costs
/// less quota. `Full` returns the nested MIME part tree that
/// [`Message::extract_body`](super::types::Message::extract_body) walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageFormat {
    Minimal,
    Metadata,
    Full,
    Raw,
}

impl MessageFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageFormat::Minimal => "minimal",
            MessageFormat::Metadata => "metadata",
            MessageFormat::Full => "full",
            MessageFormat::Raw => "raw",
        }
    }
}

/// The kinds of change `users.history.list` can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryType {
    MessageAdded,
    MessageDeleted,
    LabelAdded,
    LabelRemoved,
}

impl HistoryType {
    pub fn as_str(self) -> &'static str {
        match self {
            HistoryType::MessageAdded => "messageAdded",
            HistoryType::MessageDeleted => "messageDeleted",
            HistoryType::LabelAdded => "labelAdded",
            HistoryType::LabelRemoved => "labelRemoved",
        }
    }

    /// Everything the sync engine cares about.
    pub fn all() -> [HistoryType; 4] {
        [
            HistoryType::MessageAdded,
            HistoryType::MessageDeleted,
            HistoryType::LabelAdded,
            HistoryType::LabelRemoved,
        ]
    }
}

#[derive(Debug, Clone, Default)]
pub struct MessagesListQuery {
    pub q: Option<String>,
    pub label_ids: Vec<String>,
    pub max_results: Option<u32>,
    pub include_spam_trash: Option<bool>,
}

impl MessagesListQuery {
    pub fn new() -> Self {
        Self::default()
    }

    /// A Gmail search expression, e.g. `from:tawny has:attachment`.
    pub fn q(mut self, q: impl Into<String>) -> Self {
        self.q = Some(q.into());
        self
    }

    pub fn label_ids<I, S>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.label_ids = ids.into_iter().map(Into::into).collect();
        self
    }

    pub fn max_results(mut self, n: u32) -> Self {
        self.max_results = Some(n);
        self
    }

    pub fn include_spam_trash(mut self, yes: bool) -> Self {
        self.include_spam_trash = Some(yes);
        self
    }

    fn apply(&self, url: &mut url::Url) {
        let mut pairs = url.query_pairs_mut();
        if let Some(q) = &self.q {
            pairs.append_pair("q", q);
        }
        for id in &self.label_ids {
            pairs.append_pair("labelIds", id);
        }
        if let Some(n) = self.max_results {
            pairs.append_pair("maxResults", &n.to_string());
        }
        if let Some(b) = self.include_spam_trash {
            pairs.append_pair("includeSpamTrash", if b { "true" } else { "false" });
        }
    }
}

#[derive(Debug, Clone)]
pub struct HistoryListQuery {
    /// The stored watermark. Gmail returns every change *after* it.
    pub start_history_id: String,
    pub history_types: Vec<HistoryType>,
    pub label_id: Option<String>,
    pub max_results: Option<u32>,
}

impl HistoryListQuery {
    pub fn new(start_history_id: impl Into<String>) -> Self {
        Self {
            start_history_id: start_history_id.into(),
            history_types: Vec::new(),
            label_id: None,
            max_results: None,
        }
    }

    pub fn history_types<I>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = HistoryType>,
    {
        self.history_types = types.into_iter().collect();
        self
    }

    pub fn label_id(mut self, id: impl Into<String>) -> Self {
        self.label_id = Some(id.into());
        self
    }

    pub fn max_results(mut self, n: u32) -> Self {
        self.max_results = Some(n);
        self
    }

    fn apply(&self, url: &mut url::Url) {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("startHistoryId", &self.start_history_id);
        for t in &self.history_types {
            pairs.append_pair("historyTypes", t.as_str());
        }
        if let Some(id) = &self.label_id {
            pairs.append_pair("labelId", id);
        }
        if let Some(n) = self.max_results {
            pairs.append_pair("maxResults", &n.to_string());
        }
    }
}

/// Gmail API client for one account.
#[derive(Clone)]
pub struct GmailClient {
    rest: RestClient,
}

impl GmailClient {
    pub fn new(transport: Arc<dyn HttpTransport>, tokens: Arc<dyn TokenProvider>) -> Self {
        Self {
            rest: RestClient::new(transport, tokens, GMAIL_BASE_URL),
        }
    }

    /// Point the client somewhere other than `gmail.googleapis.com` — the seam
    /// that makes these endpoints testable without a network.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.rest = self.rest.with_base_url(base_url);
        self
    }

    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.rest = self.rest.with_retry_policy(retry);
        self
    }

    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.rest = self.rest.with_sleeper(sleeper);
        self
    }

    pub fn base_url(&self) -> &str {
        self.rest.base_url()
    }

    // ------------------------------------------------------------- messages

    /// `users.messages.list`, one page.
    pub async fn messages_list_page(
        &self,
        user_id: &str,
        query: &MessagesListQuery,
        page_token: Option<&str>,
    ) -> Result<Page<MessageRef>, GoogleError> {
        let mut url = self.rest.endpoint(&["users", user_id, "messages"])?;
        query.apply(&mut url);
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", token);
        }
        let response: MessagesListResponse =
            self.rest.send_json(HttpMethod::Get, url, None).await?;
        Ok(Page::new(response.messages, response.next_page_token))
    }

    /// `users.messages.list`, following `nextPageToken` to the end so callers
    /// never write a token loop. `limit` caps the result and stops paging early.
    pub async fn messages_list_all(
        &self,
        user_id: &str,
        query: &MessagesListQuery,
        limit: Option<usize>,
    ) -> Result<Vec<MessageRef>, GoogleError> {
        let mut out: Vec<MessageRef> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .messages_list_page(user_id, query, token.as_deref())
                .await?;
            out.extend(page.items);
            if let Some(limit) = limit {
                if out.len() >= limit {
                    out.truncate(limit);
                    return Ok(out);
                }
            }
            match page.next_page_token {
                Some(next) => token = Some(next),
                None => return Ok(out),
            }
        }
    }

    /// `users.threads.list`, one page.
    pub async fn threads_list_page(
        &self,
        user_id: &str,
        query: &MessagesListQuery,
        page_token: Option<&str>,
    ) -> Result<Page<MessageRef>, GoogleError> {
        let mut url = self.rest.endpoint(&["users", user_id, "threads"])?;
        query.apply(&mut url);
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", token);
        }
        let response: ThreadsListResponse = self.rest.send_json(HttpMethod::Get, url, None).await?;
        Ok(Page::new(response.threads, response.next_page_token))
    }

    /// `users.messages.get`.
    pub async fn messages_get(
        &self,
        user_id: &str,
        message_id: &str,
        format: MessageFormat,
    ) -> Result<Message, GoogleError> {
        let mut url = self
            .rest
            .endpoint(&["users", user_id, "messages", message_id])?;
        url.query_pairs_mut().append_pair("format", format.as_str());
        self.rest.send_json(HttpMethod::Get, url, None).await
    }

    /// `users.messages.get` with `format=metadata`, restricted to the headers
    /// a list row actually needs.
    pub async fn messages_get_metadata(
        &self,
        user_id: &str,
        message_id: &str,
        headers: &[&str],
    ) -> Result<Message, GoogleError> {
        let mut url = self
            .rest
            .endpoint(&["users", user_id, "messages", message_id])?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("format", MessageFormat::Metadata.as_str());
            for header in headers {
                pairs.append_pair("metadataHeaders", header);
            }
        }
        self.rest.send_json(HttpMethod::Get, url, None).await
    }

    /// `users.messages.modify` — how archive, read and star are all expressed.
    /// Archive is `remove: ["INBOX"]`; read is `remove: ["UNREAD"]`; star is
    /// `add: ["STARRED"]`.
    pub async fn messages_modify(
        &self,
        user_id: &str,
        message_id: &str,
        add_label_ids: &[&str],
        remove_label_ids: &[&str],
    ) -> Result<Message, GoogleError> {
        let url = self
            .rest
            .endpoint(&["users", user_id, "messages", message_id, "modify"])?;
        let body = json!({
            "addLabelIds": add_label_ids,
            "removeLabelIds": remove_label_ids,
        });
        self.rest
            .send_json(HttpMethod::Post, url, Some(body.to_string().into_bytes()))
            .await
    }

    /// `users.messages.batchModify` — the same operation over up to 1000 ids,
    /// for one-keystroke bulk triage. Returns nothing on success.
    pub async fn messages_batch_modify(
        &self,
        user_id: &str,
        message_ids: &[&str],
        add_label_ids: &[&str],
        remove_label_ids: &[&str],
    ) -> Result<(), GoogleError> {
        let url = self
            .rest
            .endpoint(&["users", user_id, "messages", "batchModify"])?;
        let body = json!({
            "ids": message_ids,
            "addLabelIds": add_label_ids,
            "removeLabelIds": remove_label_ids,
        });
        self.rest
            .send_empty(HttpMethod::Post, url, Some(body.to_string().into_bytes()))
            .await
    }

    /// `users.messages.trash`.
    pub async fn messages_trash(
        &self,
        user_id: &str,
        message_id: &str,
    ) -> Result<Message, GoogleError> {
        let url = self
            .rest
            .endpoint(&["users", user_id, "messages", message_id, "trash"])?;
        self.rest
            .send_json(HttpMethod::Post, url, Some(b"{}".to_vec()))
            .await
    }

    /// `users.messages.send`.
    ///
    /// There is no structured to/subject/body form — Gmail takes a complete
    /// RFC822 message, base64url encoded, in `raw`. Composing that message is
    /// somebody else's job; this only puts it on the wire. `thread_id` makes
    /// the sent message land in an existing thread.
    pub async fn messages_send(
        &self,
        user_id: &str,
        rfc822: &[u8],
        thread_id: Option<&str>,
    ) -> Result<Message, GoogleError> {
        let url = self
            .rest
            .endpoint(&["users", user_id, "messages", "send"])?;
        let mut body = json!({ "raw": encode_base64url(rfc822) });
        if let Some(thread_id) = thread_id {
            body["threadId"] = json!(thread_id);
        }
        self.rest
            .send_json(HttpMethod::Post, url, Some(body.to_string().into_bytes()))
            .await
    }

    // --------------------------------------------------------------- drafts

    /// `users.drafts.create`.
    ///
    /// Same `raw` bytes as [`messages_send`](Self::messages_send) — a Gmail
    /// draft *is* a message, filed under the `DRAFT` label — so the composer
    /// builds one message and this decides not to send it. `thread_id` is what
    /// makes a reply draft appear inside its conversation rather than as a
    /// stray message, on the phone as well as here.
    pub async fn drafts_create(
        &self,
        user_id: &str,
        rfc822: &[u8],
        thread_id: Option<&str>,
    ) -> Result<Draft, GoogleError> {
        let url = self.rest.endpoint(&["users", user_id, "drafts"])?;
        let body = json!({ "message": draft_message(rfc822, thread_id) });
        self.rest
            .send_json(HttpMethod::Post, url, Some(body.to_string().into_bytes()))
            .await
    }

    /// `users.drafts.update` — replaces the draft's content in place.
    ///
    /// In place matters: an update keeps the draft id, so editing a draft five
    /// times leaves one draft on the phone rather than five.
    pub async fn drafts_update(
        &self,
        user_id: &str,
        draft_id: &str,
        rfc822: &[u8],
        thread_id: Option<&str>,
    ) -> Result<Draft, GoogleError> {
        let url = self.rest.endpoint(&["users", user_id, "drafts", draft_id])?;
        let body = json!({ "id": draft_id, "message": draft_message(rfc822, thread_id) });
        self.rest
            .send_json(HttpMethod::Put, url, Some(body.to_string().into_bytes()))
            .await
    }

    /// `users.drafts.delete`. Returns nothing on success.
    pub async fn drafts_delete(&self, user_id: &str, draft_id: &str) -> Result<(), GoogleError> {
        let url = self.rest.endpoint(&["users", user_id, "drafts", draft_id])?;
        self.rest.send_empty(HttpMethod::Delete, url, None).await
    }

    // -------------------------------------------------------------- history

    /// `users.history.list`, one page — the incremental sync path (2 quota
    /// units against `messages.list`'s 5).
    ///
    /// A 404 here does **not** mean "no such history". It means the stored
    /// watermark has aged out, so it is surfaced as
    /// [`GoogleError::HistoryExpired`] and the caller must do a full resync.
    pub async fn history_list(
        &self,
        user_id: &str,
        query: &HistoryListQuery,
        page_token: Option<&str>,
    ) -> Result<HistoryListResponse, GoogleError> {
        let mut url = self.rest.endpoint(&["users", user_id, "history"])?;
        query.apply(&mut url);
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", token);
        }
        self.rest
            .send_json(HttpMethod::Get, url, None)
            .await
            .map_err(history_not_found_means_expired)
    }

    /// Every page of a history sweep, plus the watermark to persist. Returns
    /// [`GoogleError::HistoryExpired`] if the starting watermark is too old.
    pub async fn history_list_all(
        &self,
        user_id: &str,
        query: &HistoryListQuery,
        limit: Option<usize>,
    ) -> Result<HistorySweep, GoogleError> {
        let mut sweep = HistorySweep::default();
        let mut token: Option<String> = None;
        loop {
            let page = self.history_list(user_id, query, token.as_deref()).await?;
            // The last page seen carries the newest watermark.
            if page.history_id.is_some() {
                sweep.history_id = page.history_id.clone();
            }
            sweep.records.extend(page.history);
            if let Some(limit) = limit {
                if sweep.records.len() >= limit {
                    sweep.records.truncate(limit);
                    return Ok(sweep);
                }
            }
            match page.next_page_token.filter(|t| !t.is_empty()) {
                Some(next) => token = Some(next),
                None => return Ok(sweep),
            }
        }
    }

    // --------------------------------------------------------------- labels

    /// `users.labels.list`. Not paginated by Google.
    pub async fn labels_list(&self, user_id: &str) -> Result<Vec<Label>, GoogleError> {
        let url = self.rest.endpoint(&["users", user_id, "labels"])?;
        let response: LabelsListResponse = self.rest.send_json(HttpMethod::Get, url, None).await?;
        Ok(response.labels)
    }

    /// `users.labels.create`.
    ///
    /// Used to make the snooze label on demand. Gmail has no snooze primitive,
    /// so Mach represents it with a real user label — and refusing to snooze
    /// because that label does not exist yet would be a dead end the user
    /// cannot resolve from inside the app.
    ///
    /// Returns `GoogleError::InvalidRequest` if the name is already taken,
    /// which the caller should treat as "fine, look it up" rather than fatal.
    pub async fn labels_create(&self, user_id: &str, name: &str) -> Result<Label, GoogleError> {
        let url = self.rest.endpoint(&["users", user_id, "labels"])?;
        let body = serde_json::json!({
            "name": name,
            "labelListVisibility": "labelShow",
            "messageListVisibility": "show",
        });
        self.rest
            .send_json(HttpMethod::Post, url, Some(body.to_string().into_bytes()))
            .await
    }

    // ---------------------------------------------------------- attachments

    /// `users.messages.attachments.get`, decoded to bytes.
    ///
    /// Uncapped. Prefer [`attachment_get_capped`](Self::attachment_get_capped)
    /// for anything whose size a stranger chose.
    pub async fn attachment_get(
        &self,
        user_id: &str,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GoogleError> {
        self.attachment_get_capped(user_id, message_id, attachment_id, usize::MAX)
            .await
    }

    /// `users.messages.attachments.get`, refusing anything over `max_bytes`.
    ///
    /// # Why this buffers, and what is done about it
    ///
    /// It would be better to stream this to disk, and it is not possible here.
    /// The endpoint does not return the file: it returns a JSON envelope,
    /// `{"size":N,"data":"<base64url>"}`, so there is no useful prefix of the
    /// response — the bytes cannot be decoded until the field is closed, and the
    /// field is not closed until the transfer is over. Layered on top of that,
    /// [`HttpTransport`](super::HttpTransport) hands back an
    /// [`HttpResponse`](super::HttpResponse) whose body is an owned `Vec<u8>`;
    /// every client in this crate and both test suites are written against that
    /// shape, and widening it to a byte stream is a change to the transport
    /// seam rather than to this endpoint.
    ///
    /// So the honest description is: this is a buffered fetch, with the peak
    /// held down by three things.
    ///
    /// 1. **The cap is checked before the body is looked at.** A response whose
    ///    encoded length could not possibly decode to something under the limit
    ///    is rejected without being parsed, so a hostile `size` field cannot make
    ///    us allocate a second copy of a gigabyte.
    /// 2. **`send` is used rather than `send_json`,** which lets the raw response
    ///    be dropped the moment the envelope is parsed instead of being held
    ///    alive across the decode.
    /// 3. **The encoded string is dropped before the decoded bytes are
    ///    returned,** so the two large allocations do not both outlive the call.
    ///
    /// Peak is therefore a little over twice the file, transiently, against a
    /// cap the caller sets — not the unbounded growth a naive version has.
    pub async fn attachment_get_capped(
        &self,
        user_id: &str,
        message_id: &str,
        attachment_id: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, GoogleError> {
        let url = self.rest.endpoint(&[
            "users",
            user_id,
            "messages",
            message_id,
            "attachments",
            attachment_id,
        ])?;

        let response = self.rest.send(HttpMethod::Get, url, None).await?;

        // base64 is four characters per three bytes; the rest of the envelope is
        // a few dozen. Anything past that ceiling cannot decode to something
        // within the cap, so it is refused before it is parsed.
        let ceiling = encoded_ceiling(max_bytes);
        if response.body.len() > ceiling {
            return Err(too_large(attachment_id, max_bytes));
        }

        let body: AttachmentBody =
            serde_json::from_slice(&response.body).map_err(|e| GoogleError::Deserialize {
                message: format!("attachment {attachment_id} was not an attachment body: {e}"),
            })?;
        drop(response);

        // Google's own declared size, checked before the decode allocates.
        if body.size > 0 && usize::try_from(body.size).unwrap_or(usize::MAX) > max_bytes {
            return Err(too_large(attachment_id, max_bytes));
        }

        let data = body.data.unwrap_or_default();
        let bytes =
            super::types::decode_base64url(&data).map_err(|e| GoogleError::Deserialize {
                message: format!("attachment {attachment_id} was not valid base64url: {e}"),
            })?;
        drop(data);

        // And the truth, in case the envelope lied about both of the above.
        if bytes.len() > max_bytes {
            return Err(too_large(attachment_id, max_bytes));
        }

        Ok(bytes)
    }

    // -------------------------------------------------------------- profile

    /// `users.getProfile` — the account's own address and current `historyId`,
    /// which is the starting watermark after a backfill.
    pub async fn get_profile(&self, user_id: &str) -> Result<Profile, GoogleError> {
        let url = self.rest.endpoint(&["users", user_id, "profile"])?;
        self.rest.send_json(HttpMethod::Get, url, None).await
    }
}

/// The `message` object a draft write takes: the bytes, and the conversation
/// they belong to.
fn draft_message(rfc822: &[u8], thread_id: Option<&str>) -> serde_json::Value {
    let mut message = json!({ "raw": encode_base64url(rfc822) });
    if let Some(thread_id) = thread_id {
        message["threadId"] = json!(thread_id);
    }
    message
}

/// The largest response body that could still decode to `max_bytes`.
///
/// Four base64 characters per three bytes, plus room for the JSON envelope, the
/// `size` field and any whitespace Google chooses to pretty-print with.
/// Saturating, because `usize::MAX` is a legitimate "no cap" argument and must
/// not wrap around into a cap of nearly zero.
fn encoded_ceiling(max_bytes: usize) -> usize {
    max_bytes
        .saturating_mul(4)
        .saturating_div(3)
        .saturating_add(64 * 1024)
}

fn too_large(attachment_id: &str, max_bytes: usize) -> GoogleError {
    GoogleError::InvalidRequest {
        message: format!(
            "attachment {attachment_id} is larger than the {} MiB Mach will download",
            max_bytes / (1024 * 1024)
        ),
    }
}

/// The one error translation the whole sync design hangs on.
fn history_not_found_means_expired(error: GoogleError) -> GoogleError {
    match error {
        GoogleError::NotFound { message } => GoogleError::HistoryExpired { message },
        other => other,
    }
}
