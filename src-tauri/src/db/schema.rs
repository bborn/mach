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
];

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
