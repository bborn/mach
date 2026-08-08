//! The local SQLite store — the single source of truth the UI renders from.
//!
//! # Concurrency
//!
//! `rusqlite::Connection` is `Send` but **not** `Sync`: one connection cannot be
//! used from two threads at once. Meanwhile this app has exactly the shape that
//! makes that awkward — a background sync loop writing continuously while the UI
//! thread reads on every keystroke.
//!
//! The choice here is **one mutex-guarded writer plus a small pool of read-only
//! connections**, not a single global `Mutex<Connection>`.
//!
//! A single shared mutex would serialise UI reads behind sync writes, which
//! breaks the one invariant this whole codebase exists for. WAL mode gives us
//! the alternative for free: readers never block the writer and the writer never
//! blocks readers, because readers work from the last committed snapshot while
//! the writer appends to the log. So:
//!
//!  * **Writer** — a single `Mutex<Connection>`. SQLite permits exactly one
//!    writer per database anyway, so the mutex costs nothing we were not already
//!    going to pay, and it makes "one writer" a type-level fact rather than a
//!    convention. The sync loop holds it; commands take it briefly.
//!  * **Readers** — a lazily grown, bounded pool of connections opened with
//!    `PRAGMA query_only = ON`. Checkout is a `Vec::pop` under a very short
//!    mutex, never held for the duration of a query. Read-only is enforced by
//!    the engine, not by discipline, so a stray write in a UI command fails loudly
//!    instead of stalling the sync loop.
//!
//! A crate-level pool (r2d2/deadpool) would do the same thing with more
//! dependency; the pool here is ~40 lines because the requirements are tiny:
//! bounded, lazy, no health checks, no async.
//!
//! `Db` is `Clone` (it is an `Arc` inside) and `Send + Sync`, so it drops
//! straight into `tauri::State`.

pub mod command_queries;
pub mod models;
pub mod queries;
pub mod schema;
pub mod sync_queries;

use std::ops::Deref;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OpenFlags};

/// How many read connections we keep parked. Five accounts, one list view, one
/// reading pane, one search box — four idle readers is more than the UI can use
/// at once, and each costs only a file handle plus a page cache.
const MAX_IDLE_READERS: usize = 4;

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("malformed json in column: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

/// So a failed query can be returned straight out of a Tauri command.
impl serde::Serialize for DbError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, DbError>;

// ---------------------------------------------------------------------------
// Db
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    /// A connection string every connection in this handle can open. For
    /// on-disk databases this is the path; for in-memory it is a shared-cache
    /// URI so the pool sees the same database.
    uri: String,
    writer: Mutex<Connection>,
    idle_readers: Mutex<Vec<Connection>>,
}

impl Db {
    /// Open (creating if needed) the database at `path`, apply pragmas, and
    /// bring the schema up to date.
    pub fn open(path: impl AsRef<Path>) -> Result<Db> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| DbError::Other(format!("creating {}: {e}", parent.display())))?;
            }
        }
        Db::from_uri(path.to_string_lossy().into_owned())
    }

    /// An in-memory database, for tests and throwaway work.
    ///
    /// Uses a uniquely named shared-cache URI so the reader pool addresses the
    /// same database as the writer. WAL does not apply to memory databases, so
    /// concurrent access here is serialised by SQLite's shared cache — fine for
    /// tests, not intended for the running app.
    pub fn open_in_memory() -> Result<Db> {
        static N: AtomicU64 = AtomicU64::new(0);
        let uri = format!(
            "file:mach-mem-{}-{}?mode=memory&cache=shared",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        Db::from_uri(uri)
    }

    fn from_uri(uri: String) -> Result<Db> {
        let mut writer = open_connection(&uri, false)?;
        schema::migrate(&mut writer)?;
        Ok(Db {
            inner: Arc::new(Inner {
                uri,
                writer: Mutex::new(writer),
                idle_readers: Mutex::new(Vec::new()),
            }),
        })
    }

    /// The single write connection. Held for the duration of the guard, so keep
    /// the critical section to one logical unit of work.
    ///
    /// A poisoned mutex is recovered rather than propagated: a panic in one
    /// command must not take the whole store offline, and SQLite's own
    /// transaction rollback has already restored consistency.
    pub fn writer(&self) -> MutexGuard<'_, Connection> {
        self.inner
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A read-only connection from the pool. Never blocks on the writer.
    pub fn reader(&self) -> Reader {
        let pooled = {
            let mut idle = self
                .inner
                .idle_readers
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            idle.pop()
        };
        let conn = match pooled {
            Some(conn) => conn,
            // If opening a reader fails we cannot usefully proceed: the file was
            // openable moments ago when the writer was created.
            None => open_connection(&self.inner.uri, true).expect("open reader connection"),
        };
        Reader {
            conn: Some(conn),
            inner: Arc::clone(&self.inner),
        }
    }

    /// Convenience: run a closure against a pooled reader.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let reader = self.reader();
        f(&reader)
    }

    /// Convenience: run a closure inside a write transaction, committing on
    /// `Ok` and rolling back on `Err`.
    pub fn write<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

/// A pooled read-only connection. Returns itself to the pool on drop.
pub struct Reader {
    conn: Option<Connection>,
    inner: Arc<Inner>,
}

impl Deref for Reader {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("reader used after drop")
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let mut idle = self.inner.idle_readers.lock().unwrap_or_else(|p| p.into_inner());
            if idle.len() < MAX_IDLE_READERS {
                idle.push(conn);
            }
            // Otherwise the connection is simply closed.
        }
    }
}

// ---------------------------------------------------------------------------
// pragmas
// ---------------------------------------------------------------------------

fn open_connection(uri: &str, read_only: bool) -> Result<Connection> {
    // NO_MUTEX = SQLite's own serialisation is off; each Connection is used from
    // one thread at a time, which the Rust types already guarantee.
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(uri, flags)?;
    apply_pragmas(&conn, read_only)?;
    Ok(conn)
}

/// Pragmas for a single-user desktop app.
///
///  * `journal_mode = WAL` — the reason the reader pool works at all. Readers
///    see a consistent snapshot while the sync loop commits, so the UI never
///    waits on a write. Persistent, stored in the file header; setting it again
///    on later opens is a no-op. (Ignored by in-memory databases.)
///  * `synchronous = NORMAL` — under WAL this fsyncs at checkpoints rather than
///    at every commit. An application crash, or the OS killing us, still cannot
///    corrupt or lose a committed transaction; only a power cut or kernel panic
///    can drop the tail of the log. That trade is right here: this database is a
///    rebuildable cache of Gmail, a lost tail is re-fetched from the stored
///    `historyId` on next sync, and `FULL` would put an fsync in the middle of
///    every batch of a 12-month backfill.
///  * `foreign_keys = ON` — off by default in SQLite, and per-connection, so it
///    has to be set on every connection we open or cascades silently stop.
///    (`recursive_triggers` is deliberately *not* set: it was assumed to be
///    needed for the FTS delete trigger to fire on rows removed by
///    `ON DELETE CASCADE`, and measurement showed it is not — SQLite fires
///    those triggers either way. `cascading_a_thread_delete_also_clears_the_index`
///    pins that behaviour by asking `messages_fts` directly.)
///  * `busy_timeout` — a second writer (or a checkpointer) should wait, not
///    fail. Five seconds is far longer than any write here takes.
///  * `query_only` on pool readers — the engine, not code review, enforces that
///    UI reads cannot write.
fn apply_pragmas(conn: &Connection, read_only: bool) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -16000;",
    )?;
    if read_only {
        conn.execute_batch("PRAGMA query_only = ON;")?;
    }
    Ok(())
}
