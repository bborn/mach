//! Attachments: fetch the bytes, cache them, and hand them to the OS.
//!
//! | command | argument | returns |
//! |---|---|---|
//! | `attachment_open` | `attachmentId` | [`AttachmentFile`] |
//! | `attachment_save` | `attachmentId` | [`SavedAttachment`] |
//! | `attachment_inline_image` | `messageId`, `contentId` | [`InlineImage`] |
//!
//! # Nothing here happens on its own
//!
//! Every one of these commands is reached from a keystroke or a click on the
//! attachment the reader is looking at. There is no prefetch, no "download when
//! the thread opens", and no speculative fetch of inline images for a collapsed
//! message. That is a security property, not a performance one: the moment
//! bytes are fetched and written without the reader asking, "did you open the
//! attachment" stops being a question with an answer, and the only reason this
//! module is allowed to write attacker-controlled bytes to disk at all is that
//! a human asked for these specific bytes.
//!
//! Opening is stricter still — see [`store::names::is_dangerous`] for what is
//! refused and why the refusal has no "open anyway".
//!
//! # Where the module lives
//!
//! The cache and the name sanitizer are `src-tauri/src/attachments/`, declared
//! below with `#[path]` rather than in `lib.rs`, for exactly the reason
//! [`super::compose`] does the same thing: `lib.rs` belongs to another unit
//! while both are being built. Promoting `pub mod attachments;` to the crate
//! root later is a one-line change that makes `ipc::attachments::store` an
//! alias.

#[path = "../attachments/mod.rs"]
pub mod store;

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use rusqlite::OptionalExtension;
use serde::Serialize;
use tauri::State;

use crate::commands::CommandError;
use crate::db::Db;
use crate::google::gmail::{GmailClient, MessageFormat};
use crate::google::types::AttachmentMeta;
use crate::google::GoogleError;

use store::names;
use store::{AttachmentCache, PartKind, MAX_ATTACHMENT_BYTES, MAX_INLINE_IMAGE_BYTES};

use super::error::IpcError;
use super::state::AppState;

/// Gmail addresses the authorised account as `me`, the same way the command
/// layer does.
const USER_ID: &str = "me";

/// How long a click on an attachment may go unanswered before it becomes an
/// error the reader can read.
///
/// # What it covers, and what it provably does not
///
/// It covers the network. Nothing in this app sets a timeout on a `reqwest`
/// client, so a connection that stalls after the headers stalls a click on an
/// attachment for as long as the socket stays open — with a spinner on the chip
/// and no way for the reader to learn that nothing is happening.
///
/// It does **not** cover the failure that was actually caught here. Opening a
/// PDF from the owner's own mailbox hung indefinitely, and the sample says why:
///
/// ```text
/// attachment_open → materialise → attachment_get_capped → RestClient::send
///   → ManagedToken::access_token → KeychainTokenStore::load_refresh_token
///   → SecKeychainFindGenericPassword → __psynch_mutexwait
/// ```
///
/// The token read is a **blocking** Keychain call sitting inside an `async`
/// poll. When macOS decides the build needs the user's approval it parks in
/// `securityd` until a dialog is answered, and a dialog cannot be answered
/// while the app is not able to show one. A `timeout` cannot help there: the
/// task is stuck *inside* a poll, so it is never polled again and the timer
/// never gets to fire. Worse, every other token read then queues on the same
/// lock, which is how one unanswered prompt takes the whole app's network side
/// — sync included — with it.
///
/// So this is a bound on the half that can be bounded, and the other half is a
/// note to whoever owns `auth::tokens`: the Keychain must not be read from a
/// runtime worker on the request path. It is the same lesson as "never read the
/// Keychain on the launch thread", one layer further in.
///
/// Two minutes is longer than any legitimate attachment fetch (Gmail will not
/// deliver over 50 MB, and this cache refuses over 64 MiB) and far shorter than
/// "never".
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Run a fetch under [`FETCH_TIMEOUT`], turning a stall into a sentence.
async fn within_deadline<T>(
    what: &str,
    future: impl std::future::Future<Output = Result<T, IpcError>>,
) -> Result<T, IpcError> {
    match tokio::time::timeout(FETCH_TIMEOUT, future).await {
        Ok(result) => result,
        Err(_) => Err(refused(format!(
            "{what} did not arrive within {} seconds, so Mach stopped waiting. \
             Nothing was downloaded — try again.",
            FETCH_TIMEOUT.as_secs()
        ))),
    }
}

// ===========================================================================
// Payloads
// ===========================================================================

/// One attachment, materialised on disk.
///
/// `filename` is the **sanitized** name, not the sender's, because it is the
/// name the file actually has and showing anything else would be showing the
/// reader a name that does not exist.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentFile {
    pub attachment_id: i64,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    /// Absolute path inside the cache. Handed to the frontend for diagnostics
    /// and for the "Copy path" affordance; it is never used to reach back into
    /// the filesystem from JavaScript, which cannot do that anyway.
    pub path: String,
    /// True when the bytes were already on disk — i.e. nothing was downloaded.
    pub from_cache: bool,
}

/// The result of a save. `path` is `None` when the user cancelled the panel,
/// which is an outcome and not an error.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAttachment {
    pub path: Option<String>,
    pub filename: String,
}

/// One resolved `cid:` image, ready to be spliced into a message frame.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineImage {
    pub content_id: String,
    /// The **sniffed** type, never the sender's declared one.
    pub mime_type: String,
    /// Standard base64 (not base64url) — this becomes a `data:` URL.
    pub base64: String,
}

// ===========================================================================
// Commands
// ===========================================================================

/// Download if needed, then hand the file to the system handler.
#[tauri::command(async)]
pub async fn attachment_open(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    attachment_id: i64,
) -> Result<AttachmentFile, IpcError> {
    let file = materialise(&state, attachment_id).await?;

    // Checked here rather than only at fetch time: the refusal is about handing
    // the path to LaunchServices, and that is what this command does. It is
    // also checked on a cache hit, which is the case that matters — the second
    // open of an attachment does no fetching at all, so a check that lived on
    // the download path would run exactly once and then never again.
    if names::is_dangerous(&file.filename, &file.mime_type) {
        return Err(refused(format!(
            "Mach will not open {} — it is a program, not a document. Save it and open it \
             from Finder if you are sure.",
            file.filename
        )));
    }

    // The same question asked of the bytes. Both halves of `is_dangerous` read
    // something the sender wrote; this reads what the sender sent, which is the
    // only way to catch a part named `invoice`, declared `application/pdf`, and
    // carrying a Mach-O header.
    if let Some(what) = head_of(&file.path).as_deref().and_then(names::sniff_executable) {
        return Err(refused(format!(
            "Mach will not open {} — whatever it is called, it is {}. Save it and open it \
             from Finder if you are sure.",
            file.filename, what
        )));
    }

    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(file.path.clone(), None::<&str>)
        .map_err(|e| {
            eprintln!("attachment_open failed for {}: {e}", file.path);
            IpcError::internal(format!("could not open {}: {e}", file.filename))
        })?;

    Ok(file)
}

/// Download if needed, ask where to put it, and copy it there.
#[tauri::command(async)]
pub async fn attachment_save(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    attachment_id: i64,
) -> Result<SavedAttachment, IpcError> {
    let file = materialise(&state, attachment_id).await?;
    let source = PathBuf::from(&file.path);
    let suggested = file.filename.clone();

    let chosen = save_panel(&app, suggested.clone()).await?;
    let Some(destination) = chosen else {
        return Ok(SavedAttachment {
            path: None,
            filename: suggested,
        });
    };

    std::fs::copy(&source, &destination).map_err(|e| {
        IpcError::internal(format!(
            "could not write {}: {e}",
            destination.display()
        ))
    })?;

    Ok(SavedAttachment {
        path: Some(destination.to_string_lossy().into_owned()),
        filename: suggested,
    })
}

/// Resolve one `cid:` reference from a message body to actual image bytes.
///
/// The reading pane calls this once per distinct `data-mach-cid` the sanitizer
/// left behind, for a message the reader has expanded.
#[tauri::command(async)]
pub async fn attachment_inline_image(
    state: State<'_, AppState>,
    message_id: i64,
    content_id: String,
) -> Result<InlineImage, IpcError> {
    if !names::is_valid_content_id(&content_id) {
        return Err(refused(format!(
            "{content_id:?} is not a Content-ID this message could contain"
        )));
    }

    let identity = message_identity(&state.db, message_id)?;
    let cache = cache_for(&state);
    let key = store::cache_key(
        identity.account_id,
        &identity.gmail_message_id,
        PartKind::Inline,
        &content_id,
    );

    if let Some(hit) = cache.find(&key) {
        if let Some(image) = read_cached_image(&hit.path, &content_id) {
            return Ok(image);
        }
        // The entry is there but is not something we can serve — a stale entry
        // from a build that stored a type we no longer accept. Fall through and
        // refetch; `store` clears the directory first.
    }

    let gmail = gmail_for(&state, identity.account_id)?;
    // Under the same deadline as a file, and for the same reason: the reading
    // pane asks for these images the moment a message is expanded, so a stalled
    // token read here would leave one hung request per inline image for as long
    // as the app is open.
    let bytes = within_deadline(&format!("the image {content_id}"), async {
        let message = gmail
            .messages_get(USER_ID, &identity.gmail_message_id, MessageFormat::Full)
            .await
            .map_err(google)?;
        let body = message.extract_body();

        let part = body
            .attachments
            .iter()
            .find(|part| matches_content_id(part, &content_id))
            .ok_or_else(|| {
                refused(format!(
                    "this message has no part with Content-ID {content_id}"
                ))
            })?;

        part_bytes(&gmail, &identity.gmail_message_id, part, MAX_INLINE_IMAGE_BYTES).await
    })
    .await?;

    // The sender's Content-Type is not consulted. An inline part becomes a
    // `data:` URL inside the message frame, and `render::sanitize` refuses SVG
    // there for good reasons; letting a declared type decide would be a way
    // around that decision rather than an agreement with it.
    let mime = names::sniff_raster_image(&bytes).ok_or_else(|| {
        refused(format!(
            "the part with Content-ID {content_id} is not an image Mach will render inline"
        ))
    })?;

    let filename = format!("inline.{}", names::raster_extension(mime));
    // A cache write failure is not fatal: the image can still be shown, it will
    // just be fetched again next time.
    let _ = cache.store(&key, &filename, &bytes);

    Ok(InlineImage {
        content_id,
        mime_type: mime.to_string(),
        base64: STANDARD.encode(&bytes),
    })
}

// ===========================================================================
// The fetch path
// ===========================================================================

/// The row the commands work from. Joined against `messages` because the Gmail
/// message id and the account are what a fetch needs and neither lives on the
/// attachment row.
#[derive(Debug, Clone)]
struct AttachmentRow {
    id: i64,
    account_id: i64,
    gmail_message_id: String,
    gmail_attachment_id: Option<String>,
    filename: String,
    mime_type: String,
    size_bytes: i64,
}

#[derive(Debug, Clone)]
struct MessageIdentity {
    account_id: i64,
    gmail_message_id: String,
}

/// Bytes on disk for one attachment, downloading them only if they are not
/// already there.
async fn materialise(state: &AppState, attachment_id: i64) -> Result<AttachmentFile, IpcError> {
    let row = attachment_row(&state.db, attachment_id)?;
    let filename = names::safe_filename(&row.filename);
    let cache = cache_for(state);

    // A part with no attachment id had its bytes inlined in the sync response,
    // so there is no handle to fetch by and the part has to be found in the
    // message. It still gets a stable key: the part id if Gmail gave one, the
    // filename and size otherwise, which is the most identity such a part has.
    let part_id = row
        .gmail_attachment_id
        .clone()
        .unwrap_or_else(|| format!("inline:{}:{}", row.size_bytes, row.filename));
    let key = store::cache_key(
        row.account_id,
        &row.gmail_message_id,
        PartKind::File,
        &part_id,
    );

    if let Some(hit) = cache.find(&key) {
        return Ok(AttachmentFile {
            attachment_id: row.id,
            filename,
            mime_type: row.mime_type,
            size_bytes: hit.size_bytes as i64,
            path: hit.path.to_string_lossy().into_owned(),
            from_cache: true,
        });
    }

    // The cheapest possible refusal: the size we already synced. It came from
    // the same sender as everything else here, so it is a hint and not a
    // guarantee — which is why the fetch is capped as well.
    if row.size_bytes > MAX_ATTACHMENT_BYTES as i64 {
        return Err(refused(format!(
            "{} is {} — larger than the {} MiB Mach will download",
            filename,
            human_size(row.size_bytes),
            MAX_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }

    let gmail = gmail_for(state, row.account_id)?;
    let bytes = within_deadline(&filename, async {
        match &row.gmail_attachment_id {
            Some(id) => gmail
                .attachment_get_capped(USER_ID, &row.gmail_message_id, id, MAX_ATTACHMENT_BYTES)
                .await
                .map_err(google),
            None => inline_part_bytes(&gmail, &row).await,
        }
    })
    .await?;

    let stored = cache.store(&key, &filename, &bytes).map_err(|e| {
        IpcError::internal(format!("could not cache {filename}: {e}"))
    })?;

    Ok(AttachmentFile {
        attachment_id: row.id,
        filename,
        mime_type: row.mime_type,
        size_bytes: stored.size_bytes as i64,
        path: stored.path.to_string_lossy().into_owned(),
        from_cache: false,
    })
}

/// The fallback for a part Gmail inlined rather than handing out an id for.
/// Re-fetches the message and matches on the two things the row still knows.
async fn inline_part_bytes(gmail: &GmailClient, row: &AttachmentRow) -> Result<Vec<u8>, IpcError> {
    let message = gmail
        .messages_get(USER_ID, &row.gmail_message_id, MessageFormat::Full)
        .await
        .map_err(google)?;
    let body = message.extract_body();
    let part = body
        .attachments
        .iter()
        .find(|part| part.filename == row.filename && part.size == row.size_bytes)
        .or_else(|| {
            body.attachments
                .iter()
                .find(|part| part.filename == row.filename)
        })
        .ok_or_else(|| {
            refused(format!(
                "{} is no longer part of this message on Gmail",
                row.filename
            ))
        })?;

    part_bytes(gmail, &row.gmail_message_id, part, MAX_ATTACHMENT_BYTES).await
}

/// The bytes behind one MIME part, whichever way Gmail chose to deliver them.
async fn part_bytes(
    gmail: &GmailClient,
    gmail_message_id: &str,
    part: &AttachmentMeta,
    max_bytes: usize,
) -> Result<Vec<u8>, IpcError> {
    if let Some(data) = &part.data {
        if data.len() > max_bytes {
            return Err(refused(format!(
                "{} is larger than the {} MiB Mach will hold",
                if part.filename.is_empty() {
                    "that part"
                } else {
                    &part.filename
                },
                max_bytes / (1024 * 1024)
            )));
        }
        return Ok(data.clone());
    }

    let id = part.attachment_id.as_deref().ok_or_else(|| {
        refused("that part of the message carries no bytes and no way to fetch them")
    })?;

    gmail
        .attachment_get_capped(USER_ID, gmail_message_id, id, max_bytes)
        .await
        .map_err(google)
}

/// Does this part answer to `content_id`?
///
/// Exact first, because a Content-ID is a case-sensitive addr-spec and two
/// parts in one message can differ only in case. Case-insensitively second,
/// because plenty of mailers do not know that.
fn matches_content_id(part: &AttachmentMeta, content_id: &str) -> bool {
    match part.content_id.as_deref() {
        Some(id) => id == content_id || id.eq_ignore_ascii_case(content_id),
        None => false,
    }
}

/// The first few bytes of a file, for [`names::sniff_executable`].
///
/// Reads the head rather than the file: the magic numbers are four bytes, the
/// attachment may be 64 MiB, and a read failure is not a reason to refuse — an
/// unreadable file will fail at `open_path` a moment later with a better error.
fn head_of(path: &str) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut head = Vec::with_capacity(8);
    // `take` rather than one `read`: a single `read` is allowed to return short
    // even when more is available, and a short read here would silently mean
    // "not a program".
    file.take(8).read_to_end(&mut head).ok()?;
    Some(head)
}

/// A cached inline image, if the file is still one we would serve.
fn read_cached_image(path: &Path, content_id: &str) -> Option<InlineImage> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    names::raster_mime(&extension)?;
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_INLINE_IMAGE_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    // Sniffed again on the way out. The cache directory is ours, but "ours" is
    // a directory on a disk other processes can write to, and the cost of being
    // wrong is a `data:` URL in a message frame.
    let mime = names::sniff_raster_image(&bytes)?;
    Some(InlineImage {
        content_id: content_id.to_string(),
        mime_type: mime.to_string(),
        base64: STANDARD.encode(&bytes),
    })
}

// ===========================================================================
// The save panel
// ===========================================================================

/// Show the system save panel and return what the user chose.
///
/// # The plugin is registered here, at runtime
///
/// Nothing about the panel is reachable from JavaScript: the capability file
/// grants no `dialog:` permissions, and this calls the Rust API directly. The
/// only thing the frontend can ask for is "save *this* attachment".
async fn save_panel(
    app: &tauri::AppHandle,
    suggested_name: String,
) -> Result<Option<PathBuf>, IpcError> {
    let app = app.clone();
    // The panel blocks its thread until the user answers, which can be minutes.
    // A Tokio worker is not the place for that.
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app.dialog()
            .file()
            .set_file_name(&suggested_name)
            .blocking_save_file()
    })
    .await
    .map_err(|e| IpcError::internal(format!("the save panel did not answer: {e}")))?;

    let Some(chosen) = chosen else {
        return Ok(None);
    };

    chosen
        .into_path()
        .map(Some)
        .map_err(|e| IpcError::internal(format!("that is not a path Mach can write to: {e}")))
}

// ===========================================================================
// Store access
// ===========================================================================

fn cache_for(state: &AppState) -> AttachmentCache {
    // Beside the database, so `MACH_DATA_DIR` scopes the cache the same way it
    // scopes everything else.
    let data_dir = state
        .config
        .database_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    AttachmentCache::new(data_dir)
}

fn gmail_for(state: &AppState, account_id: i64) -> Result<GmailClient, IpcError> {
    state
        .dispatcher
        .clients
        .gmail(account_id)
        .map_err(IpcError::from)
}

fn attachment_row(db: &Db, attachment_id: i64) -> Result<AttachmentRow, IpcError> {
    let found = db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT a.id, m.account_id, m.gmail_message_id, a.gmail_attachment_id, \
                        a.filename, a.mime_type, a.size_bytes \
                 FROM attachments a JOIN messages m ON m.id = a.message_id \
                 WHERE a.id = ?1",
                [attachment_id],
                |row| {
                    Ok(AttachmentRow {
                        id: row.get(0)?,
                        account_id: row.get(1)?,
                        gmail_message_id: row.get(2)?,
                        gmail_attachment_id: row.get(3)?,
                        filename: row.get(4)?,
                        mime_type: row.get(5)?,
                        size_bytes: row.get(6)?,
                    })
                },
            )
            .optional()?)
    })?;

    found.ok_or_else(|| IpcError::not_found("attachment", attachment_id))
}

fn message_identity(db: &Db, message_id: i64) -> Result<MessageIdentity, IpcError> {
    let found = db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT account_id, gmail_message_id FROM messages WHERE id = ?1",
                [message_id],
                |row| {
                    Ok(MessageIdentity {
                        account_id: row.get(0)?,
                        gmail_message_id: row.get(1)?,
                    })
                },
            )
            .optional()?)
    })?;

    found.ok_or_else(|| IpcError::not_found("message", message_id))
}

// ===========================================================================
// Errors
// ===========================================================================

/// A refusal the user can read and act on. `invalid` rather than `internal`:
/// nothing broke, the app declined.
fn refused(message: impl Into<String>) -> IpcError {
    IpcError::Command(CommandError::Invalid {
        message: message.into(),
    })
}

/// Google's own prose is better than anything this layer could invent —
/// "google rate limited (429)" and "google auth failed (401)" already say what
/// happened and what the reader should expect next.
fn google(error: GoogleError) -> IpcError {
    IpcError::Command(CommandError::Invalid {
        message: error.to_string(),
    })
}

fn human_size(bytes: i64) -> String {
    const MIB: i64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    } else {
        format!("{} KB", (bytes / 1024).max(1))
    }
}

// ===========================================================================
// Forwarding
// ===========================================================================

/// Put the forwarded message's files on the forwarding draft.
///
/// # Why this is a second call and not part of `prepare`
///
/// A forward's *text* is free: [`compose::draft::forward_text`] reproduces the
/// original out of rows that are already local, and it happens at build time
/// with nothing to wait for. Its files are not. A synced message carries an
/// attachment's name, type and size but not its bytes — those arrive only when
/// somebody opens or saves one — so forwarding almost always has to fetch, and
/// a fetch belongs nowhere near the keystroke that opens a composer. `prepare`
/// stays a local read and the window opens on it; this follows.
///
/// # What it does not do
///
/// **Inline images are left behind.** They are parts of the body, addressed by
/// `cid:`, and [`forward_html`] reproduces that body as the sender wrote it —
/// including those references. Attaching them again would put every logo and
/// signature graphic in the message a second time, as files, under a chip. A
/// recipient whose client resolves the `cid:` sees the original; one whose
/// client does not sees what Gmail's own forward shows them.
///
/// [`forward_html`]: crate::ipc::compose::engine::draft::forward_html
/// [`compose::draft::forward_text`]: crate::ipc::compose::engine::draft::forward_text
///
/// # Failure is per file
///
/// One attachment Google will not hand back — too large for the cap, a part
/// that is no longer there — must not cost the other three, and must not cost
/// the forward itself. Each is tried on its own and the ones that failed come
/// back named, so the composer can say which files are *not* going and the
/// owner can decide before sending rather than after.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardedFiles {
    /// The draft's attachments as they now stand, in the shape the composer
    /// already draws.
    pub attachments: Vec<crate::ipc::compose::engine::attach::Attachment>,
    /// Files that could not be brought across, by name, with Google's reason.
    pub refused: Vec<String>,
}

#[tauri::command(async)]
pub async fn forward_attachments(
    state: State<'_, AppState>,
    draft_id: String,
    message_id: i64,
) -> Result<ForwardedFiles, IpcError> {
    use crate::ipc::compose::engine::attach;

    // Metadata only: the ids of every file on the message being forwarded.
    //
    // No inline filter, because there is nothing inline in here to filter.
    // `sync::convert` populates this table from `ExtractedBody::files()`, which
    // is defined as the parts a person would call attachments — the `cid:`
    // images the body references are kept out of it and fetched by Content-ID
    // on their own path. So this is already the file list and nothing else.
    let ids: Vec<i64> = state.db.read(|conn| {
        let mut stmt =
            conn.prepare("SELECT id FROM attachments WHERE message_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([message_id], |row| row.get::<_, i64>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })?;

    let now = crate::ipc::compose::now_ms();
    let mut refused: Vec<String> = Vec::new();
    for id in ids {
        match materialise(&state, id).await {
            Ok(file) => match std::fs::read(&file.path) {
                Ok(bytes) => {
                    if let Err(error) =
                        attach::add_bytes(&state.db, &draft_id, &file.filename, &bytes, false, now)
                    {
                        refused.push(format!("{}: {error}", file.filename));
                    }
                }
                Err(error) => refused.push(format!("{}: {error}", file.filename)),
            },
            Err(error) => refused.push(error.to_string()),
        }
    }

    Ok(ForwardedFiles {
        attachments: attach::list(&state.db, &draft_id)?,
        refused,
    })
}
