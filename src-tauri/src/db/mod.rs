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
//!
//! # Why the writer is not just a mutex
//!
//! It was, and that was the lag. A sync batch holds the write connection for a
//! median of 13 ms against the owner's mailbox, which is nothing; but the sync
//! loop releases the lock and immediately asks for it again, and `Mutex` makes
//! no fairness promise. On macOS the thread that just unlocked reliably wins the
//! relock against a thread that was already waiting. So a keystroke did not wait
//! for *a* batch, it waited for however many batches happened before it got
//! lucky. Measured on a generated store of the owner's shape: p50 34 ms, p95
//! 1.6 s, worst case 4.5 s, for a write whose own work is 0.4 ms.
//!
//! [`Db::write_background`] is the fix, and it is a scheduling change rather
//! than a SQLite one. Every caller that is not the sync engine keeps
//! [`Db::write`], which registers itself as waiting before it queues on the
//! mutex; a background writer parks before *each* batch for as long as any
//! interactive writer is registered. A user command therefore waits for at most
//! one batch already in progress, which is the shortest wait that does not
//! involve aborting work the sync loop has already done.
//!
//! The standoff has a timeout ([`BACKGROUND_STANDOFF`]) so that a user holding
//! the keyboard down cannot stop mail syncing altogether; on expiry the sync
//! loop simply queues for the mutex as it used to.
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

pub mod backfill;
pub mod command_queries;
pub mod models;
pub mod queries;
pub mod schema;
pub mod sync_queries;

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// How many read connections we keep parked. Five accounts, one list view, one
/// reading pane, one search box — four idle readers is more than the UI can use
/// at once, and each costs only a file handle plus a page cache.
const MAX_IDLE_READERS: usize = 4;

/// The longest a background writer stands off for interactive writes before
/// queueing for the connection anyway.
///
/// Without it, a burst of commands arriving faster than they are served would
/// stop mail syncing for as long as the burst lasted. With it the sync loop
/// falls back to the ordinary queue, so the worst this bound can cost a user
/// command is the same wait it had before — and reaching it takes a command
/// pending continuously for a quarter of a second, which a keyboard does not
/// produce.
const BACKGROUND_STANDOFF: Duration = Duration::from_millis(250);

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
    /// How many interactive writers are queued for `writer` but have not yet
    /// taken it. Background writers wait for this to reach zero.
    ///
    /// A count under its own mutex rather than an atomic, because a background
    /// writer sleeps on it and a `Condvar` needs a mutex to make "check, then
    /// sleep" atomic against "decrement, then wake".
    interactive_waiting: Mutex<usize>,
    /// Where a background writer sleeps while `interactive_waiting` is non-zero.
    quiet: Condvar,
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
                interactive_waiting: Mutex::new(0),
                quiet: Condvar::new(),
            }),
        })
    }

    /// The directory this store lives in, when it lives anywhere.
    ///
    /// `None` for an in-memory database, which is what every test opens. That
    /// distinction is the whole reason this returns an `Option`: the attachment
    /// cache is a sibling of `mach.db`, and answering with a *relative*
    /// directory for a store that has none would have a test write a cache into
    /// whatever the working directory happened to be.
    pub fn directory(&self) -> Option<PathBuf> {
        let uri = &self.inner.uri;
        if uri.starts_with("file:") && uri.contains("mode=memory") {
            return None;
        }
        Path::new(uri)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
    }

    /// The single write connection, for work a person is waiting on. Held for
    /// the duration of the guard, so keep the critical section to one logical
    /// unit of work.
    ///
    /// Registers itself as waiting before it queues, which is what makes the
    /// sync loop stand aside at its next batch boundary rather than relocking
    /// ahead of it. See the module doc.
    ///
    /// A poisoned mutex is recovered rather than propagated: a panic in one
    /// command must not take the whole store offline, and SQLite's own
    /// transaction rollback has already restored consistency.
    pub fn writer(&self) -> MutexGuard<'_, Connection> {
        *lock(&self.inner.interactive_waiting) += 1;
        let conn = lock(&self.inner.writer);
        // Deregister on *acquisition*, not on release. The point of the count is
        // to stop a background writer starting a new batch while somebody is
        // queued; once we hold the connection it is the mutex that keeps it out,
        // and holding the count any longer would delay the sync loop for no
        // further benefit.
        let mut waiting = lock(&self.inner.interactive_waiting);
        *waiting -= 1;
        if *waiting == 0 {
            self.inner.quiet.notify_all();
        }
        drop(waiting);
        conn
    }

    /// The single write connection, for work nobody is waiting on.
    ///
    /// Waits for every queued interactive writer first, so a sync batch cannot
    /// start while a keystroke is pending. The wait is bounded by
    /// [`BACKGROUND_STANDOFF`]; after that this queues like any other writer.
    pub fn background_writer(&self) -> MutexGuard<'_, Connection> {
        let mut waiting = lock(&self.inner.interactive_waiting);
        let mut left = BACKGROUND_STANDOFF;
        while *waiting > 0 && !left.is_zero() {
            let started = std::time::Instant::now();
            let (guard, timeout) = self
                .inner
                .quiet
                .wait_timeout(waiting, left)
                .unwrap_or_else(|p| p.into_inner());
            waiting = guard;
            if timeout.timed_out() {
                break;
            }
            left = left.saturating_sub(started.elapsed());
        }
        drop(waiting);
        lock(&self.inner.writer)
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
    ///
    /// Interactive. Use [`Db::write_background`] for anything the sync engine
    /// does on its own schedule.
    pub fn write<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// The same, for work nobody is waiting on.
    ///
    /// Identical transaction semantics — same isolation, same durability, same
    /// rollback on `Err`. The only difference is when it is allowed to start.
    pub fn write_background<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.background_writer();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Fold the write-ahead log back into the database file and truncate it,
    /// but only once it is worth the stall, and only if nothing is reading.
    ///
    /// # Why this is not left to SQLite
    ///
    /// The automatic checkpoint runs after a commit that leaves the log over
    /// `wal_autocheckpoint` pages, and it runs in `PASSIVE` mode: it copies what
    /// it can and gives up the moment a reader is using the log, without
    /// resetting the file. That is the right default for a process that goes
    /// quiet between writes. This one does not — the reader pool answers the UI
    /// continuously while the sync loop writes — so on a busy mailbox the
    /// passive checkpoint never once completes and the log only ever grows.
    /// Measured on a generated store of the owner's shape, with four readers
    /// busy: 139 MB of log after 3,200 messages written, rising linearly, and
    /// never falling. His own log had reached 814 MB, about a third of the
    /// store's total footprint on disk.
    ///
    /// So the checkpoint is asked for explicitly: from the gap between sync
    /// passes, and from the backfill writer between batches, which are the two
    /// moments with no transaction of their own to interrupt. It goes through
    /// [`Db::background_writer`], so like any other background write it cannot
    /// start while a user command is queued.
    ///
    /// `TRUNCATE` rather than `PASSIVE` because shrinking the file is the whole
    /// point, and rather than `FULL` because `FULL` leaves the pages in place.
    /// It blocks writers for its duration and waits for readers to finish; on a
    /// 139 MB log that measured 61 ms, and it scales with the log, so the first
    /// run against a very large one costs a few hundred milliseconds once.
    /// A checkpoint that cannot get in returns `SQLITE_BUSY`, which is reported
    /// as `Ok(false)` — there is another gap in a minute.
    ///
    /// Nothing here risks data: a checkpoint moves committed pages from one file
    /// to the other and is exactly as crash-safe as the commits that produced
    /// them.
    pub fn checkpoint_if_large(&self, over_bytes: u64) -> Result<bool> {
        let wal = format!("{}-wal", self.inner.uri);
        match std::fs::metadata(&wal) {
            Ok(meta) if meta.len() >= over_bytes => {}
            // No log, or one that is not costing anything yet. In-memory
            // databases have no file and land here too.
            _ => return Ok(false),
        }
        let conn = self.background_writer();
        // A busy checkpoint is not an error; it is "later".
        match conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            row.get::<_, i64>(0)
        }) {
            Ok(busy) => Ok(busy == 0),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Take a mutex, recovering rather than propagating poison. A panic in one
/// command must not take the whole store offline.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
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
///  * `wal_autocheckpoint` — raised far above its default of 1000 pages, and
///    replaced in practice by [`Db::checkpoint_if_large`]. The automatic
///    checkpoint runs **inside the commit** of whichever transaction pushed the
///    log past the threshold, so with the default it lands on one sync batch in
///    seven and turns a 11 ms batch into a 350 ms one — a stall that a user
///    command then waits behind. Moving it out of the commit path is what takes
///    the worst case from 346 ms to under a batch. It is raised rather than
///    disabled so that a build which somehow never calls the explicit
///    checkpoint still has a backstop, instead of growing the log until the
///    disk fills.
///  * `busy_timeout` — a second writer (or a checkpointer) should wait, not
///    fail. Five seconds is far longer than any write here takes.
///  * `query_only` on pool readers — the engine, not code review, enforces that
///    UI reads cannot write.
fn apply_pragmas(conn: &Connection, read_only: bool) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA wal_autocheckpoint = 65536;
         PRAGMA busy_timeout = 5000;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -16000;",
    )?;
    if read_only {
        conn.execute_batch("PRAGMA query_only = ON;")?;
    }
    Ok(())
}
