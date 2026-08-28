//! Dropping `body_html` for old mail, and getting the disk back.
//!
//! # What this is for
//!
//! The owner's store is 2.3 GB for 46 000 threads and 66 000 messages, and
//! `messages` keeps `body_html` in full for every message ever synced. HTML mail
//! is mostly markup — a newsletter is 40 to 300 KB of it against 2 KB of text —
//! so `body_html` is the bulk of the file. The other three quarters of a
//! gigabyte is the WAL, which is a separate problem and not this module's.
//!
//! `body_html` is also the one column in this store that is a *cache*. Gmail
//! holds it, addressed by `gmail_message_id`, and `messages.get` returns it. So
//! for a message old enough not to be opened again, keeping it locally buys a
//! few hundred milliseconds on a read that will not happen, at a cost measured
//! in gigabytes.
//!
//! # What it is worth
//!
//! `tests/evict_scale.rs` generates a store of that shape and runs the whole
//! thing. On 46 000 threads and 66 000 messages, 1.95 GB of file holding 1.87 GB
//! of HTML:
//!
//! ```text
//!   sweep    10–22 s   58 972 bodies, 1 774 MB of HTML dropped
//!   file       unchanged at 1 954 MB, with 1 589 MB now on the free list
//!   vacuum    1–22 s   1 954 MB → 143 MB
//! ```
//!
//! Ranges, because both numbers are disk-bound and moved by a factor of ten
//! across three runs on the same machine depending on what else was using it.
//! The shape is what matters and it did not move: 92 % of the file goes, and
//! neither step is a minute.
//!
//! Opening a message, measured on the same store: 1 564 µs for one that still
//! has its HTML, 33 µs for an evicted one — the text is *faster* to put on
//! screen, because the cost of a resident open is sanitizing 20 to 80 KB of
//! markup. What the reader waits for is not the body; it is the upgrade, which
//! is one `messages.get`.
//!
//! # The rule
//!
//! Age, and nothing else. A message whose `internal_date` is older than
//! [`EvictionPolicy::older_than_ms`] — 90 days by default — has its `body_html`
//! set to NULL and `html_evicted_at` stamped.
//!
//! Age *plus not-recently-opened* was the alternative and it was rejected for
//! what it costs to know: "recently opened" is a write on the read path, taking
//! the single writer mutex on every message the reader expands, which is exactly
//! the contention this store is already short of. Age is free — `internal_date`
//! is already indexed — and it reclaims essentially the same space, because a
//! mailbox of this shape has almost all of its bytes in mail that is years old
//! and is not opened at all.
//!
//! There is one exception, and it is about the cache rather than the rule: a
//! message whose HTML was re-fetched within [`EvictionPolicy::keep_restored_for_ms`]
//! stays resident. That is the read signal, taken where it is free — on the
//! re-fetch, which is already a write — rather than on every open. Without it a
//! message opened this morning is evicted again this afternoon and re-fetched
//! tomorrow.
//!
//! # What is never evicted
//!
//! [`plan`] is the whole guard, and it is the only guard. The SQL in
//! [`candidates`] narrows the scan; it does not decide. Every candidate goes
//! through this function and only a [`Plan::Evict`] or [`Plan::Derive`] is
//! written.
//!
//!  * **Anything Gmail cannot give back.** A local draft, an outbox message, any
//!    row whose `gmail_message_id` is one of Mach's own placeholders, and the
//!    empty id — all of it via [`is_local_message_id`], which is the codebase's
//!    single answer to "is this id ours or Google's". Evicting one of these is
//!    permanent loss of something the owner wrote.
//!  * **Drafts.** `is_draft` covers the Gmail-side draft that has a real message
//!    id and would therefore pass the id test. A draft is unsent text the owner
//!    is in the middle of, `compose::mirror` writes its HTML locally, and the
//!    composer reads that column back. Nothing here goes near it.
//!  * **Trash and spam.** The message still exists at Gmail *today*; in thirty
//!    days it is purged and the request would 404 forever. Evicting there trades
//!    a recoverable body for an unrecoverable one at a date nobody is watching.
//!  * **Anything with no plain text, and none derivable.** The read path renders
//!    `body_text` while the request is in flight. A message with HTML and no
//!    text part would fall through to its snippet — one line — and would be a
//!    visibly worse message for as long as the fetch took, or forever if it
//!    failed. See [the section below](#html-only-mail) for what happens first.
//!  * **Anything already gone.** `body_html IS NULL` is not a candidate; there
//!    is nothing to free and a stamp would make a plain-text message look
//!    refetchable.
//!  * **Small bodies.** Under [`EvictionPolicy::min_bytes`] the page is not
//!    freed anyway — a short body shares a page with its neighbours — and the
//!    open would still cost a round trip. 2 KB is the floor.
//!  * **Bodies that are mostly text already.** A body whose derived text is
//!    within [`EvictionPolicy::min_bytes`] of the HTML it came from frees
//!    nothing and costs a round trip. Same floor, applied to the difference.
//!
//! # HTML-only mail
//!
//! The first version of this module refused every message with no `body_text`
//! and left it there, on the grounds that deriving text would write `body_text`
//! and `body_text` is what `messages_fts` indexes. On the owner's store that
//! rule refused 12 481 of 12 494 candidates and the first sweep freed nothing.
//!
//! The reasoning was backwards on this data. Plenty of senders ship HTML with no
//! `text/plain` alternative, and Mach stores what arrived; those messages have
//! *no* indexed body at all and are findable by subject alone. Writing a derived
//! `body_text` cannot cost search anything, because there was nothing there to
//! lose — it is the only way those bodies become searchable.
//!
//! So the sweep derives the text — [`crate::render::text::from_html`], which
//! sanitizes first, so no stylesheet or script ever becomes a term — and writes
//! it in **the same UPDATE that drops the HTML**:
//!
//! ```sql
//! UPDATE messages SET body_text = ?, body_html = NULL, … WHERE id = ?
//! ```
//!
//! One statement, so the ordering is not a convention that a later edit can
//! reverse. There is no instant, and no crash, in which a row has lost its HTML
//! and not yet gained its text: either the row is written or it is not. A
//! derivation that produces nothing legible means the statement is never issued
//! and the HTML stays where it is.
//!
//! `subject` is still never written, and neither is a `body_text` that already
//! holds something — the derived write carries `AND (body_text IS NULL OR
//! trim(body_text) = '')`, so a sender's own text can never be replaced by a
//! machine's reading of their markup, not even by a sweep racing a sync.
//!
//! Derived text is marked as derived, in `body_text_derived_at` (migration 13).
//! Nothing reads it to change what the reader sees; it exists so the rows this
//! module invented text for can be found again if the extractor improves.
//!
//! # The row's `search_text` goes with the HTML
//!
//! Migration 23 added a column holding the readable text of `body_html`, for
//! `messages_fts` to index and for nothing else, so that a message is findable
//! by what its markup says as well as by what its sender wrote. The sweep does
//! not fill that column — sync fills it when the message arrives and
//! [`crate::db::backfill::derive_search_text`] filled it once for the mail that
//! predates sync doing so — but it does have to keep one invariant.
//!
//! The derived write above clears `search_text` in the same statement. Both
//! columns would otherwise hold the same text, read out of the same markup by
//! the same extractor, and `messages_fts` would carry every one of its terms
//! twice: once against `body_text` and once against `search_text`. The message
//! is no less findable for the clearing, because the text did not go anywhere.
//! It moved into the column a reader can also see.
//!
//! # Getting the space back
//!
//! Setting a column to NULL does not shrink the file. SQLite moves the freed
//! pages onto the database's free list and reuses them for the next insert, so
//! after a sweep the store is the same size on disk with a very large hole in
//! it. Only [`reclaim`] — `VACUUM` — hands pages back to the filesystem, and it
//! rewrites the entire database to do it. See that function for what it costs
//! and when it can run.

pub mod command;
pub mod refetch;

use std::time::Duration;

use rusqlite::{params, Connection};

use crate::db::models::is_local_message_id;
use crate::db::{Db, Result};

pub use refetch::{restore_html, RestoreError, Restored};

// ---------------------------------------------------------------------------
// policy
// ---------------------------------------------------------------------------

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// When a message's HTML stops being worth its disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvictionPolicy {
    /// Older than this, by `internal_date`, and the HTML goes.
    pub older_than_ms: i64,
    /// A re-fetched body stays resident this long before it is eligible again.
    pub keep_restored_for_ms: i64,
    /// Bodies smaller than this are left alone.
    pub min_bytes: i64,
    /// Rows per write transaction. Bounds how long the sweep holds the writer.
    pub batch: usize,
    /// Batches per sweep, so one pass has a bounded cost. `None` is "until
    /// there is nothing left", which is what the first sweep on an old store
    /// wants.
    pub max_batches: Option<usize>,
}

impl Default for EvictionPolicy {
    /// Ninety days, which is the shape of the mailbox rather than a round
    /// number: a thread that has been silent for a quarter is not one anybody
    /// is still reading, and three months of mail is the window this app's
    /// backfill treats as current. On a store of the owner's shape it leaves
    /// roughly a twentieth of the messages resident and takes the HTML off the
    /// rest.
    fn default() -> Self {
        EvictionPolicy {
            older_than_ms: 90 * DAY_MS,
            keep_restored_for_ms: 14 * DAY_MS,
            min_bytes: 2048,
            // 500 rows is a few megabytes of UPDATE under one writer lock,
            // which is well under the 5 s `busy_timeout` a reader would wait.
            batch: 500,
            max_batches: None,
        }
    }
}

/// Why a candidate is being left alone. `None` from [`retention_reason`] is the
/// only thing that gets written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// The id is Mach's own — a draft, an outbox entry, or no id at all. Gmail
    /// has never heard of this message and cannot give the body back.
    Unrecoverable,
    /// A draft. Unsent text the owner is in the middle of.
    Draft,
    /// Trashed or spam: Gmail purges it, and then the re-fetch is a 404.
    Deleted,
    /// No plain-text body to render while the re-fetch is in flight, and none
    /// could be read out of the HTML either — a body that is one image with no
    /// `alt`, or markup with no prose in it at all.
    NoText,
    /// The text derived from this body is nearly as large as the body. Dropping
    /// the HTML would free nothing and cost a round trip.
    NoGain,
    /// There is no HTML here to drop.
    NotResident,
    /// Too small for the round trip to be worth the pages.
    TooSmall,
    /// Newer than the policy's window.
    TooRecent,
    /// Re-fetched recently enough to still be worth keeping.
    RecentlyRestored,
}

impl Keep {
    /// A stable tag, for reports and tests.
    pub fn as_str(self) -> &'static str {
        match self {
            Keep::Unrecoverable => "unrecoverable",
            Keep::Draft => "draft",
            Keep::Deleted => "deleted",
            Keep::NoText => "noText",
            Keep::NoGain => "noGain",
            Keep::NotResident => "notResident",
            Keep::TooSmall => "tooSmall",
            Keep::TooRecent => "tooRecent",
            Keep::RecentlyRestored => "recentlyRestored",
        }
    }
}

/// Everything the decision needs, and nothing else.
///
/// A struct rather than the whole [`crate::db::models::Message`] because the
/// decision must be testable without a store, and because loading 66 000 bodies
/// to decide whether to drop them would defeat the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageFacts {
    pub id: i64,
    pub account_id: i64,
    pub gmail_message_id: String,
    pub internal_date: i64,
    pub is_draft: bool,
    /// Bytes of `body_html`, or `None` when the column is NULL.
    pub html_bytes: Option<i64>,
    /// Whether `body_text` holds something renderable.
    pub has_text: bool,
    /// Thread carries `TRASH` or `SPAM`.
    pub deleted: bool,
    pub html_restored_at: Option<i64>,
}

/// What the sweep will do with one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Drop the HTML. The message already has text to render in its place.
    Evict,
    /// Read the text out of the HTML and drop the HTML, in one statement. The
    /// message has no `body_text`, which on this store is most of them.
    Derive,
    /// Leave it alone, for this reason.
    Keep(Keep),
}

impl Plan {
    /// The reason this row is being left alone, if it is.
    pub fn kept(self) -> Option<Keep> {
        match self {
            Plan::Keep(reason) => Some(reason),
            _ => None,
        }
    }
}

/// The guard.
///
/// The order is deliberate: everything unrecoverable is refused before anything
/// merely uneconomic is, so a test that asks "why was this kept" gets the answer
/// that matters rather than "it was small".
///
/// Missing text is no longer a refusal — it is [`Plan::Derive`], which is a
/// refusal until the derivation succeeds. It sits where the old check sat, after
/// everything unrecoverable and before every economic test, so nothing that
/// could not be fetched back is ever offered for derivation.
pub fn plan(facts: &MessageFacts, now_ms: i64, policy: &EvictionPolicy) -> Plan {
    // Not Google's id: a draft Mach minted, an outbox entry, or a row with no
    // id at all. There is nothing at the other end to ask.
    if is_local_message_id(&facts.gmail_message_id) {
        return Plan::Keep(Keep::Unrecoverable);
    }
    if facts.is_draft {
        return Plan::Keep(Keep::Draft);
    }
    if facts.deleted {
        return Plan::Keep(Keep::Deleted);
    }

    let Some(bytes) = facts.html_bytes else {
        return Plan::Keep(Keep::NotResident);
    };
    if bytes < policy.min_bytes {
        return Plan::Keep(Keep::TooSmall);
    }
    if facts.internal_date >= now_ms.saturating_sub(policy.older_than_ms) {
        return Plan::Keep(Keep::TooRecent);
    }
    if let Some(restored) = facts.html_restored_at {
        if restored >= now_ms.saturating_sub(policy.keep_restored_for_ms) {
            return Plan::Keep(Keep::RecentlyRestored);
        }
    }

    if facts.has_text {
        Plan::Evict
    } else {
        Plan::Derive
    }
}

/// The guard, for a caller that only wants to know whether the row is being left
/// alone. A row that needs its text derived first is not being kept — see
/// [`plan`].
pub fn retention_reason(facts: &MessageFacts, now_ms: i64, policy: &EvictionPolicy) -> Option<Keep> {
    plan(facts, now_ms, policy).kept()
}

// Both of these used to live here, because the sweep was the only thing that
// ever derived text from markup. It is not any more — sync derives at ingest and
// [`crate::db::backfill`] derives for the rows that predate it — so the
// derivation and its ceiling sit beside the extractor in
// [`crate::render::text`], where the judgement about text quality that decides
// when to use them also is. Re-exported rather than moved out of reach: this is
// still the module whose name the ceiling is about.
pub use crate::render::text::{derive_text, MAX_DERIVED_BYTES};

// ---------------------------------------------------------------------------
// the sweep
// ---------------------------------------------------------------------------

/// What one sweep did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictionReport {
    /// Rows the SQL offered.
    pub examined: u64,
    /// Rows whose HTML was dropped.
    pub evicted: u64,
    /// Of those, rows whose `body_text` this sweep wrote: the ones that had
    /// none, and the ones whose own text was not worth indexing.
    pub derived: u64,
    /// Bytes of derived text written back, which is the cost of those rows.
    pub bytes_written: u64,
    /// Bytes of `body_html` no longer stored, **less** `bytes_written`. Net,
    /// because a sweep that drops 80 KB of markup and stores 3 KB of text has
    /// freed 77 KB and saying 80 would be wrong. Not bytes returned to the
    /// filesystem either — see [`reclaim`] for that.
    pub bytes_freed: u64,
    /// Rows the guard refused, and why. Kept because "the sweep found 40 000
    /// candidates and evicted none" is a sentence somebody has to be able to
    /// explain.
    pub kept: Vec<(Keep, u64)>,
}

impl EvictionReport {
    fn keep(&mut self, reason: Keep) {
        match self.kept.iter_mut().find(|(r, _)| *r == reason) {
            Some((_, n)) => *n += 1,
            None => self.kept.push((reason, 1)),
        }
    }

    pub fn kept_count(&self, reason: Keep) -> u64 {
        self.kept
            .iter()
            .find(|(r, _)| *r == reason)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }

    fn absorb(&mut self, other: EvictionReport) {
        self.examined += other.examined;
        self.evicted += other.evicted;
        self.derived += other.derived;
        self.bytes_written += other.bytes_written;
        self.bytes_freed += other.bytes_freed;
        for (reason, n) in other.kept {
            match self.kept.iter_mut().find(|(r, _)| *r == reason) {
                Some((_, count)) => *count += n,
                None => self.kept.push((reason, n)),
            }
        }
    }
}

/// The rows the sweep will consider, cheapest-first.
///
/// Every clause here is also in [`plan`], which is the point: this
/// is a pre-filter that keeps the sweep from reading 66 000 rows to reject
/// 63 000 of them, and the guard is what actually decides. The one clause that
/// cannot be expressed twice is the local-id test — `gmail_message_id = ''` is
/// local and `NOT LIKE 'mach-%'` does not catch it — so the SQL states both
/// halves and the guard states the rule.
///
/// `after_id` is a keyset cursor rather than an offset. The sweep is mutating
/// the very predicate it is paginating over, so an offset would skip rows on
/// every batch after the first.
fn candidates(
    conn: &Connection,
    now_ms: i64,
    policy: &EvictionPolicy,
    after_id: i64,
) -> Result<Vec<MessageFacts>> {
    let cutoff = now_ms.saturating_sub(policy.older_than_ms);
    let mut stmt = conn.prepare(
        "SELECT m.id,
                m.account_id,
                m.gmail_message_id,
                m.internal_date,
                m.is_draft,
                length(m.body_html),
                (m.body_text IS NOT NULL AND trim(m.body_text) <> ''),
                EXISTS (SELECT 1 FROM thread_labels tl
                         WHERE tl.thread_id = m.thread_id
                           AND tl.gmail_label_id IN ('TRASH', 'SPAM')),
                m.html_restored_at
           FROM messages m
          WHERE m.body_html IS NOT NULL
            AND m.is_draft = 0
            AND m.internal_date < ?1
            AND m.id > ?2
            AND length(m.body_html) >= ?3
            AND m.gmail_message_id <> ''
            AND m.gmail_message_id NOT LIKE 'mach-%'
          ORDER BY m.id
          LIMIT ?4",
    )?;
    let rows = stmt
        .query_map(params![cutoff, after_id, policy.min_bytes, policy.batch as i64], |row| {
            Ok(MessageFacts {
                id: row.get(0)?,
                account_id: row.get(1)?,
                gmail_message_id: row.get(2)?,
                internal_date: row.get(3)?,
                is_draft: row.get(4)?,
                html_bytes: row.get(5)?,
                has_text: row.get(6)?,
                deleted: row.get(7)?,
                html_restored_at: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One row the sweep has decided to evict, and the text it will carry with it.
struct Doomed {
    id: i64,
    /// Bytes of `body_html` about to go.
    html_bytes: i64,
    /// The text derived from that HTML, for a row that had none. `None` means
    /// the row already has a body the reader can fall back to.
    derived: Option<String>,
}

/// The HTML of one row, read back so its text can be derived from it.
///
/// A separate single-row read rather than a column on [`candidates`], because
/// the batch is 500 rows and a batch of 500 bodies is 40 MB held to decide
/// something that only needs one at a time.
fn resident_html(db: &Db, id: i64) -> Result<Option<String>> {
    db.read(|conn| {
        use rusqlite::OptionalExtension;
        Ok(conn
            .query_row("SELECT body_html FROM messages WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten())
    })
}

/// The text that will stand in for this row's HTML, or why it will not.
///
/// The two economic refusals that can only be made once the derivation has been
/// tried. Both leave the HTML exactly where it is, which is what makes trying
/// safe: nothing has been written by the time either is returned.
fn derived_body(
    db: &Db,
    facts: &MessageFacts,
    policy: &EvictionPolicy,
) -> Result<std::result::Result<String, Keep>> {
    let derived = resident_html(db, facts.id)?.as_deref().and_then(derive_text);
    // Markup with no prose in it — one image and no `alt`, a body that is all
    // layout. Nothing to fall back to and nothing to index.
    let Some(text) = derived else {
        return Ok(Err(Keep::NoText));
    };
    // Storing the text would cost most of what dropping the HTML saves, and the
    // next open would still be a round trip.
    if facts.html_bytes.unwrap_or(0) - (text.len() as i64) < policy.min_bytes {
        return Ok(Err(Keep::NoGain));
    }
    Ok(Ok(text))
}

/// Drop the HTML of everything the policy and the guard agree on.
///
/// One write transaction per [`EvictionPolicy::batch`], because the alternative
/// — one transaction for 60 000 rows — holds the single writer for the length of
/// the whole sweep and puts every sync write behind it. Between batches the lock
/// is released, so the sync loop interleaves normally.
///
/// # Where the derivation happens
///
/// Before the transaction opens, not inside it. Sanitizing a body costs on the
/// order of a millisecond and a batch of 500 would be most of a second of it;
/// spending that under the single writer lock is exactly the contention this
/// module exists to avoid. So the text is derived from a pooled read, which
/// blocks nobody, and the write is only the UPDATEs.
///
/// That leaves a gap in which sync could have replaced `body_html` — and it is
/// closed by the statement rather than by the ordering. `body_html IS NOT NULL`
/// means a row re-fetched in between is not stamped as evicted while holding a
/// body, and `body_text IS NULL OR trim(body_text) = ''` means a row that gained
/// a real text part in between keeps it. A message body at Gmail does not change
/// under a message id, so the derivation cannot be reading different bytes from
/// the ones being dropped; these two clauses are what makes that not need to be
/// true.
///
/// # The one statement
///
/// The derived write sets `body_text` and NULLs `body_html` in a single UPDATE.
/// Not two statements in a transaction, which would be equally durable and would
/// leave the ordering as a line of code somebody could move. There is no
/// sequence of interruptions that loses both, because there is no intermediate
/// row state to be interrupted in.
///
/// The `UPDATE` fires `messages_fts_au`, which deletes this row's old terms and
/// inserts its new ones. For a row that is only losing HTML the terms are
/// unchanged and the churn buys nothing, which is why [`reclaim`] runs
/// `optimize` before the `VACUUM`. For a row that is gaining derived text, that
/// trigger is the entire point: `subject` was all `messages_fts` had of it, and
/// after the sweep it has the body.
pub fn sweep(db: &Db, now_ms: i64, policy: &EvictionPolicy) -> Result<EvictionReport> {
    let mut total = EvictionReport::default();
    let mut cursor = 0i64;
    let mut batches = 0usize;

    loop {
        if let Some(max) = policy.max_batches {
            if batches >= max {
                break;
            }
        }

        let batch = db.read(|conn| candidates(conn, now_ms, policy, cursor))?;
        if batch.is_empty() {
            break;
        }
        batches += 1;
        cursor = batch.last().map(|f| f.id).unwrap_or(cursor);

        let mut report = EvictionReport::default();
        let mut doomed: Vec<Doomed> = Vec::with_capacity(batch.len());
        for facts in &batch {
            report.examined += 1;
            let html_bytes = facts.html_bytes.unwrap_or(0);
            match plan(facts, now_ms, policy) {
                Plan::Keep(reason) => report.keep(reason),
                Plan::Evict => doomed.push(Doomed {
                    id: facts.id,
                    html_bytes,
                    derived: None,
                }),
                Plan::Derive => match derived_body(db, facts, policy)? {
                    Err(reason) => report.keep(reason),
                    Ok(text) => doomed.push(Doomed {
                        id: facts.id,
                        html_bytes,
                        derived: Some(text),
                    }),
                },
            }
        }

        if !doomed.is_empty() {
            db.write(|conn| {
                let mut evict = conn.prepare(
                    "UPDATE messages
                        SET body_html        = NULL,
                            html_evicted_at  = ?2,
                            html_restored_at = NULL
                      WHERE id = ?1 AND body_html IS NOT NULL",
                )?;
                // Text and eviction in one statement. See the function doc.
                //
                // `search_text = NULL` because the text being written into
                // `body_text` is the same text, read out of the same markup by
                // the same extractor. Leaving both would put every one of its
                // terms into `messages_fts` twice.
                let mut derive = conn.prepare(
                    "UPDATE messages
                        SET body_text            = ?3,
                            body_text_derived_at = ?2,
                            search_text          = NULL,
                            body_html            = NULL,
                            html_evicted_at      = ?2,
                            html_restored_at     = NULL
                      WHERE id = ?1
                        AND body_html IS NOT NULL
                        AND (body_text IS NULL OR trim(body_text) = '')",
                )?;
                for row in &doomed {
                    let wrote = match &row.derived {
                        None => evict.execute(params![row.id, now_ms])?,
                        Some(text) => derive.execute(params![row.id, now_ms, text])?,
                    };
                    if wrote == 0 {
                        continue;
                    }
                    report.evicted += 1;
                    let written = row.derived.as_ref().map(|t| t.len() as u64).unwrap_or(0);
                    if written > 0 {
                        report.derived += 1;
                        report.bytes_written += written;
                    }
                    report.bytes_freed += (row.html_bytes as u64).saturating_sub(written);
                }
                Ok(())
            })?;
        }

        let short = batch.len() < policy.batch;
        total.absorb(report);
        if short {
            break;
        }
    }

    Ok(total)
}

// ---------------------------------------------------------------------------
// reclaiming the pages
// ---------------------------------------------------------------------------

/// What a sweep has left lying around, in pages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FreeSpace {
    pub page_size: i64,
    pub page_count: i64,
    pub freelist_count: i64,
}

impl FreeSpace {
    /// Bytes the file is holding that hold nothing.
    pub fn reclaimable_bytes(&self) -> i64 {
        self.page_size * self.freelist_count
    }

    pub fn file_bytes(&self) -> i64 {
        self.page_size * self.page_count
    }
}

/// How much of the file is free list right now.
pub fn free_space(db: &Db) -> Result<FreeSpace> {
    db.read(|conn| {
        Ok(FreeSpace {
            page_size: conn.query_row("PRAGMA page_size", [], |r| r.get(0))?,
            page_count: conn.query_row("PRAGMA page_count", [], |r| r.get(0))?,
            freelist_count: conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?,
        })
    })
}

/// What a `VACUUM` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReclaimReport {
    pub before: FreeSpace,
    pub after: FreeSpace,
    pub elapsed: Duration,
}

impl ReclaimReport {
    pub fn bytes_returned(&self) -> i64 {
        self.before.file_bytes() - self.after.file_bytes()
    }
}

/// Return the freed pages to the filesystem.
///
/// # Why `VACUUM` and not something cheaper
///
/// There is no cheaper option on this store. `PRAGMA incremental_vacuum` gives
/// pages back a few at a time with no exclusive lock, and it is only available
/// on a database created with `auto_vacuum = INCREMENTAL` — a header field fixed
/// before the first table exists. The owner's store was not, and the only way to
/// change it *is* a full `VACUUM`. So the first one is unavoidable whichever
/// path is taken afterwards.
///
/// # What it costs
///
/// `VACUUM` copies every live page into a new file and moves it into place. It
/// takes an exclusive lock for the duration, so nothing reads and nothing writes
/// while it runs, and it needs free disk space of roughly the size of the
/// *finished* database on top of the original.
///
/// The cost is proportional to what survives, not to the file being replaced,
/// which is what makes it affordable here: after a sweep almost nothing
/// survives. `tests/evict_scale.rs` builds a store of the owner's shape — 46 000
/// threads, 66 000 messages, 1.95 GB — and takes it to 143 MB in **1 to 22
/// seconds**, three runs on one machine. Seconds is a short enough stall to ask
/// for; it is not short enough to take without asking.
///
/// It also writes the whole new file through the WAL, which comes out at about
/// the size of the vacuumed database (144 MB in that run) and is returned by the
/// next checkpoint.
///
/// # So it does not run while he is using the app
///
/// [`run`] never calls this. Nothing calls it on a timer. It is a deliberate
/// act, and the caller is expected to say so: a sweep is invisible and can run
/// whenever, a vacuum stops the app and must be asked for. `optimize` on the FTS
/// index first, because the sweep's UPDATEs left it carrying a delete and an
/// insert per evicted row, and vacuuming those into the new file would be
/// copying garbage.
pub fn reclaim(db: &Db) -> Result<ReclaimReport> {
    let before = free_space(db)?;
    let started = std::time::Instant::now();
    {
        let conn = db.writer();
        conn.execute_batch("INSERT INTO messages_fts (messages_fts) VALUES ('optimize');")?;
        conn.execute_batch("VACUUM;")?;
    }
    let elapsed = started.elapsed();
    let after = free_space(db)?;
    Ok(ReclaimReport {
        before,
        after,
        elapsed,
    })
}

// ---------------------------------------------------------------------------
// the loop
// ---------------------------------------------------------------------------

/// How often the sweep runs while the app is open.
///
/// Six hours, because there is nothing to be punctual about. A message crossing
/// the ninety-day line is not an event anybody is waiting on, and the whole
/// point is that the work is invisible.
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// How long after launch the first sweep runs.
///
/// Not immediately. Launch is when the sync loop is doing its heaviest writing
/// and the window is still painting itself; a sweep that takes the writer in the
/// middle of that is competing with the one thing the owner can see.
pub const FIRST_SWEEP_DELAY: Duration = Duration::from_secs(5 * 60);

/// Sweep on a tick until cancelled. Never vacuums.
pub async fn run<F>(
    db: Db,
    cancel: crate::sync::CancelToken,
    policy: EvictionPolicy,
    first_delay: Duration,
    interval: Duration,
    mut observe: F,
) where
    F: FnMut(EvictionReport) + Send + 'static,
{
    tokio::select! {
        () = cancel.cancelled() => return,
        () = tokio::time::sleep(first_delay) => {}
    }

    loop {
        if cancel.is_cancelled() {
            break;
        }
        match sweep(&db, now_ms(), &policy) {
            Ok(report) => {
                if report.evicted > 0 {
                    observe(report);
                }
            }
            // A sweep that could not run is not a reason to stop sweeping.
            // Nothing was written and the rows are still there next time.
            Err(error) => eprintln!("could not evict old message bodies: {error}"),
        }

        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(interval) => {}
        }
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
