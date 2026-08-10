//! What a forward carries.
//!
//! Forwarding "here's the invoice" used to send the words and not the invoice.
//! The body was reproduced whole under the separator and the files were dropped
//! silently, which is the shape of failure this project has paid most for: no
//! error, no chip, nothing on screen that differs from the message that worked.
//!
//! # The plan is made before anything is fetched
//!
//! [`plan`] is a local read. It knows the filenames and the sizes because the
//! sync pass stored them, so the 25 MB ceiling is decided against numbers that
//! are already here — no request goes out for a file that cannot ride. That is
//! what lets the composer say "these two are on it, that one is 30 MB and is
//! not" at the moment the draft is built, rather than after `⌘⏎`.
//!
//! The bytes are somebody else's job ([`crate::ipc::compose`]), because the
//! bytes are either on this disk already or a network away, and this module is
//! not allowed to know about either.
//!
//! # Two kinds of part, and only one of them has a row
//!
//! A **file** is an `attachments` row: name, type, size, and usually a Gmail
//! attachment id to fetch it by. Those are the chips the reader sees.
//!
//! An **inline image** is not a row at all. `google::types::is_inline` keeps it
//! out of `attachments` on purpose — it belongs to the body, addressed by a
//! `Content-ID` the HTML refers to as `cid:…`. So the only trace of one here is
//! the reference in the stored body, which is exactly what [`plan`] reads. Its
//! size is unknown until it arrives, so it is carried *first*: a body with a
//! hole in it is worse than a file that did not fit, and a file that did not fit
//! says so by name.
//!
//! # Gmail cannot be told to reuse the original
//!
//! There is no such call. `drafts.create` and `messages.send` take one field,
//! `raw`, holding a complete RFC822 message — the same constraint that makes
//! [`super::mime`] exist. Nothing in the Message resource is an input that names
//! a part of another message; `attachmentId` is a handle for
//! `users.messages.attachments.get`, which downloads. So a forwarded file is
//! re-uploaded, and the only saving available is not *downloading* it twice,
//! which is what the local cache is for.

use rusqlite::OptionalExtension;

use crate::db::Db;

use super::attach::{
    refusal_past_total, refusal_too_large, MAX_ATTACHMENT_BYTES, MAX_TOTAL_ATTACHMENT_BYTES,
};
use super::{ComposeError, Result};

/// One file a forward means to carry, before its bytes are in hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    /// The `attachments` row it comes from.
    pub attachment_id: i64,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    /// The handle `users.messages.attachments.get` takes. Absent when Gmail
    /// inlined the bytes in the sync response instead of handing out an id.
    pub gmail_attachment_id: Option<String>,
}

/// Everything a forward of one message will try to take with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub account_id: i64,
    pub gmail_message_id: String,
    /// The files that fit, oldest first — the order the message lists them and
    /// the order the composer will.
    pub files: Vec<PlannedFile>,
    /// The `cid:` values the original body addresses, in document order. These
    /// have no row and no known size; see the module doc.
    pub inline_cids: Vec<String>,
    /// A sentence for each file that will not be on the forward, naming it.
    /// Empty for almost every message, and never silent when it is not.
    pub refused: Vec<String>,
}

impl Plan {
    /// Is there anything at all to carry? A reply's plan is empty and so is a
    /// forward of a message with no files and no pictures in it.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.inline_cids.is_empty()
    }
}

/// What forwarding this message would take with it.
///
/// A local read: the `attachments` rows and the stored HTML body, both of which
/// the sync pass already wrote. Nothing here touches the network or the disk
/// cache, so it is safe on the path that opens a composer.
pub fn plan(db: &Db, message_id: i64) -> Result<Plan> {
    let Some((account_id, gmail_message_id, body_html)) = message_row(db, message_id)? else {
        return Err(ComposeError::UnknownMessage(message_id));
    };

    let inline_cids = body_html
        .as_deref()
        .map(super::html::cid_references)
        .unwrap_or_default();

    let mut files = Vec::new();
    let mut refused = Vec::new();
    let mut running: i64 = 0;
    for row in attachment_rows(db, message_id)? {
        if row.size_bytes > MAX_ATTACHMENT_BYTES as i64 {
            refused.push(refusal_too_large(&row.filename, row.size_bytes));
            continue;
        }
        if running + row.size_bytes > MAX_TOTAL_ATTACHMENT_BYTES as i64 {
            refused.push(refusal_past_total(&row.filename));
            continue;
        }
        running += row.size_bytes;
        files.push(row);
    }

    Ok(Plan {
        account_id,
        gmail_message_id,
        files,
        inline_cids,
        refused,
    })
}

/// The three things about the message a plan is made from.
fn message_row(db: &Db, message_id: i64) -> Result<Option<(i64, String, Option<String>)>> {
    Ok(db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT account_id, gmail_message_id, body_html FROM messages WHERE id = ?1",
                [message_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    })?)
}

fn attachment_rows(db: &Db, message_id: i64) -> Result<Vec<PlannedFile>> {
    Ok(db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, gmail_attachment_id, filename, mime_type, size_bytes
               FROM attachments WHERE message_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([message_id], |row| {
            Ok(PlannedFile {
                attachment_id: row.get(0)?,
                gmail_attachment_id: row.get::<_, Option<String>>(1)?.filter(|s| !s.is_empty()),
                filename: row.get(2)?,
                mime_type: row.get(3)?,
                size_bytes: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })?)
}
