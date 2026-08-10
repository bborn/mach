//! Drafting and sending. Owned by the composer unit.
//!
//! # One command, several operations
//!
//! `send_message` is the only compose entry point registered in `lib.rs`, and
//! `lib.rs` belongs to nobody while three units are being built at once. So
//! this handler is a small router: the payload carries an `op`, and the shapes
//! below are the contract. An absent `op` means `send`, which keeps the
//! original one-argument signature honest.
//!
//! | `op` | argument | returns |
//! |---|---|---|
//! | `prepare` | `threadId`, `kind` | `{ draft, carrying, refused }` |
//! | `carryForward` | `draftId`, `messageId` | `{ attachments, added, refused }` |
//! | `loadDraft` | `draftId`, `messageId` or `threadId` | `{ draft \| null }` |
//! | `saveDraft` | `draft` | `{ draft }` |
//! | `discardDraft` | `draftId` | `{ ok, remote }` |
//! | `attachChoose` | `draftId`, `inline?` | `{ attachments, added, refused }` |
//! | `attachAdd` | `draftId`, `paths`, `inline?` | `{ attachments, added, refused }` |
//! | `attachRemove` | `attachmentId` | `{ ok, attachments }` |
//! | `attachList` | `draftId` | `{ attachments }` |
//! | `attachInline` | `attachmentId`, `inline` | `{ attachment, attachments }` |
//! | `attachImages` | `draftId` | `{ images }` |
//! | `preview` | `draft` | `{ rfc822, headers }` |
//! | `send` | `draft`, `scheduleAt?` | `{ entry, undoUntil }` |
//! | `undo` | `outboxId` | `{ cancelled }` |
//! | `flush` | `now?` | `{ outcomes, pending }` |
//! | `outbox` | — | `{ pending }` |
//! | `retry` / `discard` | `outboxId` | `{ ok }` |
//!
//! Collapsing these into one command is a constraint, not a design. When the
//! composer's IPC surface is promoted they become ordinary sibling commands and
//! this router disappears; nothing above it changes, because `src/lib/compose.ts`
//! already exposes them as separate functions.
//!
//! # Where the module lives
//!
//! The implementation is `src-tauri/src/compose/`, declared below with
//! `#[path]` rather than in `lib.rs`, for the same reason
//! `command_queries::ensure_command_schema` exists: another unit owns that file
//! today. Moving `pub mod compose;` up to the crate root later is a one-line
//! change that makes `ipc::compose::engine` an alias.

#[path = "../compose/mod.rs"]
pub mod engine;

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};

use engine::draft::{self, Draft, DraftKind};
use engine::mime::build_rfc822;
use engine::outbox::{Outbox, UNDO_WINDOW_MS};
use engine::ComposeError;

use super::attachments::store::{self, AttachmentCache};
use super::error::IpcError;
use super::state::AppState;

/// Gmail addresses the authorised account as `me`, the same way the rest of
/// this layer does.
const USER_ID: &str = "me";

// ---------------------------------------------------------------------------
// the handler
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn send_message(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    draft: Value,
) -> Result<Value, IpcError> {
    let outbox = Outbox::new(
        state.db.clone(),
        Arc::clone(&state.dispatcher.clients),
    )
    .map_err(IpcError::from)?;
    // One operation needs the application itself: choosing files is a system
    // panel, and a panel needs a window to be modal to. Everything else runs
    // through `dispatch`, which is a plain function so the tests can drive it.
    if payload_op(&draft) == "attachChoose" {
        let draft_id = required_str(&draft, "draftId")?;
        let inline = draft.get("inline").and_then(Value::as_bool).unwrap_or(false);
        let paths = open_panel(&app).await?;
        return attach_paths(&state.db, &draft_id, &paths, inline, now_ms())
            .await
            .map_err(Into::into);
    }
    dispatch(&state.db, &outbox, draft).await.map_err(Into::into)
}

fn payload_op(payload: &Value) -> &str {
    payload.get("op").and_then(Value::as_str).unwrap_or("send")
}

/// Read the chosen files and hang them on the draft.
///
/// Shared by the panel and by drag-and-drop, which arrives from the webview as
/// a list of paths and must not be a second, subtly different implementation of
/// "attach these files".
///
/// **The read happens off the runtime.** A 25 MB file off a slow volume is tens
/// of milliseconds of a thread that is not allowed to block: every other IPC
/// call — the keystroke that autosaves the draft this file is going on — is
/// waiting behind it on the same executor. `spawn_blocking` is the whole fix,
/// and it is why this function is async for one `std::fs::read`.
async fn attach_paths(
    db: &crate::db::Db,
    draft_id: &str,
    paths: &[String],
    inline: bool,
    now: i64,
) -> Result<Value, ComposeError> {
    let mut added = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for path in paths {
        let owned = path.clone();
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".to_string());
        // Read first, then cap on what was actually read: the metadata size is
        // a claim about a file that could change between the two calls, and the
        // number that matters is the number of bytes now in memory.
        let read = tauri::async_runtime::spawn_blocking(move || std::fs::read(&owned)).await;
        let bytes = match read {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(error)) => {
                refused.push(format!("{name}: {error}"));
                continue;
            }
            Err(error) => {
                refused.push(format!("{name} could not be read: {error}"));
                continue;
            }
        };
        match engine::attach::add_bytes(db, draft_id, &name, &bytes, inline, now) {
            Ok(attachment) => added.push(attachment),
            Err(error) => refused.push(error.to_string()),
        }
    }
    Ok(json!({
        "attachments": engine::attach::list(db, draft_id)?,
        "added": added,
        "refused": refused,
    }))
}

/// The system open panel, on a thread that is allowed to block.
///
/// Nothing about it is reachable from JavaScript: the capability file grants no
/// `dialog:` permission, and this calls the Rust API directly — the same
/// arrangement, and the same reasoning, as the save panel in
/// [`super::attachments`].
async fn open_panel(app: &tauri::AppHandle) -> Result<Vec<String>, IpcError> {
    let app = app.clone();
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app.dialog().file().blocking_pick_files()
    })
    .await
    .map_err(|e| IpcError::internal(format!("the file panel did not answer: {e}")))?;

    Ok(chosen
        .unwrap_or_default()
        .into_iter()
        .filter_map(|file| file.into_path().ok())
        .map(|path| path.to_string_lossy().into_owned())
        .collect())
}

/// The router, as a plain function over `&Db` — a `#[tauri::command]` cannot be
/// called without an application, so it is not allowed to hold a decision.
/// `tests/compose.rs` drives this.
///
/// The attachment cache is found from the store's own directory, which is where
/// [`super::attachments`] puts it. A test opens an in-memory store and gets
/// `None`, which means "nothing is cached here" and not "cache into the working
/// directory"; a test that wants to *observe* the cache calls [`dispatch_in`]
/// with one of its own.
pub async fn dispatch(
    db: &crate::db::Db,
    outbox: &Outbox,
    payload: Value,
) -> Result<Value, ComposeError> {
    let cache = db.directory().map(AttachmentCache::new);
    dispatch_in(db, outbox, cache.as_ref(), payload).await
}

pub async fn dispatch_in(
    db: &crate::db::Db,
    outbox: &Outbox,
    cache: Option<&AttachmentCache>,
    payload: Value,
) -> Result<Value, ComposeError> {
    let op = payload
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or("send")
        .to_string();
    let now = payload
        .get("now")
        .and_then(Value::as_i64)
        .unwrap_or_else(now_ms);

    match op.as_str() {
        "prepare" => {
            let thread_id = required_i64(&payload, "threadId")?;
            let kind = payload
                .get("kind")
                .and_then(Value::as_str)
                .map(DraftKind::parse)
                .unwrap_or(DraftKind::Reply);
            let id = payload
                .get("draftId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| new_draft_id(now));
            let prepared = draft::prepare(db, thread_id, kind, id)?;
            // What a forward will take with it, decided here so the composer
            // can draw a chip per file the instant it opens — and say which
            // file is too large before a single byte is fetched. A local read;
            // see `compose::forward`. Every other kind carries nothing, so
            // `plan` is not even asked.
            let plan = match (kind, prepared.reply_to_id) {
                (DraftKind::Forward, Some(message_id)) => {
                    engine::forward::plan(db, message_id)?
                }
                _ => engine::forward::Plan::default(),
            };
            Ok(json!({
                "draft": prepared,
                // Whether the fetch below is worth making at all. A message
                // with no files and no pictures answers `false`, and pressing
                // `f` on it costs exactly what it always did.
                "carries": !plan.is_empty(),
                "carrying": plan
                    .files
                    .iter()
                    .map(|file| json!({
                        "filename": file.filename,
                        "mimeType": file.mime_type,
                        "sizeBytes": file.size_bytes,
                    }))
                    .collect::<Vec<Value>>(),
                "refused": plan.refused,
            }))
        }

        // Go and get what the plan said this forward would carry.
        //
        // Its own operation rather than part of `prepare` because it is the
        // only half that can touch the network, and the composer must be on
        // screen before it starts. `prepare` answers from SQLite in a
        // millisecond; this resolves whenever Gmail does, and the chips fill in
        // when it lands.
        "carryForward" => {
            let draft_id = required_str(&payload, "draftId")?;
            let message_id = required_i64(&payload, "messageId")?;
            carry_forward(
                db,
                outbox.clients(),
                cache,
                &draft_id,
                message_id,
                now,
            )
            .await
        }

        // Three keys, narrowest first. `messageId` is the reading pane's: it
        // holds a row that *is* a draft and needs the editable copy behind it,
        // and a thread can carry two drafts, so the thread-keyed lookup would
        // hand back whichever was touched last rather than the one activated.
        //
        // It is also the one that can *write*: a draft written in another client
        // has no editable copy here until the first time it is opened, and that
        // is what `draft::load_draft_for_message` adopts.
        "loadDraft" => {
            let found = if let Some(id) = payload.get("draftId").and_then(Value::as_str) {
                draft::load_draft(db, id)?
            } else if let Some(message_id) = payload.get("messageId").and_then(Value::as_i64) {
                draft::load_draft_for_message(db, message_id, now)?
            } else if let Some(thread_id) = payload.get("threadId").and_then(Value::as_i64) {
                draft::load_draft_for_thread(db, thread_id)?
            } else {
                return Err(ComposeError::invalid(
                    "loadDraft needs a draftId, a messageId or a threadId",
                ));
            };
            Ok(json!({ "draft": found }))
        }

        // Three writes, in the order the invariant demands: the draft row, the
        // mirror that puts it in the Drafts mailbox, and only then Gmail —
        // which is spawned, not awaited, so this call costs SQLite and nothing
        // else. See `compose::mirror` and `compose::remote`.
        "saveDraft" => {
            let parsed = parse_draft(&payload)?;
            // A draft that has been sent or discarded is not saved again, and
            // the mirror below is the reason this is checked *here* rather than
            // left to `save_draft`: writing the row back is only half of a
            // resurrection, and the other half — a `DRAFT` message in the
            // conversation with nothing behind it — is the half the owner sees.
            // The composer's last autosave routinely lands after `⌘⏎`.
            if draft::is_retired(db, &parsed.id)? {
                return Ok(json!({ "draft": parsed }));
            }
            let saved = draft::save_draft(db, &parsed, now)?;
            let thread_id = engine::mirror::mirror(db, &saved, now)?;
            // A draft started from nothing now has a conversation to live in,
            // and the row has to know which — otherwise reopening it from the
            // Drafts mailbox would find no draft on that thread.
            let saved = if saved.thread_id.is_none() {
                draft::save_draft(
                    db,
                    &Draft {
                        thread_id: Some(thread_id),
                        ..saved
                    },
                    now,
                )?
            } else {
                saved
            };
            engine::remote::spawn_push(db.clone(), outbox.clients(), saved.id.clone(), now);
            Ok(json!({ "draft": saved }))
        }

        // Throw a draft away, everywhere it exists.
        //
        // The remote half is **awaited** here, unlike the one on the send path.
        // Both orders have a failure mode and only one of them is honest: the
        // local rows go first so the UI never waits on Google, and then the
        // `drafts.delete` is waited for, because if it fails the draft is still
        // on his phone — and the next sync pass, finding a `DRAFT` message with
        // no local draft row, will adopt it right back into the conversation he
        // just cleared. That is worth a sentence on screen rather than silence.
        "discardDraft" => {
            let id = required_str(&payload, "draftId")?;
            let existing = draft::load_draft(db, &id)?;
            forget_draft_locally(db, &id, now)?;
            let Some(remote_id) = existing.as_ref().and_then(|d| d.remote.draft_id.clone()) else {
                return Ok(json!({ "ok": true, "remote": "none" }));
            };
            let account_id = existing.map(|d| d.account_id).unwrap_or_default();
            match engine::remote::DraftRemoteSync::new(db.clone(), outbox.clients())
                .delete(&remote_id, account_id)
                .await
            {
                Ok(()) => Ok(json!({ "ok": true, "remote": "deleted" })),
                Err(error) => Ok(json!({
                    "ok": true,
                    "remote": "failed",
                    "error": error.to_string(),
                })),
            }
        }

        // Attach files already on disk. The panel route goes through
        // `send_message` because it needs the application; this is what
        // drag-and-drop uses, and what a test can call.
        "attachAdd" => {
            let draft_id = required_str(&payload, "draftId")?;
            let paths: Vec<String> = payload
                .get("paths")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let inline = payload
                .get("inline")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            attach_paths(db, &draft_id, &paths, inline, now).await
        }

        // Move one file between the body and the list under the message.
        //
        // The whole list comes back, not just the one row: the composer draws
        // the chips from it, and reconciling one changed member into an array
        // it already holds is a second place for the two to disagree.
        "attachInline" => {
            let id = required_str(&payload, "attachmentId")?;
            let inline = payload
                .get("inline")
                .and_then(Value::as_bool)
                .ok_or_else(|| ComposeError::invalid("inline is required"))?;
            let attachment = engine::attach::set_inline(db, &id, inline)?;
            let draft_id = attachment
                .as_ref()
                .map(|a| a.draft_id.clone())
                .unwrap_or_default();
            Ok(json!({
                "attachment": attachment,
                "attachments": engine::attach::list(db, &draft_id)?,
            }))
        }

        // The bytes of every inline image on a draft, so the composer can draw
        // the message the way it will arrive.
        //
        // Only the inline ones. An attached file is a name and a size in the
        // composer, and shipping 25 MB of base64 over IPC to render a chip that
        // says "invoice.pdf" would be the same mistake as putting the bytes on
        // the draft object.
        //
        // The encoding is off the runtime for the reason `attach_paths` reads
        // its files off it: base64 inflates by a third, and turning several
        // megabytes into a string is CPU that every other IPC call would be
        // queued behind — including the autosave of the draft being looked at.
        // `MAX_INLINE_IMAGE_BYTES` caps what can arrive here at 4 MB an image.
        "attachImages" => {
            let draft_id = required_str(&payload, "draftId")?;
            let inline: Vec<_> = engine::attach::list_with_bytes(db, &draft_id)?
                .into_iter()
                .filter(|(meta, _)| meta.inline)
                .collect();
            let images = tauri::async_runtime::spawn_blocking(move || {
                inline
                    .into_iter()
                    .map(|(meta, bytes)| {
                        json!({
                            "attachmentId": meta.id,
                            "contentId": meta.content_id,
                            "mimeType": meta.mime_type,
                            "filename": meta.filename,
                            "base64": BASE64.encode(&bytes),
                        })
                    })
                    .collect::<Vec<Value>>()
            })
            .await
            .map_err(|e| ComposeError::invalid(format!("the images could not be read: {e}")))?;
            Ok(json!({ "images": images }))
        }

        "attachRemove" => {
            let id = required_str(&payload, "attachmentId")?;
            let attachment = engine::attach::get(db, &id)?;
            let removed = engine::attach::remove(db, &id)?;
            let draft_id = attachment.map(|a| a.draft_id).unwrap_or_default();
            Ok(json!({
                "ok": removed,
                "attachments": engine::attach::list(db, &draft_id)?,
            }))
        }

        "attachList" => {
            let draft_id = required_str(&payload, "draftId")?;
            Ok(json!({ "attachments": engine::attach::list(db, &draft_id)? }))
        }

        // The generated message, without sending it. Exists because "is the
        // threading right?" is a question about bytes, and the only honest way
        // to answer it is to look at them.
        "preview" => {
            let parsed = parse_draft(&payload)?;
            let built = draft::build(db, &parsed, now, entropy(now))?;
            let bytes = build_rfc822(&built.outgoing)?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let headers = text.split("\r\n\r\n").next().unwrap_or_default().to_string();
            Ok(json!({
                "rfc822": text,
                "headers": headers,
                "gmailThreadId": built.gmail_thread_id,
            }))
        }

        "send" => {
            let parsed = parse_draft(&payload)?;
            // The text comes from the editor; **the Gmail identity comes from
            // the row**. The composer holds a draft object it rebuilt from its
            // own state, and the push that gave the draft an id may have landed
            // after the last copy it was handed — so trusting the payload here
            // would send a draft-backed message down the `messages.send` road
            // and leave the draft behind, which is the bug. `save_draft` has
            // never let the editor write these columns for the same reason.
            let parsed = match draft::load_draft(db, &parsed.id)? {
                Some(stored) => Draft {
                    remote: stored.remote,
                    ..parsed
                },
                None => parsed,
            };
            if parsed.to.is_empty() && parsed.cc.is_empty() && parsed.bcc.is_empty() {
                return Err(ComposeError::invalid("this message has no recipients"));
            }
            let built = draft::build(db, &parsed, now, entropy(now))?;
            // A scheduled send and an ordinary one differ only in this number.
            let send_after = payload
                .get("scheduleAt")
                .and_then(Value::as_i64)
                .filter(|at| *at > now)
                .unwrap_or(now + UNDO_WINDOW_MS);
            let entry = outbox.queue(&built, now, send_after)?;
            // The draft has become a message; keeping it would re-open an empty
            // composer on the thread that now contains the reply.
            //
            // **Local only, and that is the change.** The Gmail draft is not
            // deleted here, because it is not litter — it is the thing that
            // will be sent. `outbox` carries its id and calls `drafts.send`,
            // which sends and removes it in one request. Deleting it now would
            // leave nothing to send in ten seconds; deleting it *after* the
            // send, as this used to, is the two-call arrangement whose failure
            // mode is the duplicate draft in the owner's mailbox.
            //
            // The order matters against the drafts sweep in `sync::mail`. The
            // local row goes first, so the sweep — which only ever reaps drafts
            // it can see in `compose_drafts` — cannot act on this draft at all
            // while it is in flight. Gmail still lists it, so nothing on that
            // side looks deleted either; the two ends agree until `drafts.send`
            // changes both at once.
            forget_draft_locally(db, &parsed.id, now)?;
            Ok(json!({
                "entry": entry,
                "undoUntil": send_after,
                "scheduled": send_after > now + UNDO_WINDOW_MS,
            }))
        }

        "undo" => {
            let id = required_str(&payload, "outboxId")?;
            let cancelled = outbox.cancel(&id)?;
            Ok(json!({ "cancelled": cancelled }))
        }

        "flush" => {
            // Drafts written while the network was down are pushed here too.
            // The frontend flushes on mount, so "the app was offline yesterday"
            // resolves itself at launch rather than staying local for ever.
            let pushed = engine::remote::DraftRemoteSync::new(db.clone(), outbox.clients())
                .push_pending(now)
                .await
                .unwrap_or(0);
            let outcomes = outbox.flush_due(now).await?;
            // A day is long enough for "sent" to have been seen and short
            // enough that the table does not become a mail archive. The
            // tombstones go with them: they exist to beat an autosave by a few
            // hundred milliseconds, and a day later a save with that id could
            // only be a new draft.
            let _ = outbox.forget_sent(now - 86_400_000);
            let _ = draft::forget_retired_before(db, now - 86_400_000);
            Ok(json!({
                "outcomes": outcomes,
                "pending": outbox.pending()?,
                "draftsPushed": pushed,
            }))
        }

        "outbox" => Ok(json!({ "pending": outbox.pending()?, "all": outbox.list()? })),

        "retry" => {
            let id = required_str(&payload, "outboxId")?;
            Ok(json!({ "ok": outbox.retry(&id, now)? }))
        }

        "discard" => {
            let id = required_str(&payload, "outboxId")?;
            Ok(json!({ "ok": outbox.discard(&id)? }))
        }

        // Handled one level up, in `send_message`, because a system panel needs
        // the application. Saying so beats "unknown operation" for the next
        // person who calls `dispatch` directly from a test.
        "attachChoose" => Err(ComposeError::invalid(
            "attachChoose needs the application; call send_message, or attachAdd with paths",
        )),

        other => Err(ComposeError::invalid(format!(
            "unknown compose operation {other:?}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// what a forward carries
// ---------------------------------------------------------------------------

/// Put the original's files and pictures on the forward.
///
/// # Where the bytes come from, in order of preference
///
/// 1. **The local attachment cache**, if the owner has already opened or saved
///    this file. Same directory, same key — see [`store::file_part_id`] — so a
///    file read a minute ago is not downloaded a second time to be re-sent.
/// 2. **Gmail**, by `attachmentId` where there is one, and otherwise out of a
///    `messages.get`, which is the only way to reach a part whose bytes were
///    inlined in the sync response. Whatever is fetched is written into the
///    cache on the way past, so the reader gets the same saving in reverse.
///
/// There is no third option. Gmail has no call that would let a send reference
/// a part of another message: `drafts.create` and `messages.send` take a
/// complete RFC822 message in `raw` and nothing else, and `attachmentId` is a
/// handle for downloading. So the bytes make the round trip. See the doc on
/// [`engine::forward`].
///
/// # Nothing here is fatal
///
/// One file that cannot be fetched must not cost the other three, and must not
/// be silent either — every failure becomes a sentence naming the file, which
/// the composer says out loud. That is the same contract `attachAdd` has.
async fn carry_forward(
    db: &crate::db::Db,
    clients: Arc<dyn crate::commands::GoogleClients>,
    cache: Option<&AttachmentCache>,
    draft_id: &str,
    message_id: i64,
    now: i64,
) -> Result<Value, ComposeError> {
    let plan = engine::forward::plan(db, message_id)?;
    let mut refused = plan.refused.clone();
    let mut added: Vec<engine::attach::Attachment> = Vec::new();

    if plan.is_empty() {
        return Ok(json!({
            "attachments": engine::attach::list(db, draft_id)?,
            "added": added,
            "refused": refused,
        }));
    }

    let gmail = match clients.gmail(plan.account_id) {
        Ok(client) => Some(client),
        // Not a reason to stop: anything already cached still rides, and the
        // files that needed the network are named below rather than here, so
        // the owner reads which ones he has lost and not only why.
        Err(error) => {
            refused.push(error.to_string());
            None
        }
    };
    // One `messages.get` for the whole forward, however many parts need it.
    let mut fetched: Option<crate::google::types::Message> = None;

    // The pictures first. They have no row and so no known size, and a body
    // with a hole where a picture was is worse than a file that says it did not
    // fit. `added_at` is nudged per file so the chips keep the order the
    // message had rather than the order two ids happened to sort in.
    let mut clock = now;
    for content_id in &plan.inline_cids {
        clock += 1;
        let key = store::cache_key(
            plan.account_id,
            &plan.gmail_message_id,
            store::PartKind::Inline,
            content_id,
        );
        let known = cache.and_then(|cache| cache.find(&key)).and_then(|hit| {
            std::fs::read(&hit.path).ok()
        });

        let (bytes, name, declared) = match known {
            Some(bytes) => (bytes, String::new(), None),
            None => {
                let Some(gmail) = gmail.as_ref() else { continue };
                let message = match full_message(gmail, &plan.gmail_message_id, &mut fetched).await
                {
                    Ok(message) => message,
                    Err(error) => {
                        refused.push(error);
                        break;
                    }
                };
                let body = message.extract_body();
                let Some(part) = body
                    .attachments
                    .iter()
                    .find(|part| matches_content_id(part, content_id))
                else {
                    // A `cid:` the message does not answer to. Nothing to
                    // carry and nothing to say: the reference is dropped from
                    // the forwarded body at build time, which is what a
                    // recipient of the *original* already sees.
                    continue;
                };
                let name = part.filename.clone();
                let declared = Some(part.mime_type.clone());
                match part_bytes(gmail, &plan.gmail_message_id, part).await {
                    Ok(bytes) => (bytes, name, declared),
                    Err(error) => {
                        refused.push(error);
                        continue;
                    }
                }
            }
        };

        // Sniffed, not taken on the sender's word — the same rule the reading
        // pane applies before it draws one. A part that is not an image cannot
        // be drawn where it sits by anybody, so it rides as a file and the
        // reference to it is dropped rather than left broken.
        let sniffed = store::names::sniff_raster_image(&bytes);
        let mime = sniffed.map(str::to_string).or(declared);
        match engine::attach::add_incoming(
            db,
            draft_id,
            engine::attach::Incoming {
                filename: &name,
                mime_type: mime.as_deref(),
                inline: sniffed.is_some(),
                content_id: Some(content_id),
            },
            &bytes,
            clock,
        ) {
            Ok(attachment) => {
                if let Some(cache) = cache {
                    let _ = cache.store(&key, &attachment.filename, &bytes);
                }
                added.push(attachment);
            }
            Err(error) => refused.push(error.to_string()),
        }
    }

    for file in &plan.files {
        clock += 1;
        let key = store::cache_key(
            plan.account_id,
            &plan.gmail_message_id,
            store::PartKind::File,
            &store::file_part_id(
                file.gmail_attachment_id.as_deref(),
                file.size_bytes,
                &file.filename,
            ),
        );
        let cached = cache
            .and_then(|cache| cache.find(&key))
            .and_then(|hit| std::fs::read(&hit.path).ok());

        let bytes = match cached {
            Some(bytes) => bytes,
            None => {
                let Some(gmail) = gmail.as_ref() else {
                    // No way to reach Gmail, and these bytes are not here. The
                    // reason is already in `refused`; this is the file it cost.
                    refused.push(format!("{} could not be fetched", file.filename));
                    continue;
                };
                match file_bytes(gmail, &plan, file, &mut fetched).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        refused.push(error);
                        continue;
                    }
                }
            }
        };

        match engine::attach::add_incoming(
            db,
            draft_id,
            engine::attach::Incoming {
                filename: &file.filename,
                mime_type: Some(&file.mime_type),
                inline: false,
                content_id: None,
            },
            &bytes,
            clock,
        ) {
            Ok(attachment) => {
                if let Some(cache) = cache {
                    let _ = cache.store(&key, &attachment.filename, &bytes);
                }
                added.push(attachment);
            }
            Err(error) => refused.push(error.to_string()),
        }
    }

    Ok(json!({
        "attachments": engine::attach::list(db, draft_id)?,
        "added": added,
        "refused": refused,
    }))
}

/// The whole message, fetched at most once per forward.
async fn full_message<'a>(
    gmail: &crate::google::gmail::GmailClient,
    gmail_message_id: &str,
    held: &'a mut Option<crate::google::types::Message>,
) -> Result<&'a crate::google::types::Message, String> {
    if held.is_none() {
        let message = gmail
            .messages_get(
                USER_ID,
                gmail_message_id,
                crate::google::gmail::MessageFormat::Full,
            )
            .await
            .map_err(|error| error.to_string())?;
        *held = Some(message);
    }
    Ok(held.as_ref().expect("just filled"))
}

/// One file's bytes: by attachment id when Gmail gave one, and out of the
/// message when it did not — the same two roads [`super::attachments`] takes,
/// for the same reason.
async fn file_bytes(
    gmail: &crate::google::gmail::GmailClient,
    plan: &engine::forward::Plan,
    file: &engine::forward::PlannedFile,
    held: &mut Option<crate::google::types::Message>,
) -> Result<Vec<u8>, String> {
    if let Some(id) = file.gmail_attachment_id.as_deref() {
        return gmail
            .attachment_get_capped(
                USER_ID,
                &plan.gmail_message_id,
                id,
                engine::attach::MAX_ATTACHMENT_BYTES as usize,
            )
            .await
            .map_err(|error| format!("{}: {error}", file.filename));
    }
    let message = full_message(gmail, &plan.gmail_message_id, held).await?;
    let body = message.extract_body();
    let part = body
        .attachments
        .iter()
        .find(|part| part.filename == file.filename && part.size == file.size_bytes)
        .or_else(|| {
            body.attachments
                .iter()
                .find(|part| part.filename == file.filename)
        })
        .ok_or_else(|| {
            format!(
                "{} is no longer part of that message on Gmail",
                file.filename
            )
        })?;
    part_bytes(gmail, &plan.gmail_message_id, part).await
}

/// The bytes behind one MIME part, whichever way Gmail chose to deliver them.
async fn part_bytes(
    gmail: &crate::google::gmail::GmailClient,
    gmail_message_id: &str,
    part: &crate::google::types::AttachmentMeta,
) -> Result<Vec<u8>, String> {
    if let Some(data) = &part.data {
        return Ok(data.clone());
    }
    let id = part.attachment_id.as_deref().ok_or_else(|| {
        format!(
            "{} carries no bytes and no way to fetch them",
            if part.filename.is_empty() {
                "that part of the message"
            } else {
                &part.filename
            }
        )
    })?;
    gmail
        .attachment_get_capped(
            USER_ID,
            gmail_message_id,
            id,
            engine::attach::MAX_ATTACHMENT_BYTES as usize,
        )
        .await
        .map_err(|error| error.to_string())
}

/// Does this part answer to `content_id`? Exact first, because a Content-ID is
/// a case-sensitive addr-spec; case-insensitively second, because plenty of
/// mailers do not know that.
fn matches_content_id(part: &crate::google::types::AttachmentMeta, content_id: &str) -> bool {
    match part.content_id.as_deref() {
        Some(id) => id == content_id || id.eq_ignore_ascii_case(content_id),
        None => false,
    }
}

/// The two local halves of forgetting a draft: the mirror in the conversation,
/// then the row and the files it was carrying.
///
/// In that order. Taking the row out first would leave [`mirror::unmirror`]
/// nothing to look the message up by, and the phantom draft in the middle of
/// the thread is exactly the thing being removed.
///
/// Deleting the row also writes a tombstone (`draft::retire`), which is what
/// stops an autosave that was already on the wire from writing the draft back
/// half a second later.
fn forget_draft_locally(
    db: &crate::db::Db,
    draft_id: &str,
    now: i64,
) -> Result<(), ComposeError> {
    if let Some(draft) = draft::load_draft(db, draft_id)? {
        engine::mirror::unmirror(db, &draft)?;
    }
    draft::delete_draft(db, draft_id, now)
}

// ---------------------------------------------------------------------------
// payload helpers
// ---------------------------------------------------------------------------

fn parse_draft(payload: &Value) -> Result<Draft, ComposeError> {
    // Accept both `{ draft: {…} }` and a bare draft, so the original
    // `send_message(draft)` signature still means what it says.
    let raw = payload.get("draft").unwrap_or(payload);
    serde_json::from_value(raw.clone())
        .map_err(|e| ComposeError::invalid(format!("that is not a draft: {e}")))
}

fn required_i64(payload: &Value, key: &str) -> Result<i64, ComposeError> {
    payload
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| ComposeError::invalid(format!("{key} is required")))
}

fn required_str(payload: &Value, key: &str) -> Result<String, ComposeError> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ComposeError::invalid(format!("{key} is required")))
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The router's own draft id, exposed so `agent::tools` mints ids in the same
/// grammar rather than inventing a second one.
pub fn new_draft_id(now: i64) -> String {
    format!("draft-{now:x}-{:x}", entropy(now))
}

/// Enough variation to keep two drafts started in the same millisecond apart.
/// Not a security boundary — the ids are local row keys.
fn entropy(now: i64) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    (now as u64).rotate_left(17) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

impl From<ComposeError> for IpcError {
    fn from(error: ComposeError) -> Self {
        match error {
            ComposeError::Db(inner) => IpcError::Db(inner),
            ComposeError::Command(inner) => IpcError::Command(inner),
            ComposeError::Mime(message) => IpcError::internal(format!(
                "the message could not be assembled: {message}"
            )),
            other => IpcError::Command(crate::commands::CommandError::Invalid {
                message: other.to_string(),
            }),
        }
    }
}
