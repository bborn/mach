//! The attachment byte store: a content-addressed cache under the app data dir.
//!
//! Attachment *metadata* has always been synced — filename, type, size, and the
//! Gmail id that would fetch the bytes. The bytes themselves had nowhere to go,
//! so the paperclip in the thread list was a promise the app could not keep.
//! This module is where they go.
//!
//! # Why the path is a hash and not the filename
//!
//! The single most important decision here. A cache entry lives at
//!
//! ```text
//! <data dir>/attachments/<aa>/<aaaa…>/<sanitized filename>
//! ```
//!
//! where `<aaaa…>` is a SHA-256 over the account id, the Gmail message id and
//! the part id, and `<aa>` is its first two characters (a fan-out directory, so
//! a mailbox with ten thousand cached attachments does not put ten thousand
//! entries in one directory).
//!
//! The sender controls the filename. The sender does **not** control any part of
//! the directory path. That is the property that makes traversal structurally
//! impossible rather than merely defended against: even if
//! [`names::safe_filename`] were wrong — and [`names::is_safe_component`] is
//! asserted immediately before the write precisely because it might be — the
//! worst a hostile name could do is misname a file inside a directory that is
//! already unique to that one attachment.
//!
//! It also answers the collision question directly. Gmail's attachment ids are
//! scoped to a message, so two different messages can and do carry the same id
//! for entirely different bytes; and one account's ids mean nothing in another
//! account. Hashing all three together is what makes "two messages with the
//! same attachment id" two different cache entries. The three fields are
//! length-prefixed into the hash so that no rearrangement of the boundaries
//! between them can produce the same digest.
//!
//! # Size cap and eviction
//!
//! Two limits, chosen for different reasons.
//!
//! * [`MAX_ATTACHMENT_BYTES`] (64 MiB) is a **refusal**, not an eviction. Gmail
//!   itself will not deliver a message over 50 MB, so anything past this is
//!   either impossible or an attempt to make the app allocate. It is checked
//!   against the stored size *before* a request goes out, and again against the
//!   response, because the stored size came from the same sender.
//!
//! * [`MAX_CACHE_BYTES`] (512 MiB) is the total. When a write pushes the cache
//!   over it, whole entries are deleted oldest-first until the total is back
//!   under [`EVICT_TO_BYTES`] — a low-water mark, so eviction runs occasionally
//!   and does real work rather than running on every write and freeing one file.
//!
//! Age is read from mtime, and mtime is refreshed on every cache *hit*, which
//! makes the policy least-recently-used rather than first-in-first-out. It is
//! worth the one `set_times` call: without it, the attachment the owner opens
//! every week is exactly the one that ages out, and the cheapest entry to
//! rebuild is the one nobody wants.
//!
//! Evicting is always safe. A missing entry costs one HTTPS request; nothing in
//! this cache is the only copy of anything.

pub mod names;

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// Largest single attachment Mach will fetch.
///
/// Gmail's own inbound limit is 50 MB, so this has headroom over anything that
/// can actually arrive while still being a number the process can hold.
pub const MAX_ATTACHMENT_BYTES: usize = 64 * 1024 * 1024;

/// Largest inline `cid:` image, which is a tighter limit than the one above
/// because these bytes end up base64'd into a message frame's `srcdoc` rather
/// than on disk being handed to another program. The same number
/// `render::sanitize` uses for a `data:` image, so the two ways a picture can
/// arrive inline cost the same ceiling.
pub const MAX_INLINE_IMAGE_BYTES: usize = crate::render::sanitize::MAX_DATA_URI_BYTES;

/// High-water mark for the whole cache.
pub const MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;

/// Low-water mark. Eviction runs down to here, not to the cap.
pub const EVICT_TO_BYTES: u64 = MAX_CACHE_BYTES * 4 / 5;

/// The directory under the app data dir. Sibling of `mach.db`, so an instance
/// launched with its own `MACH_DATA_DIR` gets its own attachment cache too and
/// QA can never read bytes out of the mailbox someone is using.
pub const CACHE_DIR_NAME: &str = "attachments";

/// Files being written are named with this prefix and renamed into place, so a
/// crash mid-download leaves a fragment that is visibly not a cache entry
/// rather than a truncated file that is indistinguishable from a complete one.
const PARTIAL_PREFIX: &str = ".part-";

/// Which part of a message an entry holds.
///
/// The discriminant goes into the hash, so a `cid:` lookup and an attachment
/// lookup can never land on each other even in the pathological case where a
/// Content-ID and an attachment id are the same string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    /// A file the reader would call an attachment, by Gmail attachment id.
    File,
    /// An image the body refers to with `cid:`, by Content-ID.
    Inline,
}

impl PartKind {
    fn tag(self) -> &'static str {
        match self {
            PartKind::File => "att",
            PartKind::Inline => "cid",
        }
    }
}

/// The cache key for one part of one message on one account.
///
/// Every field is written into the digest behind its own length, so
/// `("ab", "c")` and `("a", "bc")` cannot collide. Without that the boundary
/// between a message id and an attachment id would be guessable, and a sender
/// who controls part of one could aim at another.
pub fn cache_key(
    account_id: i64,
    gmail_message_id: &str,
    kind: PartKind,
    part_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mach-attachment-v1");
    for field in [
        account_id.to_string().as_bytes(),
        gmail_message_id.as_bytes(),
        kind.tag().as_bytes(),
        part_id.as_bytes(),
    ] {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field);
    }
    hex::encode(hasher.finalize())
}

/// One cached file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedFile {
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// The on-disk cache.
#[derive(Debug, Clone)]
pub struct AttachmentCache {
    root: PathBuf,
    max_total_bytes: u64,
    evict_to_bytes: u64,
}

impl AttachmentCache {
    /// Rooted at `<data_dir>/attachments`. The directory is created lazily, on
    /// the first write — an app that never opens an attachment should not leave
    /// an empty directory behind to explain.
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        AttachmentCache {
            root: data_dir.as_ref().join(CACHE_DIR_NAME),
            max_total_bytes: MAX_CACHE_BYTES,
            evict_to_bytes: EVICT_TO_BYTES,
        }
    }

    /// Smaller limits, for tests that would otherwise have to write half a
    /// gigabyte to observe an eviction.
    pub fn with_limits(mut self, max_total_bytes: u64, evict_to_bytes: u64) -> Self {
        self.max_total_bytes = max_total_bytes;
        self.evict_to_bytes = evict_to_bytes.min(max_total_bytes);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where one entry's single file lives. Derived entirely from the hash;
    /// nothing a sender wrote reaches this path.
    pub fn entry_dir(&self, key: &str) -> PathBuf {
        // A key is hex from `cache_key`, but this is a public method and a
        // two-byte slice would panic on a short key or a multibyte first
        // character. `get` degrades instead of trusting its caller.
        let prefix = key.get(..2).unwrap_or("00");
        self.root.join(prefix).join(key)
    }

    /// The cached bytes for a key, if we already have them.
    ///
    /// The entry directory holds exactly one real file and its name is not
    /// known to the caller (a re-sync can change the sender's filename without
    /// changing the bytes), so the directory is read rather than a path being
    /// guessed. A hit refreshes mtime, which is what makes eviction LRU.
    pub fn find(&self, key: &str) -> Option<CachedFile> {
        let dir = self.entry_dir(key);
        let entries = fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(PARTIAL_PREFIX) {
                continue;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                continue;
            }
            let path = entry.path();
            touch(&path);
            return Some(CachedFile {
                path,
                size_bytes: metadata.len(),
            });
        }
        None
    }

    /// Write bytes for a key under `filename`, replacing whatever was there.
    ///
    /// `filename` must already have been through [`names::safe_filename`]; this
    /// re-checks it with [`names::is_safe_component`] and refuses rather than
    /// repairing, because at this point a name that needs repairing means a bug
    /// upstream and the right outcome is a visible failure rather than a file
    /// written somewhere surprising.
    pub fn store(&self, key: &str, filename: &str, bytes: &[u8]) -> io::Result<CachedFile> {
        if !names::is_safe_component(filename) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing to cache under the name {filename:?}"),
            ));
        }

        let dir = self.entry_dir(key);
        fs::create_dir_all(&dir)?;

        // Clear the entry first. A sender who re-sends the same part under a
        // different name would otherwise leave both files here, and `find`
        // would answer with whichever the directory listed first.
        clear_dir(&dir);

        let partial = dir.join(format!("{PARTIAL_PREFIX}{}", unique_suffix()));
        {
            let mut file = File::create(&partial)?;
            file.write_all(bytes)?;
            // Flushed and synced before the rename: the rename is what makes
            // the entry visible, and it must never become visible ahead of the
            // bytes it names.
            file.flush()?;
            let _ = file.sync_all();
        }

        let final_path = dir.join(filename);
        // The one place a sender-influenced string becomes a path. If it did
        // not stay inside the entry directory, nothing gets written.
        if final_path.parent() != Some(dir.as_path()) {
            let _ = fs::remove_file(&partial);
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{filename:?} does not stay inside its cache entry"),
            ));
        }

        fs::rename(&partial, &final_path)?;
        self.enforce_budget(key);

        Ok(CachedFile {
            path: final_path,
            size_bytes: bytes.len() as u64,
        })
    }

    /// Total bytes held, across every entry.
    pub fn total_bytes(&self) -> u64 {
        self.entries().iter().map(|e| e.size).sum()
    }

    /// How many entries are cached. Test and diagnostics affordance.
    pub fn entry_count(&self) -> usize {
        self.entries().len()
    }

    /// Delete oldest-first until the cache is under the low-water mark.
    ///
    /// `protect` is the entry that was just written: evicting it would turn a
    /// successful download into a cache miss and the next open would fetch it
    /// again, forever, for any file large enough to trip the cap on its own.
    pub fn enforce_budget(&self, protect: &str) {
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|e| e.size).sum();
        if total <= self.max_total_bytes {
            return;
        }

        // Oldest first. mtime is refreshed by `find`, so "oldest" means "least
        // recently used", not "downloaded longest ago".
        entries.sort_by_key(|e| e.modified);
        for entry in entries {
            if total <= self.evict_to_bytes {
                break;
            }
            if entry.key == protect {
                continue;
            }
            if fs::remove_dir_all(&entry.dir).is_ok() {
                total = total.saturating_sub(entry.size);
            }
        }
    }

    /// Every entry directory, with its size and age.
    fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let Ok(buckets) = fs::read_dir(&self.root) else {
            return out;
        };
        for bucket in buckets.flatten() {
            if !bucket.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Ok(keys) = fs::read_dir(bucket.path()) else {
                continue;
            };
            for key_dir in keys.flatten() {
                if !key_dir.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let dir = key_dir.path();
                let (size, modified) = dir_stats(&dir);
                out.push(Entry {
                    key: key_dir.file_name().to_string_lossy().into_owned(),
                    dir,
                    size,
                    modified,
                });
            }
        }
        out
    }
}

struct Entry {
    key: String,
    dir: PathBuf,
    size: u64,
    modified: SystemTime,
}

fn dir_stats(dir: &Path) -> (u64, SystemTime) {
    let mut size = 0u64;
    let mut newest = SystemTime::UNIX_EPOCH;
    if let Ok(files) = fs::read_dir(dir) {
        for file in files.flatten() {
            if let Ok(metadata) = file.metadata() {
                if metadata.is_file() {
                    size += metadata.len();
                    if let Ok(modified) = metadata.modified() {
                        if modified > newest {
                            newest = modified;
                        }
                    }
                }
            }
        }
    }
    (size, newest)
}

fn clear_dir(dir: &Path) {
    if let Ok(files) = fs::read_dir(dir) {
        for file in files.flatten() {
            let _ = fs::remove_file(file.path());
        }
    }
}

/// Best-effort mtime refresh. A failure here costs nothing but eviction
/// accuracy, so it is deliberately silent — a read-only volume or a file being
/// evicted concurrently must not turn a cache hit into an error.
fn touch(path: &Path) {
    if let Ok(file) = File::options().write(true).open(path) {
        let times = fs::FileTimes::new().set_modified(SystemTime::now());
        let _ = file.set_times(times);
    }
}

/// Enough to keep two concurrent downloads of the same entry from writing to
/// one temporary file. Not a security value — the file is renamed into a
/// directory only this process can name.
fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{:x}-{:x}", nanos, n)
}
