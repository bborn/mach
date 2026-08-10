//! Files on their way out.
//!
//! The receive side ([`crate::ipc::attachments`]) caches bytes it did not ask
//! for, from senders it does not trust, so it is content-addressed, capped and
//! evictable. This is the opposite problem — bytes the owner chose, which must
//! not be evicted, must survive a restart, and must still be there when the
//! outbox flushes twenty seconds later.
//!
//! So: rows in the store, beside the draft they belong to, deleted by the same
//! statement that deletes it. What *is* reused from the receive side is the part
//! that has already been thought about hardest — [`names::safe_filename`], which
//! is the only sanitiser in this codebase for a filename that a person did not
//! choose character by character. Writing a second one would be the mistake.
//!
//! # Why the bytes are a BLOB
//!
//! A path would be smaller and wrong. A draft written on Tuesday and sent on
//! Thursday would reference a file that has since been renamed, moved into a
//! Trash the owner then emptied, or dropped from a volume that is no longer
//! mounted — and the failure would land at the one moment nothing can be done
//! about it, after `⌘⏎`, inside the ten seconds when the message is supposed to
//! be already gone. `compose_outbox.rfc822` made the same choice for the same
//! reason: once something is queued, the store holds everything it needs.
//!
//! # Limits
//!
//! [`MAX_ATTACHMENT_BYTES`] and [`MAX_TOTAL_ATTACHMENT_BYTES`] are both 25 MB,
//! which is Gmail's own ceiling on a message. Base64 costs a third on top, so
//! 25 MB of files is about 34 MB on the wire — inside the 35 MB the upload
//! endpoint accepts and well outside what `messages.send`'s JSON body will take,
//! which is why [`outbox`](super::outbox) picks its endpoint by size.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::ipc::attachments::store::names;

use super::{ensure_compose_schema, ComposeError, Result};

/// The largest single file Mach will attach. Gmail refuses a message over 25 MB
/// whichever endpoint it arrives on, so a larger file is not a limit this app
/// invented — it is the one the send would hit anyway, moved to the moment the
/// file is chosen, when the owner can still do something about it.
pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

/// The largest a draft's attachments may come to in total. The same number: one
/// 25 MB file and five 5 MB ones are the same message to Gmail.
pub const MAX_TOTAL_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

/// The largest image Mach will put *in* a body rather than beside it.
///
/// Much tighter than the send limit, and the same number the receive side uses
/// for a `cid:` image ([`crate::render::sanitize::MAX_DATA_URI_BYTES`]) —
/// because it is the same cost. Showing an inline image in the composer means
/// base64 over IPC and a `data:` URL in the document, which base64 inflates by
/// a third: a 20 MB photograph is a 27 MB string built, copied and parsed
/// before the picture appears. An image past this is still attached and still
/// sent; it is the *placing in the body* that is refused, and refused where the
/// file is chosen rather than by drawing a picture that never loads.
pub const MAX_INLINE_IMAGE_BYTES: u64 = crate::render::sanitize::MAX_DATA_URI_BYTES as u64;

/// One file attached to a draft. Metadata only — the bytes are fetched by id
/// when the message is built, because a draft round-trips through the editor on
/// every keystroke and 25 MB has no business making that trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub draft_id: String,
    /// Sanitized. This is the name the recipient sees and the name the composer
    /// shows, which must be the same string.
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    #[serde(default)]
    pub added_at: i64,
    /// In the body rather than beside it: `Content-Disposition: inline`, a
    /// `Content-ID`, and an `<img src="cid:…">` in the HTML pointing at it.
    ///
    /// Only ever true for an image. A `Content-Disposition: inline` PDF is
    /// legal and means "render this where it sits", which no mail client does —
    /// so it arrives as an attachment with a header saying otherwise.
    #[serde(default)]
    pub inline: bool,
    /// The `Content-ID`, bare — no angle brackets, which are added on write, and
    /// no `cid:` prefix, which belongs to the URL and not to the header.
    ///
    /// Allocated for every attachment, image or not, so that turning one inline
    /// later is a flag and not a rename. It is only written into the message
    /// when [`inline`](Self::inline) is set.
    #[serde(default)]
    pub content_id: String,
}

impl Attachment {
    /// Whether this file *could* go in the body. The composer offers the choice
    /// on exactly these, and [`set_inline`] refuses it on anything else.
    pub fn is_image(&self) -> bool {
        is_image_mime(&self.mime_type)
    }
}

/// An image small enough to be worth drawing where it sits.
fn can_be_inline(mime_type: &str, size: u64) -> bool {
    is_image_mime(mime_type) && size <= MAX_INLINE_IMAGE_BYTES
}

/// An image by its Content-Type, ignoring parameters.
///
/// SVG is excluded. It is an image to a browser and a script host to everything
/// that renders it, and an inline SVG in a message is asking a recipient's
/// client to run a document from this Mac. It can still be attached.
pub fn is_image_mime(mime_type: &str) -> bool {
    let base = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    base.starts_with("image/") && base != "image/svg+xml"
}

/// Attach a file already read into memory.
///
/// Reading is the caller's job so that this function is testable without a
/// filesystem, and so the one place that turns a user-chosen path into bytes is
/// the IPC layer, where the choice was made.
///
/// `inline` is a *request*, not an instruction: a file that is not an image
/// lands as an ordinary attachment whatever was asked for, because the
/// alternative is a message whose body references a `cid:` no client will draw.
pub fn add_bytes(
    db: &Db,
    draft_id: &str,
    raw_name: &str,
    bytes: &[u8],
    inline: bool,
    now_ms: i64,
) -> Result<Attachment> {
    db.write(ensure_compose_schema)?;

    let size = bytes.len() as u64;
    let filename = names::safe_filename(raw_name);
    if size > MAX_ATTACHMENT_BYTES {
        return Err(ComposeError::invalid(format!(
            "{filename} is {} — larger than the {} MB Gmail will send",
            human_size(size as i64),
            MAX_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }
    let already = total_bytes(db, draft_id)? as u64;
    if already + size > MAX_TOTAL_ATTACHMENT_BYTES {
        return Err(ComposeError::invalid(format!(
            "{filename} would take this message past the {} MB Gmail will send",
            MAX_TOTAL_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }

    let mime_type = mime_for(&filename);
    let id = format!("att-{now_ms:x}-{:x}", entropy(now_ms));
    let attachment = Attachment {
        content_id: content_id_for(&id),
        inline: inline && can_be_inline(&mime_type, size),
        id: id.clone(),
        draft_id: draft_id.to_string(),
        filename,
        mime_type,
        size_bytes: size as i64,
        added_at: now_ms,
    };

    db.write(|conn| {
        conn.execute(
            "INSERT INTO compose_attachments
                 (id, draft_id, filename, mime_type, size_bytes, bytes, added_at,
                  inline, content_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                attachment.id,
                attachment.draft_id,
                attachment.filename,
                attachment.mime_type,
                attachment.size_bytes,
                bytes,
                attachment.added_at,
                attachment.inline as i64,
                attachment.content_id,
            ],
        )?;
        Ok(())
    })?;

    Ok(attachment)
}

/// Move one file between the body and the attachment list.
///
/// Returns the row as it now stands, or `None` when there is no such file — a
/// second press of the same key on a chip that has already gone.
///
/// Refused on anything that is not an image, for the reason on
/// [`Attachment::inline`]. The refusal is an error rather than a silent no-op
/// because the control that produced it is only ever drawn on images: reaching
/// this arm means something else is wrong.
pub fn set_inline(db: &Db, attachment_id: &str, inline: bool) -> Result<Option<Attachment>> {
    let Some(existing) = get(db, attachment_id)? else {
        return Ok(None);
    };
    if inline && !existing.is_image() {
        return Err(ComposeError::invalid(format!(
            "{} is not an image, so it cannot go in the body",
            existing.filename
        )));
    }
    if inline && existing.size_bytes as u64 > MAX_INLINE_IMAGE_BYTES {
        return Err(ComposeError::invalid(format!(
            "{} is {} — too large to draw in the message. It is attached instead.",
            existing.filename,
            human_size(existing.size_bytes)
        )));
    }
    if existing.inline == inline {
        return Ok(Some(existing));
    }
    db.write(|conn| {
        conn.execute(
            "UPDATE compose_attachments SET inline = ?2 WHERE id = ?1",
            params![attachment_id, inline as i64],
        )?;
        Ok(())
    })?;
    get(db, attachment_id)
}

/// The bytes of one file, for showing an inline image in the composer as the
/// recipient will see it. Metadata comes back with it so the caller does not
/// need a second lookup to build a `data:` URL.
pub fn bytes_of(db: &Db, attachment_id: &str) -> Result<Option<(Attachment, Vec<u8>)>> {
    db.write(ensure_compose_schema)?;
    Ok(db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT id, draft_id, filename, mime_type, size_bytes, added_at,
                        inline, content_id, bytes
                   FROM compose_attachments WHERE id = ?1",
                [attachment_id],
                |row| Ok((map_attachment(row)?, row.get::<_, Vec<u8>>(8)?)),
            )
            .optional()?)
    })?)
}

/// A `Content-ID` for a new attachment.
///
/// The local part is the row's own id, so the header, the `cid:` in the body
/// and the row are one string apart and a mismatch is impossible to introduce.
/// The domain is `mach.invalid` rather than the sender's: RFC 2392 addresses a
/// part *within this message*, nothing dereferences it, and a real domain in a
/// `cid:` is what makes some scanners try to fetch it.
fn content_id_for(attachment_id: &str) -> String {
    format!("{attachment_id}@mach.invalid")
}

/// What a draft is carrying, oldest first — the order they were chosen in, which
/// is the order the composer lists them and the order they ride in the message.
pub fn list(db: &Db, draft_id: &str) -> Result<Vec<Attachment>> {
    db.write(ensure_compose_schema)?;
    Ok(db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, draft_id, filename, mime_type, size_bytes, added_at, inline, content_id
               FROM compose_attachments WHERE draft_id = ?1 ORDER BY added_at, id",
        )?;
        let rows = stmt.query_map([draft_id], map_attachment)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })?)
}

/// Metadata and bytes together — what building the message needs.
pub fn list_with_bytes(db: &Db, draft_id: &str) -> Result<Vec<(Attachment, Vec<u8>)>> {
    db.write(ensure_compose_schema)?;
    Ok(db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, draft_id, filename, mime_type, size_bytes, added_at, inline, content_id,
                    bytes
               FROM compose_attachments WHERE draft_id = ?1 ORDER BY added_at, id",
        )?;
        let rows = stmt.query_map([draft_id], |row| {
            Ok((map_attachment(row)?, row.get::<_, Vec<u8>>(8)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })?)
}

pub fn total_bytes(db: &Db, draft_id: &str) -> Result<i64> {
    db.write(ensure_compose_schema)?;
    Ok(db.read(|conn| {
        Ok(conn.query_row(
            "SELECT COALESCE(SUM(size_bytes), 0) FROM compose_attachments WHERE draft_id = ?1",
            [draft_id],
            |row| row.get(0),
        )?)
    })?)
}

/// Take one file off a draft. Returns false when it was not there — a second
/// press of the same key, or a draft that has already been sent.
pub fn remove(db: &Db, attachment_id: &str) -> Result<bool> {
    db.write(ensure_compose_schema)?;
    Ok(db.write(|conn| {
        Ok(conn.execute(
            "DELETE FROM compose_attachments WHERE id = ?1",
            [attachment_id],
        )? > 0)
    })?)
}

/// Everything a draft was carrying, gone with it.
///
/// Called from the one place that forgets a draft, so a discarded or sent draft
/// cannot leave 25 MB behind in the store with nothing pointing at it.
pub fn delete_for_draft(db: &Db, draft_id: &str) -> Result<()> {
    db.write(ensure_compose_schema)?;
    db.write(|conn| {
        conn.execute(
            "DELETE FROM compose_attachments WHERE draft_id = ?1",
            [draft_id],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// Move a draft's files to another draft id.
///
/// Adoption and reconciliation can rename a draft row; without this the files
/// would be stranded under an id nothing reads.
pub fn reassign(db: &Db, from_draft_id: &str, to_draft_id: &str) -> Result<()> {
    if from_draft_id == to_draft_id {
        return Ok(());
    }
    db.write(ensure_compose_schema)?;
    db.write(|conn| {
        conn.execute(
            "UPDATE compose_attachments SET draft_id = ?2 WHERE draft_id = ?1",
            params![from_draft_id, to_draft_id],
        )?;
        Ok(())
    })?;
    Ok(())
}

pub fn get(db: &Db, attachment_id: &str) -> Result<Option<Attachment>> {
    db.write(ensure_compose_schema)?;
    Ok(db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT id, draft_id, filename, mime_type, size_bytes, added_at, inline, content_id
                   FROM compose_attachments WHERE id = ?1",
                [attachment_id],
                map_attachment,
            )
            .optional()?)
    })?)
}

fn map_attachment(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attachment> {
    let id: String = row.get(0)?;
    // A row written before this column existed has neither flag nor id. The
    // fallback derives the same string `content_id_for` would have: an old
    // attachment turned inline is then addressable without a backfill.
    let content_id: String = row.get::<_, Option<String>>(7)?.unwrap_or_default();
    let content_id = if content_id.is_empty() {
        content_id_for(&id)
    } else {
        content_id
    };
    Ok(Attachment {
        draft_id: row.get(1)?,
        filename: row.get(2)?,
        mime_type: row.get(3)?,
        size_bytes: row.get(4)?,
        added_at: row.get(5)?,
        inline: row.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
        content_id,
        id,
    })
}

/// A Content-Type from the extension.
///
/// Sniffing the bytes would be better and is what the receive side does for
/// images, where the sender's word is worth nothing. Here the owner chose the
/// file off his own disk, and the extension is what every other mail client
/// uses; guessing differently from the rest of the world would only mean the
/// recipient's client opens the file with the wrong program.
pub fn mime_for(filename: &str) -> String {
    let extension = names::extension_of(filename).unwrap_or_default();
    let mime = match extension.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "svg" => "image/svg+xml",
        "txt" | "log" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => "application/json",
        "xml" => "application/xml",
        "ics" => "text/calendar; charset=utf-8",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    };
    mime.to_string()
}

pub fn human_size(bytes: i64) -> String {
    const MIB: i64 = 1024 * 1024;
    const KIB: i64 = 1024;
    if bytes >= MIB {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{} KB", bytes / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

/// Enough variation to keep two files attached in the same millisecond apart.
fn entropy(now: i64) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    (now as u64).rotate_left(11) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}
