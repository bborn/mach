//! Schema and migrations.
//!
//! Migrations are a `&[Migration]` slice keyed by version, tracked in the
//! `user_version` pragma. `user_version` was chosen over a `schema_migrations`
//! table because it is a single integer read with no query, it cannot itself
//! need a migration, and this is a single-writer local store where a linear
//! version counter is the whole truth. Adding migration #2 later is appending
//! one entry to `MIGRATIONS`; `migrate()` applies only what is missing, each in
//! its own transaction, so a partially applied upgrade cannot exist.

use rusqlite::Connection;

use crate::db::Result;

pub struct Migration {
    pub version: u32,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: M1_INITIAL,
    },
    Migration {
        version: 2,
        sql: M2_SNOOZED_THREADS,
    },
    Migration {
        version: 3,
        sql: M3_REPLY_TO,
    },
    Migration {
        version: 4,
        sql: M4_PREFERENCES,
    },
    Migration {
        version: 5,
        sql: M5_EVENT_ROUND_TRIP,
    },
    Migration {
        version: 6,
        sql: M6_CALENDARS,
    },
    Migration {
        version: 7,
        sql: M7_EVENT_DETAIL,
    },
    Migration {
        version: 8,
        sql: M8_DRAFT_LOOKUP,
    },
    Migration {
        version: 9,
        sql: M9_MESSAGE_DRAFT_ID,
    },
    Migration {
        version: 10,
        sql: M10_DROP_DENSITY,
    },
    Migration {
        version: 11,
        sql: M11_TEXT_FLOWED,
    },
    Migration {
        version: 12,
        sql: M12_HTML_EVICTION,
    },
    Migration {
        version: 13,
        sql: M13_DERIVED_TEXT,
    },
    Migration {
        version: 14,
        sql: M14_SENDER_INDEX,
    },
    Migration {
        version: 15,
        sql: M15_MIRROR_OWNER,
    },
];

/// Migration 15 — which composer draft a message row is the mirror of.
///
/// # The identity the mirror never had
///
/// A draft's mirror row was addressed by `gmail_message_id`, and that is not an
/// identity: `drafts.update` mints a **new message id on every save**, whoever
/// calls it. Two writers moved the row between ids — `mirror` writing under the
/// new one, `mirror::adopt` renaming the old one — and `adopt` did its rename
/// with `UPDATE OR IGNORE`, so when both ran the collision was swallowed and the
/// draft ended up as two rows in the conversation. The owner watched one reply
/// render as two `DRAFT` rows with byte-identical text, and the composer, which
/// knew about exactly one of them, answered ⌘⇧⌫ with "There is no draft here to
/// throw away".
///
/// This column is the identity. It names the `compose_drafts` row the mirror
/// stands for, it is written only by `compose::mirror`, and nothing renames it —
/// so "the mirror of this draft" is a lookup rather than a guess at which of a
/// draft's several message ids is current. The unique index that makes one
/// draft's second mirror row impossible is created by
/// [`ensure_compose_schema`](crate::compose::ensure_compose_schema), because it
/// can only be built after the duplicates already on disk have been collapsed
/// and that needs `compose_drafts`, which this module cannot assume exists.
///
/// It is not `gmail_draft_id` — that column says what *Gmail* reports about a
/// message, and the same rule that keeps sync from writing what the editor owns
/// keeps the editor from writing that.
///
/// # The two repairs
///
/// Both are for rows already on disk, both name only tables this module owns,
/// and both are idempotent.
///
/// A conversation is in the Drafts mailbox because a `DRAFT` row in
/// `thread_labels` says so. `unmirror` drops that row only when it can see the
/// mirror it belongs to, so a mirror removed by any other path left the label
/// behind — and the Drafts list offered conversations whose reading pane had
/// nothing in it. The owner's store holds four.
///
/// The second is the conversation those drafts were the only message of.
/// `unmirror` deletes a thread it has emptied only when the thread is synthetic,
/// so a draft written into a real Gmail conversation that Mach had no other
/// message of leaves the husk: a row in `threads` with no messages, which is a
/// blank reading pane wherever it is still listed. Requiring the thread to carry
/// no labels either is what keeps this off a conversation that is merely waiting
/// for the backfill to reach it.
const M15_MIRROR_OWNER: &str = r#"
ALTER TABLE messages ADD COLUMN mach_draft_id TEXT;

DELETE FROM thread_labels
 WHERE gmail_label_id = 'DRAFT'
   AND NOT EXISTS (
        SELECT 1 FROM messages m
         WHERE m.thread_id = thread_labels.thread_id AND m.is_draft = 1);

DELETE FROM threads
 WHERE NOT EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = threads.id)
   AND NOT EXISTS (SELECT 1 FROM thread_labels l WHERE l.thread_id = threads.id);
"#;

/// Migration 14 — who sent it, as an index.
///
/// [`crate::db::queries::address_book`] reads one column out of every row in
/// `messages` and then the recipients of the few thousand the owner sent
/// himself. Without an index both halves are a full scan of a table whose rows
/// carry the bodies, so answering "who do I know" meant reading gigabytes to
/// look at a string: 1.3 seconds a half against the owner's 1.1 GB store, warm,
/// and several times that cold.
///
/// Covering, so neither half touches the table at all — `internal_date` is in
/// the key for `max(internal_date)`, which is the whole of what the sender half
/// wants beside the address. Measured on that store: 1.28s to 0.02s for the
/// senders, 1.37s to 0.01s for the people he writes to, at a cost of 2.7 MB.
///
/// Nothing waited on the old version, so this is not a fix for a stall anybody
/// saw. It is a fix for a boot that spent two seconds of disk against the sync
/// it was racing.
const M14_SENDER_INDEX: &str = r#"
CREATE INDEX IF NOT EXISTS idx_messages_sender
    ON messages (from_email, internal_date);
"#;

/// Migration 13 — when Mach wrote this row's `body_text` itself.
///
/// A great deal of mail arrives as HTML with no `text/plain` alternative. On the
/// owner's store that is 12 481 of the 12 494 messages the eviction sweep would
/// otherwise have been able to touch, and it is the reason the first sweep freed
/// nothing: [`crate::evict`] refuses to drop HTML from a message that has no
/// text to render in its place. The same rows are also unfindable by their
/// bodies, because `messages_fts` indexes `body_text` and there is none.
///
/// So the sweep derives one from the HTML before dropping it. This column is
/// where that fact is recorded — non-NULL means the text in `body_text` was
/// produced here, by [`crate::render::text`], rather than sent by whoever wrote
/// the message.
///
/// It is worth a column because the alternative is that the two are
/// indistinguishable, and they are not the same thing:
///
///  * A derivation is a decision that can be revisited. Improving the extractor
///    is only useful if the rows it already wrote can be found and redone, and
///    without this the query for "text Mach invented" does not exist.
///  * `html_evicted_at` does not answer it. A message can have derived text and
///    a resident body — sync's upsert writes `body_html` back without touching
///    either date — and a message can be evicted with the sender's own text.
///
/// Nothing reads it to change what the reader sees. Derived text is rendered
/// exactly like any other plain-text body, because it is the same claim: this is
/// what the message says.
const M13_DERIVED_TEXT: &str = r#"
ALTER TABLE messages ADD COLUMN body_text_derived_at INTEGER;
"#;

/// Migration 12 — two dates that say what happened to a message's HTML.
///
/// `body_html` is the bulk of this store. A 46 000-thread mailbox is 2.3 GB, and
/// most of that is markup for mail nobody will open again. It is also the one
/// column here that is a *cache*: Gmail still has it, addressed by
/// `gmail_message_id`, so dropping it locally costs a request rather than the
/// message. `body_text` is not that — it is small, it is what `messages_fts`
/// indexes, and it is what an evicted message renders from while the request is
/// in flight. The sweep writes it only into a row that has none, and only in the
/// statement that drops the HTML; see migration 13 and [`crate::evict`].
///
/// Both columns are about the *cache*, not the message, which is why neither is
/// on `NewMessage` and neither is written by sync's upsert:
///
///  * `html_evicted_at` — when this row's HTML was dropped. Non-NULL is the only
///    thing that distinguishes "we had HTML and let it go, ask Gmail for it"
///    from "this message never had an HTML part", which is an ordinary state for
///    plain-text mail. Without it every plain-text message would cost a pointless
///    request on every open, forever.
///  * `html_restored_at` — when a re-fetch put it back. It is the read the sweep
///    can see without writing on the read path: a message opened once stays
///    resident for a while rather than being evicted again the same afternoon.
///
/// Neither column implies anything about `body_html` on its own. Sync's upsert
/// rewrites `body_html` from `excluded.body_html` and knows nothing about either
/// column, so a message re-fetched by a history replay comes back resident with
/// `html_evicted_at` still set. Everything that asks "is this evicted" therefore
/// asks `html_evicted_at IS NOT NULL AND body_html IS NULL`, which stays true
/// through any order of those writes.
///
/// The index is partial on exactly the sweep's predicate, and it is partial in
/// the direction that matters: rows *leave* it as they are evicted. On this
/// mailbox it starts at one entry per message with HTML and settles at one entry
/// per recent message — a few thousand — so the sweep is a range seek over the
/// resident set rather than a scan of a 2 GB table, and the maintenance sync
/// pays for it is one b-tree insert per message stored.
const M12_HTML_EVICTION: &str = r#"
ALTER TABLE messages ADD COLUMN html_evicted_at  INTEGER;
ALTER TABLE messages ADD COLUMN html_restored_at INTEGER;

CREATE INDEX IF NOT EXISTS idx_messages_html_resident
    ON messages (internal_date)
    WHERE body_html IS NOT NULL AND is_draft = 0;
"#;

/// Migration 11 — whether a plain-text body's line breaks are the sender's.
///
/// `Content-Type: text/plain; format=flowed` (RFC 3676) is the sender saying
/// that the breaks ending in a space were put there by their generator and may
/// be undone. Without it a client cannot tell a wrapped paragraph from a
/// numbered list, and guessing wrong scrambles the message — so this is stored
/// rather than inferred.
///
/// Both columns are nullable and nothing backfills them. A row written before
/// this migration reads as "we were never told", which every reader treats as
/// *not* flowed — the answer that leaves the body exactly as it arrived. Mail
/// synced from here on carries the real declaration; older mail keeps its
/// breaks until it is re-fetched, which is the safe direction to be wrong in.
const M11_TEXT_FLOWED: &str = r#"
ALTER TABLE messages ADD COLUMN body_text_flowed INTEGER;
ALTER TABLE messages ADD COLUMN body_text_delsp  INTEGER;
"#;

/// Migration 10 — delete the retired `density` preference.
///
/// `density` chose between a compact and a comfortable thread row. The app has
/// one row now, and nothing reads the key.
///
/// Migration 4's note says a removed preference "leaves a row nobody reads
/// instead of a column nobody can drop". That covers a downgrade, where a build
/// that still wants the key has to find it. `density` will not come back, and
/// `get_preferences` returns every row, so leaving it means handing the
/// frontend a setting with no meaning on every launch for the lifetime of the
/// store. One `DELETE` ends it.
///
/// A store that never held the key is unaffected — `DELETE` of no rows is not
/// an error.
const M10_DROP_DENSITY: &str = "DELETE FROM preferences WHERE key = 'density';";

/// Migration 9 — which Gmail draft a message *is*.
///
/// A draft written in Gmail on the phone or on the web reaches Mach through
/// ordinary message sync, carrying the `DRAFT` label, and that is where it
/// stopped: the message was here, marked correctly, and could not be edited,
/// because `drafts.update` is addressed by a **draft id** and the draft id is
/// not on the message resource. It comes from `users.drafts.list` and from
/// nowhere else.
///
/// So this is where it is kept: one nullable column on the message it belongs
/// to, written only by the drafts sweep in `sync::mail`, read when a draft row
/// is opened. It is deliberately *not* a table. The mapping is one string per
/// draft, its lifetime is exactly the message's lifetime, and it is only ever
/// read by message — three facts that describe a column.
///
/// It is also not a second copy of `compose_drafts.gmail_draft_id`, which
/// answers a different question. This column says what **Gmail** reports about a
/// message; that one says which draft the composer's editable row has bound
/// itself to. They meet once, when a draft is first opened here and the id is
/// copied across, and after that the composer's row is the only thing that
/// pushes. Keeping them apart is what stops adoption from becoming the fourth
/// duplicate-draft bug: sync can never rewrite what the editor owns, and the
/// editor can never invent a draft id.
///
/// The index is partial because drafts are a handful of rows in a table of tens
/// of thousands, and the one query that scans by this column — forgetting the
/// ids of drafts Gmail no longer has — should cost the size of the draft set
/// rather than the size of the mailbox.
const M9_MESSAGE_DRAFT_ID: &str = r#"
ALTER TABLE messages ADD COLUMN gmail_draft_id TEXT;
CREATE INDEX IF NOT EXISTS idx_messages_gmail_draft
    ON messages (account_id, gmail_draft_id) WHERE gmail_draft_id IS NOT NULL;
"#;

/// Migration 8 — find a conversation by the draft in it.
///
/// The Drafts mailbox used to be "threads carrying Gmail's `DRAFT` label", read
/// out of `thread_labels`, and that turned out to be a set the app cannot keep.
/// `sync_queries::recompute_thread` rebuilds every derived field on `threads`
/// from the per-message label union on each pass — which is exactly what makes
/// a replayed history batch converge — so a `DRAFT` row written locally for a
/// draft Google has not told us about yet is dropped by the next sync. The
/// draft was in the mailbox, and then quietly was not.
///
/// So the Drafts mailbox reads `messages.is_draft` as well (see
/// `queries::list_threads`), which is the same fact from the side that is
/// durable: it is set locally when Mach mirrors a draft, and set by
/// `sync::convert` from Gmail's own `DRAFT` label on the way in. This is the
/// index that makes that second test a seek rather than a scan.
///
/// Partial, because drafts are a handful of rows in a table with tens of
/// thousands: the index holds only the threads that actually have one.
const M8_DRAFT_LOOKUP: &str = r#"
CREATE INDEX IF NOT EXISTS idx_messages_draft ON messages (thread_id) WHERE is_draft = 1;
"#;

/// Migration 7 — what makes an event a meeting rather than a block of colour.
///
/// The comparison that produced this was Google's own popover beside Mach's
/// modal, on one recurring standup. Google had a Meet link with a join button, a
/// dial-in number with its PIN, "3 guests · 1 yes, 1 no, 1 maybe" with each
/// answer under it and a declining guest's reason spelled out, and a creator who
/// was not the organizer. Mach had a comma-separated list of addresses. Every
/// one of those facts arrived in the same `events.list` response Mach was
/// already making — parsed, held for the length of one function, and dropped for
/// want of somewhere to put it.
///
///  * `conference` — the join link, the meeting code, and every entry point:
///    video, phone with its PIN and region, SIP, and the "more phone numbers"
///    page. Flattened out of `conferenceData` rather than stored verbatim,
///    because half of that block exists to round-trip a conference this app
///    never creates. `hangoutLink` — deprecated since Hangouts and still
///    populated on every Meet event — is folded in here as the video entry point
///    when `conferenceData` is missing, which is why it gets no column of its
///    own: two spellings of one URL is not two facts.
///  * `guests` — the answer sheet. `attendees` stays exactly as it was, holding
///    the addresses the editor round-trips, and this holds what Google says
///    about each of them: `responseStatus`, `optional`, `organizer`, `self`,
///    `resource`, and the comment a guest attached to their reply. Two columns
///    rather than a richer `attendees` because the two are written by different
///    things — a local edit sets the addresses and cannot know the answers, and
///    a sync knows the answers and must not fight the edit. When a local edit
///    changes the guest list this column is set back to `NULL`, i.e. "we no
///    longer know", and the next sync fills it in.
///  * `creator` — who made the event. Different from the organizer more often
///    than one would think: an assistant booking on a director's calendar, a
///    room system, an integration. Google shows both; showing only the organizer
///    attributes the meeting to the wrong person.
///  * `attachments` — the Drive files hanging off the event. Title and URL only.
///    The icon link is a remote image and the file id is a handle for an API
///    Mach does not speak, so neither is kept.
///  * `visibility`, `transparency` — private, and free-versus-busy. Two words
///    each, and both of them change what the event *means* rather than how it
///    looks.
///
/// Every column is nullable and there is no backfill. A row that predates this
/// migration reads back as "we were never told", which each reader treats as
/// silence rather than as a negative answer — the same rule migration 5 set for
/// `organizer_self`, and for the same reason: the first launch after an upgrade
/// must not look like data loss.
///
/// No index. Nothing here is ever a predicate; these columns are read by primary
/// key, one event at a time, by a modal that is already open.
const M7_EVENT_DETAIL: &str = r#"
ALTER TABLE events ADD COLUMN conference   TEXT;
ALTER TABLE events ADD COLUMN guests       TEXT;
ALTER TABLE events ADD COLUMN creator      TEXT;
ALTER TABLE events ADD COLUMN attachments  TEXT;
ALTER TABLE events ADD COLUMN visibility   TEXT;
ALTER TABLE events ADD COLUMN transparency TEXT;
"#;

/// Migration 6 — `calendars`, because a calendar has never had a name.
///
/// `list_calendars` answered from `SELECT DISTINCT calendar_id FROM events`, and
/// a calendar id is not a name: `en.usa#holiday@group.v.calendar.google.com`,
/// `c_8f3…@group.calendar.google.com`, and — for the calendar Google labels with
/// the account holder's own name — the account's email address. The sidebar then
/// invented something readable on top of that (`Shared · d814cb`), which is what
/// you do when there is nothing to show. There was nothing to show because
/// `calendarList.list` had a client and no caller: none of the real metadata had
/// ever been fetched.
///
/// This is the table that was missing. One row per `(account, calendar)`,
/// holding what Google actually says:
///
///  * `summary` and `summary_override` — the name, in two columns, because they
///    are two different facts. `summary` is what the calendar's *owner* called
///    it; `summary_override` is what *this* account renamed its own subscription
///    to. The override wins, and it is the entire reason a calendar can read
///    "Dad/Ben Schedule" here and something else in its owner's account.
///  * `background_color`, `foreground_color`, `color_id` — Google's spelling
///    rather than the British one on `accounts.colour_index`, because these are
///    the API's words and not Mach's, and a column that is a verbatim copy of a
///    wire field should be searchable by that field's name.
///  * `access_role` — `owner`, `writer`, `reader` or `freeBusyReader`. Nullable,
///    and read permissively: every row that predates a metadata fetch says
///    nothing about access, and "we were not told" must never collapse into
///    "read-only" or the whole calendar goes flat on first launch.
///  * `selected` — Google's own "is this calendar shown". It defaults to 1 so
///    that a calendar Mach learns about some other way is visible rather than
///    silently absent.
///  * `deleted` — a tombstone, not a `DELETE`. A calendar unsubscribed or
///    removed between two syncs still has events sitting in `events`, and
///    dropping its row would leave them nameless, which is precisely the state
///    this migration exists to end. The row stays, marked; sync stops asking
///    about it; the sidebar keeps naming it for as long as its events are still
///    on screen.
///  * `synced_at` — when this row was last refreshed. The calendar list is a
///    dozen rows that change a few times a year, so this is what lets the
///    calendar pass skip the request on almost every tick instead of asking
///    Google every minute for an answer that has not moved.
///
/// `events` is deliberately *not* foreign-keyed to this table. Event rows are
/// written by a sweep that knows a calendar id and nothing else, and a
/// constraint would mean the metadata fetch had to succeed before a single event
/// could land — coupling the cheap, frequent write to the rare, skippable one.
/// The two are joined by `(account_id, calendar_id)` at read time instead, and
/// `list_calendars` still falls back to the derived list for a calendar that has
/// events but no row yet, so nothing disappears mid-migration.
const M6_CALENDARS: &str = r#"
CREATE TABLE calendars (
    id               INTEGER PRIMARY KEY,
    account_id       INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    calendar_id      TEXT    NOT NULL,
    summary          TEXT,
    summary_override TEXT,
    description      TEXT,
    time_zone        TEXT,
    color_id         TEXT,
    background_color TEXT,
    foreground_color TEXT,
    access_role      TEXT,
    is_primary       INTEGER NOT NULL DEFAULT 0,
    selected         INTEGER NOT NULL DEFAULT 1,
    deleted          INTEGER NOT NULL DEFAULT 0,
    synced_at        INTEGER NOT NULL DEFAULT 0,
    UNIQUE (account_id, calendar_id)
);
"#;

/// Migration 5 — the six event fields that were sent but never kept.
///
/// Every column here closes the same kind of hole: something Mach knew at write
/// time, or that Google told us at read time, which had nowhere to live and was
/// therefore forgotten before the next render.
///
///  * `recurrence` — the RRULE lines. They were write-only, so an event created
///    as weekly came back as an ordinary meeting, and the modal could neither
///    show its rule nor leave that rule alone while editing something else.
///  * `reminders` — the same story for alerts, kept in Google's own shape
///    (`useDefault` plus overrides) rather than as a bare minute count, because
///    "the calendar's default" and "no reminder at all" are different states
///    and one nullable integer cannot tell them apart.
///  * `ical_uid` — the identity that survives a meeting being copied onto two
///    accounts. Without it the same invitation on two of the owner's calendars
///    is two unrelated blocks; `calendar-merge.ts` has been waiting for it.
///  * `organizer` / `organizer_self` — who owns the event, and whether that is
///    us. Google refuses a write from a non-organizer, so this is what lets the
///    UI stop offering an edit that could only ever fail.
///  * `guests_can_modify` — the one exception to the above, and the reason
///    ownership on its own is not the whole permission answer.
///
/// Every column is nullable, so existing rows migrate with no backfill and read
/// back as "we do not know" — which each consumer treats as permissive rather
/// than as a denial. The next sync pass fills them in.
const M5_EVENT_ROUND_TRIP: &str = r#"
ALTER TABLE events ADD COLUMN recurrence        TEXT;
ALTER TABLE events ADD COLUMN reminders         TEXT;
ALTER TABLE events ADD COLUMN ical_uid          TEXT;
ALTER TABLE events ADD COLUMN organizer         TEXT;
ALTER TABLE events ADD COLUMN organizer_self    INTEGER;
ALTER TABLE events ADD COLUMN guests_can_modify INTEGER;

-- Cross-account identity is a lookup by uid at a given instant, never by uid
-- alone: two copies of one meeting share a uid *and* a start, while every
-- occurrence of a series shares its master's uid and differs only in start.
CREATE INDEX idx_events_ical_uid ON events (ical_uid, start_ts) WHERE ical_uid IS NOT NULL;
"#;

/// Migration 4 — `preferences`.
///
/// One row per setting, the value a JSON document.
///
/// A column per preference is the obvious alternative and it is the wrong one.
/// Every new setting would be a migration, which means the cost of *adding* a
/// preference is a schema change and a release — and the whole reason this app
/// had none for so long is that the cost of the first one was never paid. Here
/// it is an insert.
///
/// It also makes the two directions of drift harmless. A database written by a
/// newer build can hold keys this build has never heard of; the reader ignores
/// them rather than failing. A preference that is removed leaves a row nobody
/// reads instead of a column nobody can drop.
///
/// The value is JSON in a `TEXT` column because the settings are not one type:
/// a signature is a string, a week start is a number, working hours are a pair.
/// One typed column could only hold all three by carrying a discriminator
/// beside it, which is JSON with extra steps.
///
/// `WITHOUT ROWID` because the whole table is its primary key: a handful of
/// short keys read together at boot, never scanned, never joined.
const M4_PREFERENCES: &str = r#"
CREATE TABLE preferences (
    key        TEXT    NOT NULL PRIMARY KEY,
    value      TEXT    NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
"#;

/// Migration 3 — `messages.reply_to`.
///
/// A sender who sets `Reply-To` is asking for the answer to go somewhere else,
/// and mailing lists depend on it. Without the column the composer cannot
/// honour it, so replies to a list would go to the individual who happened to
/// post rather than to the list.
const M3_REPLY_TO: &str = "ALTER TABLE messages ADD COLUMN reply_to TEXT;";

/// Migration 2 — `snoozed_threads`, promoted from the command layer.
///
/// The command layer created this table itself, through
/// [`ensure_command_schema`](crate::db::command_queries::ensure_command_schema),
/// because it could not edit this file while both units were being built. This
/// is that promotion, and it points at the same constant rather than copying the
/// DDL so the two cannot drift.
///
/// `ensure_command_schema` still runs and is still correct: the statements are
/// `IF NOT EXISTS`, so on a database that has taken this migration it finds the
/// table already there and does nothing.
const M2_SNOOZED_THREADS: &str = crate::db::command_queries::COMMAND_SCHEMA;

/// The version a freshly migrated database reports.
pub const LATEST_VERSION: u32 = match MIGRATIONS.last() {
    Some(m) => m.version,
    None => 0,
};

/// Apply every migration newer than the database's `user_version`.
///
/// Idempotent: running it on an up-to-date database does nothing and touches no
/// pages. Returns the version the database is now at.
pub fn migrate(conn: &mut Connection) -> Result<u32> {
    let mut current: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?
        .max(0) as u32;

    for m in MIGRATIONS {
        if m.version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        // `PRAGMA user_version` takes no bind parameters, hence pragma_update.
        tx.pragma_update(None, "user_version", m.version)?;
        tx.commit()?;
        current = m.version;
    }

    Ok(current)
}

/// Migration 1 — the whole store as designed in the spec.
///
/// Conventions:
///  * every timestamp is unix **milliseconds**, `INTEGER`
///  * booleans are `INTEGER` 0/1
///  * lists of people are JSON `TEXT` (display data, never queried across)
///  * every core table carries `account_id` and cascades from `accounts`
const M1_INITIAL: &str = r#"
-- accounts -------------------------------------------------------------------
CREATE TABLE accounts (
    id                  INTEGER PRIMARY KEY,
    email               TEXT    NOT NULL UNIQUE,
    display_name        TEXT,
    -- Keychain item name. Tokens themselves are never stored in SQLite.
    token_ref           TEXT    NOT NULL DEFAULT '',
    -- Gmail users.history watermark (opaque; TEXT because it is a uint64).
    history_id          TEXT,
    -- Calendar events.list incremental syncToken.
    calendar_sync_token TEXT,
    colour_index        INTEGER NOT NULL DEFAULT 0,
    created_at          INTEGER NOT NULL DEFAULT 0
);

-- labels ---------------------------------------------------------------------
CREATE TABLE labels (
    id             INTEGER PRIMARY KEY,
    account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    gmail_label_id TEXT    NOT NULL,
    name           TEXT    NOT NULL DEFAULT '',
    label_type     TEXT    NOT NULL DEFAULT 'user',
    UNIQUE (account_id, gmail_label_id)
);

-- threads --------------------------------------------------------------------
CREATE TABLE threads (
    id               INTEGER PRIMARY KEY,
    account_id       INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    gmail_thread_id  TEXT    NOT NULL,
    -- JSON array of {name, email}
    participants     TEXT    NOT NULL DEFAULT '[]',
    subject          TEXT    NOT NULL DEFAULT '',
    snippet          TEXT    NOT NULL DEFAULT '',
    last_message_at  INTEGER NOT NULL DEFAULT 0,
    is_unread        INTEGER NOT NULL DEFAULT 0,
    message_count    INTEGER NOT NULL DEFAULT 0,
    has_attachments  INTEGER NOT NULL DEFAULT 0,
    UNIQUE (account_id, gmail_thread_id)
);

-- Label refs live in a join table rather than a JSON column on `threads`:
-- the account/label rail filters by label on every keystroke, and only an
-- indexed row per (label, thread) makes that an index seek. Keyed by the Gmail
-- label id string, not labels.id, so the sync loop can write thread labels
-- before (or without) having synced the label list itself.
CREATE TABLE thread_labels (
    thread_id      INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    gmail_label_id TEXT    NOT NULL,
    PRIMARY KEY (thread_id, gmail_label_id)
) WITHOUT ROWID;

-- messages -------------------------------------------------------------------
CREATE TABLE messages (
    id                INTEGER PRIMARY KEY,
    thread_id         INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    account_id        INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    gmail_message_id  TEXT    NOT NULL,
    rfc822_message_id TEXT,
    in_reply_to       TEXT,
    -- `references` is a SQL keyword; the header keeps its name in the model.
    references_header TEXT,
    from_name         TEXT,
    from_email        TEXT    NOT NULL DEFAULT '',
    to_json           TEXT    NOT NULL DEFAULT '[]',
    cc_json           TEXT    NOT NULL DEFAULT '[]',
    bcc_json          TEXT    NOT NULL DEFAULT '[]',
    subject           TEXT    NOT NULL DEFAULT '',
    body_html         TEXT,
    body_text         TEXT,
    snippet           TEXT    NOT NULL DEFAULT '',
    internal_date     INTEGER NOT NULL DEFAULT 0,
    is_unread         INTEGER NOT NULL DEFAULT 0,
    is_draft          INTEGER NOT NULL DEFAULT 0,
    UNIQUE (account_id, gmail_message_id)
);

-- attachments ----------------------------------------------------------------
CREATE TABLE attachments (
    id                  INTEGER PRIMARY KEY,
    message_id          INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    gmail_attachment_id TEXT,
    filename            TEXT    NOT NULL DEFAULT '',
    mime_type           TEXT    NOT NULL DEFAULT 'application/octet-stream',
    size_bytes          INTEGER NOT NULL DEFAULT 0,
    -- NULL until the bytes are pulled down; set to an on-disk cache path.
    local_path          TEXT
);

-- events ---------------------------------------------------------------------
-- Populated with singleEvents=true, so rows are concrete instances, never
-- RRULEs. recurring_event_id links an instance back to its series.
CREATE TABLE events (
    id                 INTEGER PRIMARY KEY,
    account_id         INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    calendar_id        TEXT    NOT NULL,
    google_event_id    TEXT    NOT NULL,
    title              TEXT    NOT NULL DEFAULT '',
    description        TEXT,
    location           TEXT,
    start_ts           INTEGER NOT NULL DEFAULT 0,
    end_ts             INTEGER NOT NULL DEFAULT 0,
    is_all_day         INTEGER NOT NULL DEFAULT 0,
    attendees          TEXT    NOT NULL DEFAULT '[]',
    rsvp_status        TEXT,
    recurring_event_id TEXT,
    status             TEXT    NOT NULL DEFAULT 'confirmed',
    html_link          TEXT,
    updated_at         INTEGER NOT NULL DEFAULT 0,
    UNIQUE (account_id, calendar_id, google_event_id)
);

-- indices --------------------------------------------------------------------
-- The hot query: unified inbox, all accounts, newest first, keyset-paginated on
-- (last_message_at, id). This covering-ish index makes it an index scan with no
-- sort at any depth.
CREATE INDEX idx_threads_stream          ON threads (last_message_at DESC, id DESC);
-- Same list scoped to one account (the rail).
CREATE INDEX idx_threads_account_stream  ON threads (account_id, last_message_at DESC, id DESC);
-- Unread badges per account, cheap because it is partial.
CREATE INDEX idx_threads_unread          ON threads (account_id) WHERE is_unread = 1;
-- Label rail: seek straight to the threads carrying a label.
CREATE INDEX idx_thread_labels_label     ON thread_labels (gmail_label_id, thread_id);
-- Reading pane: a thread's messages in conversation order.
CREATE INDEX idx_messages_thread         ON messages (thread_id, internal_date, id);
-- Per-account history sync walks its own messages by time.
CREATE INDEX idx_messages_account_date   ON messages (account_id, internal_date DESC);
-- Also the conflict target for attachment upserts. Covers plain lookups by
-- message_id as a leftmost prefix, so no second index is needed.
CREATE UNIQUE INDEX idx_attachments_message ON attachments (message_id, gmail_attachment_id);
-- Calendar window queries, unified and per-account.
CREATE INDEX idx_events_window           ON events (start_ts, end_ts);
CREATE INDEX idx_events_account_window   ON events (account_id, start_ts, end_ts);
-- Thread lookup by Gmail id is served by the UNIQUE(account_id, gmail_thread_id)
-- index; message lookup by UNIQUE(account_id, gmail_message_id).

-- full text search -----------------------------------------------------------
-- External-content FTS5 over `messages`: the index stores only the inverted
-- terms, the text itself is never duplicated. That halves the on-disk cost of
-- a 12-month backfill.
CREATE VIRTUAL TABLE messages_fts USING fts5(
    subject,
    body_text,
    content = 'messages',
    content_rowid = 'id',
    tokenize = "unicode61 remove_diacritics 2"
);

-- Triggers, not dual writes: there is no code path that can write a body
-- without reindexing it. This holds for rows removed by ON DELETE CASCADE too
-- (SQLite fires delete triggers for those; pinned by a test that queries
-- messages_fts directly rather than through a join, since a join would mask a
-- stale entry instead of exposing it).
CREATE TRIGGER messages_fts_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts (rowid, subject, body_text)
    VALUES (new.id, new.subject, new.body_text);
END;

CREATE TRIGGER messages_fts_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, subject, body_text)
    VALUES ('delete', old.id, old.subject, old.body_text);
END;

CREATE TRIGGER messages_fts_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, subject, body_text)
    VALUES ('delete', old.id, old.subject, old.body_text);
    INSERT INTO messages_fts (rowid, subject, body_text)
    VALUES (new.id, new.subject, new.body_text);
END;
"#;
