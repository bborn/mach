# Data at rest

`docs/security-threat-model.md` covers what an attacker gets by sending mail.
This covers the last item on its list — "whatever bytes land on disk" — and the
question behind it: 47 000 threads of real mail are sitting in a SQLite file in
the home directory, and is that a problem.

The short answer is that the file is close to the least of it, and the machine
it is on is missing the control that would make the file irrelevant.

## Method, and what is cited

Everything about this machine and this store was measured on 2026-08-14 by
reading the filesystem and opening the store read-only. No message content, no
address and no token was printed at any point; the queries were counts and
lengths.

The web **search** budget was exhausted before this session began — `WebSearch`
returns "this session has used its web search budget (200 of 200)". `WebFetch`
still worked, so external research is from fetching known URLs and reading
source directly. Claims are marked **cited** where a page or a source file was
fetched, and **recalled** where they are unverified knowledge.

Three areas were researched: what other mail clients do, what SQLCipher costs,
and what macOS actually guarantees. Sources are linked inline.

---

## The answer, first

1. **Turn on FileVault.** It is off. This is the whole finding; everything else
   on this page is small next to it. (§ *FileVault is off*)
2. **Do not put SQLCipher in Mach.** It would cost real speed on the exact read
   this app is built around, and it does not defend against the threat that is
   actually live here. Delta Chat built it and then deprecated it, for reasons
   that apply here word for word. (§ *What encrypting the store would buy*)
3. **Four cheap things** are worth doing, one of which is done in this branch.
   (§ *What I would do*)

The threat that fires most often is the 486 processes running as uid 501 on this
machine right now, several of them agents with filesystem access. Nothing Mach
can do to its own file addresses that, because any key Mach can use is a key
they can get.

---

## What is on disk

`~/Library/Application Support/com.mach.mail/`:

| | size | rows |
|---|---|---|
| `mach.sqlite3` | 1.21 GB | 47 336 threads, 67 292 messages, 5 accounts |
| ` └ body_html` | 569.8 MB | 20 251 messages |
| ` └ body_text` | 317.3 MB | 61 530 messages |
| ` └ messages_fts_data` | 182.2 MB | the inverted index |
| ` └ snippet` | 12.9 MB | every thread and message |
| `mach.sqlite3-wal` | 3.9 MB | |
| `attachments/` | 16 MB | 109 cached files, against 2 447 rows in `attachments` |
| `agent/mcp-*.json` | 176 B each | three stale MCP session tokens |

Three things about that table.

**`body_text` is the readable content, and `evict` does not touch it.**
`src-tauri/src/evict/mod.rs` drops `body_html` for mail older than 90 days,
which is why only 20 251 of 67 292 messages still have HTML. That is a disk
control. It is not a privacy control: the text of all 61 530 messages stays, and
it is the text a reader would want.

**The FTS index is a plaintext concordance of the mailbox.** `messages_fts` is
declared `content = 'messages'` (`db/schema.rs:964`), so it stores no document
text — but external-content FTS5 still stores the full term dictionary and the
per-document positions in `messages_fts_data`. 182 MB of it. Anyone holding the
file can enumerate every word that appears in every message, and which message
each word is in, without reading a single body. This is what rules out
encrypting the bodies and leaving the rest alone, which is otherwise the obvious
cheap option.

**Attachments are ordinary files outside any database.** The cache holds 109
files — 76 PNG, 31 JPG, one PDF, one HTML — which is small because
`ipc/attachments.rs:9` fetches nothing without a click, so it contains only what
was actually opened. Spotlight indexes them; I checked every one and **none has
a `kMDItemTextContent` attribute**, so no extracted text has leaked into
`/System/Volumes/Data/.Spotlight-V100`. That is a property of the mix rather
than a control: cache one text-bearing PDF and its contents land in the system
index, outside anything Mach could encrypt. This is Delta Chat's blob argument
in miniature, and it is why encrypting the database alone is a boundary with a
hole in it.

There is a second full copy of a real mailbox on this disk, which is covered
under *Second copies* below.

---

## The threat model

Ordered by how likely each is to happen to this one user.

### T1 — A process running as uid 501. Live, and not hypothetical.

There are 486 processes running as `bruno` right now: 19 `claude`, 32 `node`, 14
`ty`, 14 `chrome-devtools-mcp`, 11 `honeybadger-mcp-server`, 9 `hatchbox-mcp`,
plus Dropbox and iCloud. Every one of them can `open()` the store. Writing this
document, I did: opened `mach.sqlite3` read-only from Python and read row counts
out of it, with no prompt, no entitlement check and nothing in any log.

`~/Library/Application Support/<bundle id>` is not a TCC-protected path. Apple's
[Controlling app access to files in
macOS](https://support.apple.com/guide/security/controlling-app-access-to-files-secddd1d86a6/web)
(*cited*) names only Documents, Downloads, Desktop, iCloud Drive, network and
removable volumes as consent-gated, and the Full-Disk-Access set is the
hard-coded list of system stores — Mail, Messages, Safari history, cookies, call
history, backups, Trash. `~/Library/Mail` is on it. A third-party mail client's
own directory is not, and cannot be: the path→service mapping is compiled into
the sandbox rather than configurable. Apple Mail's store sits behind a consent
prompt that Mach's structurally cannot.

Five things on this machine hold Full Disk Access outright, from the system TCC
database: Claude Code (two versions), 1Password 7, Backblaze, Dropbox, and
iTerm2. For those, even the TCC paths are open.

**Application-level encryption does not address this.** Any key Mach can use to
open the store is a key a process running as Mach's user can obtain — from the
Keychain if the ACL can be satisfied or the prompt clicked through, and from
Mach's own memory if it cannot. The login keychain on this machine is set
`no-timeout`, so it is unlocked from login until logout.

### T2 — The laptop is lost or stolen. Half closed, and it should be all closed.

This is a MacBook Pro; it leaves the house. Against someone who wants to resell
it, the mail is already safe. Against someone who wants the mail, FileVault
being off is the difference between a cryptographic barrier and a software one.
The next section is that argument in full.

### T3 — The machine is borrowed, seized, or handed over unlocked.

Nothing on this page helps. FileVault is irrelevant once the volume is mounted,
and so is SQLCipher once Mach has opened the store. The controls that apply are
screen lock — set to lock immediately on this machine, with no auto-login — and
not handing over an unlocked laptop.

### T4 — Backups. Currently zero exposure, one reinstall away from full exposure.

No Time Machine destination is configured (`tmutil destinationinfo`: "No
destinations configured"). There are no local APFS snapshots. The store is not
inside `~/Dropbox` or any `~/Library/CloudStorage` provider.

If Time Machine were ever turned on, the store would be in it. Apple's
[exclusion
documentation](https://support.apple.com/guide/mac-help/exclude-files-from-a-time-machine-backup-mh15622/mac)
(*cited*) names only the backup disk itself as automatically excluded, and
nothing covers `~/Library/Application Support`. Backup encryption is a checkbox
("**If you turn on Encrypt Backup**…", *cited*), not a default, so an
unencrypted destination would be a full plaintext copy of the mailbox on
removable media with no Secure Enclave behind it. Local snapshots would also
land on the internal Data volume and stay up to 24 hours, inheriting whatever
FileVault posture the machine has.

Backblaze is the interesting one. It is installed, holds Full Disk Access,
`bzserv` is running as root, and its licence is `billing_active` on an
`unlimitedstorage_billing` plan. It has also not backed up anything since **26
February 2026**: `bzfileids.dat` and the file lists are dated then, and today's
`bztransmit` log contains only `-synchostinfo` heartbeats and a
`BzClientVersionManager::IsEndOfLife` warning about an invalid client version.
The Mach store was created on 7 August, so it has never been uploaded.

So the mail is not in anyone's cloud today, by accident rather than by decision.
Nothing in Backblaze's exclusion rules covers `~/Library/Application Support`
— the only mention of that path in either rules file is a comment about Firefox.
Repair or reinstall that client and 1.21 GB of mail starts uploading the same
day. Backblaze holds the encryption key unless a Personal Encryption Key is set,
and even then a restore requires handing them the key so they can build the
archive server-side (*recalled*).

The flip side: there is currently no working backup of this machine at all.

### T5 — Fragments outside the store.

- **Swap: covered.** `sysctl vm.swapusage` reports `(encrypted)` — 15.36 GB
  allocated, 14.67 GB in use, encrypted with an ephemeral key. Message text
  paged out of Mach's address space is not recoverable from `/System/Volumes/VM`.
- **Crash reports: none.** `~/Library/Logs/DiagnosticReports` contains no Mach
  crash. If one appeared it would carry stack memory, not the store.
- **Logs: clean.** No `println!`, `eprintln!` or `tracing` call in the tree
  formats `body_html`, `body_text`, `subject` or `snippet`. There is no
  `~/Library/Logs/com.mach.mail`.
- **WebKit caches: 652 MB, mostly the dev server.** `~/Library/Caches/mach`
  holds 30 825 `NetworkCache` entries; 13 830 of them reference `vite`, `.tsx`
  or `node_modules`, which is the Vite module graph from `bun run dev`. Grepping
  for common newsletter and image-host domains returns zero files, which fits —
  the sanitizer blocks remote images by default and message bodies arrive over
  IPC rather than HTTP, so mail does not pass through this cache.
  `~/Library/WebKit/mach` is 4.6 MB of WebsiteData.
- **Free pages: negligible.** `secure_delete` is off, so deleted rows are not
  zeroed, but `freelist_count` is **1**. There is no meaningful pile of deleted
  mail in the file's dead space.

### T6 — Second copies.

`/Users/bruno/Projects/mach/.qa/agent/data/mach.sqlite3` is **1.38 GB** and holds
**40 383 messages across 28 597 threads for one real account**. A QA instance
signed into a real mailbox and backfilled it. It was last written on 11 August.

It is gitignored (`.gitignore:37`), so it will not be committed. It is also more
exposed than the owner's own store, because of where it is:

```
/Users/bruno              drwxr-x---  bruno staff
/Users/bruno/Projects     drwxr-xr-x  bruno staff
  …/mach/.qa/agent/data   drwxr-xr-x  bruno staff
  …/data/mach.sqlite3     -rw-r--r--
```

`/Users/bruno` is group-readable by `staff`, and on macOS every local account is
in `staff` by default. `~/Library/Application Support` is `0700`, so the owner's
real store is unreachable that way — but the QA store is `0644` at the end of a
`0755` path, so a second account on this Mac could read 40 000 messages out of
it. There is only one account (uid 501) today, so this is latent. It is still
the app's own file mode carrying a difference it did not know about, which is
what the change in this branch fixes.

Also on disk: `.env.local` holds `MACH_GOOGLE_CLIENT_SECRET`, is `0644`, and
`bin/worktree-setup` has copied it into **14** agent worktrees.

---

## FileVault is off

```
$ fdesetup status
FileVault is Off.

$ diskutil apfs list
  APFS Volume Disk (Role):   disk3s5 (Data)
  FileVault:                 No (Encrypted at rest)
```

That parenthetical is what makes this confusing, and "Apple silicon encrypts
everything anyway, so FileVault does not matter" is a claim that deserves taking
apart, because its premise is true and its conclusion is not.

Apple's Platform Security guide, [Volume encryption with
FileVault](https://support.apple.com/guide/security/volume-encryption-with-filevault-sec4c6dc1b6e/web)
(*cited*):

> If FileVault isn't turned on in a Mac with Apple silicon or a Mac with the T2
> chip during the initial Setup Assistant process, the volume is still encrypted
> but the volume encryption key is protected only by the hardware UID in the
> Secure Enclave.

With FileVault on, the key encryption key "is protected by a combination of the
user's password and hardware UID". That is the whole difference: one ingredient,
the password, added to the wrap.

**What is already true with FileVault off**, and is where the myth comes from.
Pulling the SSD is dead — the NAND is soldered and the UID is fused
into the Secure Enclave, so raw ciphertext off-machine is useless either way.
Target disk mode does not exist on Apple silicon (Platform Security, *cited*).
DFU exposes no storage. Booting an external OS requires an authenticated restart
from recoveryOS that writes a LocalPolicy to the internal drive (*cited*). And
Recovery itself always asks: Apple's [If you forgot your Mac login
password](https://support.apple.com/en-us/HT202860) (*cited*) — "While starting
up from Recovery, you're asked to select a user you know the password for" —
with no FileVault condition on that sentence. Share Disk lives in Recovery's
Utilities menu, behind that same gate. A thief who grabs this laptop off a café
table does not read the mail; the resale path is Erase Mac, which destroys it.

**What changes when FileVault is on** is the *kind* of barrier. With it off,
every one of those gates is an authorization decision made in software and
Secure Enclave policy, while the machine is holding a key that can decrypt the
volume the moment the decision goes the other way. With it on, the gate is
arithmetic: no password, no key, nothing to decide.

That distinction pays off against the attacks that are not the café thief — an
Apple Account takeover driving the documented password-reset flow, a bug in
recoveryOS, a password that was reused or shoulder-surfed, coercion, forensic
tooling with the device in hand and unlimited time. With FileVault off, a
successful password reset is sufficient: the volume key was never bound to the
credential. With it on, resetting the password gets nothing without the old
password, the recovery key or the iCloud escrow. Resistance to offline
brute-force is one of the four goals Apple lists for the FileVault hierarchy
specifically.

Apple has since settled the argument by changing the default. From [Data
Protection
overview](https://support.apple.com/guide/security/data-protection-overview-secf6276da8a/web)
(*cited*): "On a Mac with macOS 26.4 or later, FileVault is turned on by
default." This machine is on 15.2.

The cost of turning it on is a password at boot, and Apple notes enabling it
later is "immediate because the data has already been encrypted", with an
anti-replay mechanism retiring the old UID-only key. No re-encryption, no
performance change.

Encrypting `mach.sqlite3` while the volume under it unlocks itself is answering
the second question first.

---

## What already protects the store

Some of this is better than it looks, and none of it should be "hardened".

- **`~/Library/Application Support` is `0700`**, with an ACL denying `everyone`
  delete. The store directory inside it is `0755` and the store itself was
  `0644`, but nothing without the traverse bit on the parent can reach either.
  This is why T6's QA copy is the exposed one and the real store is not.
- **Only the refresh token is persisted**, in the Keychain, per account.
  Access tokens are memory-only (`auth/tokens.rs:261`), and `Secret`
  (`tokens.rs:128`) implements `Debug` as a redaction with no `Display`,
  `Serialize` or `AsRef<str>`, so reading one requires `expose()`.
- **A QA instance cannot name the owner's Keychain entries.**
  `keychain_service()` (`tokens.rs:65`) namespaces every non-owner instance, so
  an agent's build asking for the owner's refresh token gets
  `errSecItemNotFound` instead of raising a modal on the owner's screen.
- **The MCP session token is `0600`** (`agent/mcp.rs:512`) and the QA bearer
  token is `0600` (`qa/mod.rs:544`), with a test asserting it. The store was the
  one sensitive file in the app that did not get the same treatment.
- **`temp_store = MEMORY`** (`db/mod.rs:436`) keeps sort and temp-table spill
  out of `/var/folders`.
- **Swap is encrypted independently of FileVault.** The Platform Security guide
  describes the VM volume as "unencrypted and… used by macOS for storing
  encrypted swap files" (*cited*), and `sysctl` reports `(encrypted)` on this
  machine with FileVault off, which demonstrates the independence directly. The
  keys are per-boot, so swap is not a recovery path in either configuration.
- **Screen lock is immediate**, there is no auto-login, and there is exactly one
  account on the machine.
- **The app logs no message content at all.**

The Keychain's own limits set the ceiling on any encryption design Mach could
adopt. Keychain ACLs bind to the requesting process's code signature — Apple's TN2206 says the creating application "determines the
access policy using code signing requirements" (*cited*). Mach's dev binary is
`adhoc, linker-signed` with `TeamIdentifier=not set`, and the signature changes
on every rebuild, which is exactly why `bin/worktree-setup` warns that "each new
unsigned binary triggers a fresh macOS Keychain prompt". An ACL that re-prompts
on every build is one the owner is being trained to click through. That is the
mechanism any SQLCipher key would have to rely on.

---

## What encrypting the store would buy, and what it would not

### What SQLCipher costs Mach specifically

Zetetic claims "as little as 5-15% overhead"
([performance](https://www.zetetic.net/sqlcipher/performance/), *cited*), and
the same page notes their commercial builds are "up to 3-4x faster than free
Community Edition packages", which is the build `bundled-sqlcipher` gives you.

The honest model is better than the marketing number in one direction and worse
in another. SQLite's page cache holds plaintext — `readDbPage()` decrypts in
place (`src/pager.c:3214`, *cited*) — so a warm read costs nothing at all, and a
page-cache miss costs one AES-256-CBC and one HMAC-SHA512 over 4 KB. Measured on
this M4 Pro with `openssl speed`: AES-256-CBC at 2.14 GB/s and SHA-512 at
~1.4 GB/s, so **~5 µs per 4 KB page miss**. Opening a 100 KB HTML body is ~25
pages, or ~125 µs. Against Mach's budget that is nothing.

Three costs are real, and two of them land on precisely this app's shape:

1. **PBKDF2 at 256 000 iterations is 49 ms per connection open**, measured here.
   Mach opens a writer plus a reader pool. This one is avoidable — a raw hex key
   (`PRAGMA key = "x'…'"`) skips the KDF entirely, and the key would come from
   the Keychain as high-entropy bytes anyway.
2. **The direct overflow-page read is disabled.** `sqlite3PagerDirectReadOk()`
   returns 0 whenever a codec is installed (`src/pager.c:836`, *cited*). That is
   the path SQLite's own source describes as reading "directly from the database
   file into the output buffer, bypassing the page-cache altogether… speeds up
   loading large records that span many overflow pages" (`src/btree.c:5273`).
   Mach's hot read is a large record spanning many overflow pages: 569.8 MB of
   `body_html` at 4 KB pages. Every body read would go through the page cache
   one page at a time and evict the thread list while doing it.
3. **Cold FTS search gets slower.** `messages_fts_data` is 182 MB; a cold match
   walks it with 5 µs of CPU per page miss, CPU-bound rather than I/O-bound. A
   warm one is unchanged.

The mmap loss usually cited does not apply: SQLCipher disables the mmap page
getter whenever a codec is set (`src/pager.c:1078`, *cited*), but Mach never
sets `mmap_size`, so it is not using mmap today. Nothing is lost that was there.

Migration is better than expected. `sqlcipher_export` copies FTS5 shadow tables
as ordinary rowid tables and injects the virtual-table declaration row straight
into `sqlite_schema` (`src/sqlcipher.c:3940-4085`, *cited*), so **the 182 MB
index is copied byte-for-byte rather than re-tokenized**. It does not copy
`user_version`, `auto_vacuum` or `journal_mode`, all of which Mach relies on.
Budget minutes, not seconds, and it has to be resumable and backed up.

### What it would not buy

Against T1, nothing. The key has to live where Mach can reach it without asking
a human, which means the Keychain, which means an ACL evaluated against a code
signature that changes on every rebuild and a prompt whose "Always Allow" button
is one click. And a process that can read the file can generally also read
Mach's memory, where the key and the decrypted pages already are.

Against T2, nothing that FileVault does not do better and cheaper.

Against T4 and T6 it would help: a backup or a stray copy would carry ciphertext.
That is the case for it, and it is narrower than "encrypt the database" usually
implies.

### macOS Data Protection classes

macOS does have Data Protection classes, contrary to the usual assumption that
they are iOS-only. `FileAttributeKey.protectionKey` has been available since
macOS 10.6, `NSFileProtectionNone` is unsupported on macOS, and Class A
(`NSFileProtectionComplete`) has documented macOS semantics: "shortly after the
last user is logged out, the decrypted class key is discarded" ([Data Protection
classes](https://support.apple.com/guide/security/data-protection-classes-secb010e978a/web),
*cited*). The boundary is login and logout, not screen lock as on iPhone.

The default class on macOS is C, which "uses a volume key… that functions just
like FileVault" (*cited*) — so by default it is FileVault or nothing. Apple
invites developers to opt into a higher class, but does not state anywhere what
Class A actually buys on a machine with FileVault **off**, and I could not
establish it. Treat it as something to layer on top of FileVault-on, never as a
substitute for it.

### Column-level encryption of the bodies

This looks like the cheap 80% and it does not work. `messages_fts` indexes
`body_text`, so 182 MB of term dictionary and per-document positions would stay
in the same file in the clear: every word in the mailbox and which message it is
in. Encrypting the bodies while shipping their index next door is worse than
doing nothing, because it looks like it worked.

### An encrypted volume

An encrypted APFS volume, or a sparse bundle, holding the store. Encryption runs
in the SSD controller's AES engine rather than the CPU, so overhead is
approximately zero: mmap works, direct overflow reads work, no KDF at open, no
FTS5 caveats, no `libsqlite3-sys` feature entanglement, no migration beyond
`mv`. If Mach unmounts it on quit, a powered-off or logged-out machine yields
nothing.

The catch is that while it is mounted it is exactly as exposed as today, and to
mount it at login you park the passphrase in the Keychain and land in the same
key-custody problem. It is a real option and it is strictly better value than
SQLCipher; it is still second to turning FileVault on.

---

## What other clients do

The question that separates real designs from theatre is where the key lives.

| Client | Local mail encrypted by the app? | Key |
|---|---|---|
| Thunderbird | No. mbox/maildir plaintext; `global-messages-db.sqlite` plaintext, FTS included | — |
| Apple Mail | No. `.emlx` + SQLite index; relies on FileVault | — |
| Mailspring | No. Plain SQLite. *Credentials* via Electron `safeStorage` | — |
| Nylas Mail | No. Plain SQLite. *Credentials* via `keytar` | — |
| Mimestream | No. "cached email content is stored in a local database"; tokens in Keychain | — |
| eM Client | No statement anywhere | — |
| Outlook for Mac | No (*recalled*) | — |
| Canary Mail | No published claim; the security marketing is all PGP and transit | — |
| Delta Chat | Optional SQLCipher, **deprecated 2025-11**. Blobs never encrypted | passphrase from the UI |
| Proton Mail Bridge | **Yes**, message bodies only. Metadata DB is not | keychain → vault → per-user key |

Of ten clients, one encrypts message bodies at rest with a key rooted in the OS
keychain, and even it leaves the IMAP metadata database in the clear
(`internal/backend/backend.go` passes the passphrase to the store builder and
not to the database, *cited*). None derives a mail-store key from a user
password. Everyone else stores plaintext and delegates at-rest protection to
full-disk encryption, reserving the keychain for credentials.

Two findings are worth more than the table.

**Delta Chat built this and walked it back.** Their own header, `deltachat.h`
L285-289 (*cited*):

> Nonempty passphrase (db encryption) is deprecated 2025-11: — Db encryption
> does nothing with blobs, so fs/disk encryption is recommended. — Isolation
> from other apps is needed anyway.

Both halves apply to Mach exactly. The blobs are `attachments/` — 109 files on
disk today, indexed by Spotlight, and not covered by any scheme that encrypts
`mach.sqlite3`. And "isolation from other apps is needed anyway" is T1.

**Proton Bridge has a silent insecure fallback** (*cited*): if the vault key
cannot be loaded from the keychain, it logs, sets `insecure = true`, and
proceeds with `sha256(nil)` — a universal constant. To their credit it surfaces
in the API and the UI. The artifact on disk still looks exactly as encrypted as
the real thing. That is the failure mode any Mach implementation would have to
avoid, and it is the failure mode that is easy to ship.

---

## What I would do

1. **Turn on FileVault.** One setting, a password at boot, and T2 goes from open
   to closed. Nothing else on this page is close in value.

2. **Settle what Backblaze is doing.** It is billing for a backup that has not
   run since February, so there is no working backup of this machine, and also no
   cloud copy of the mail. Both of those are currently true by neglect. If the
   client gets repaired, either exclude
   `~/Library/Application Support/com.mach.mail` or set a Personal Encryption Key
   and accept the restore caveat.

3. **Delete the QA mailbox copy at
   `~/Projects/mach/.qa/agent/data/`.** 1.38 GB and 40 383 real messages, last
   touched three days ago, at the end of a group-traversable path rather than
   behind the `0700` on `~/Library/Application Support`. A QA instance can
   rebuild it whenever one is needed. Left in place here; nothing in this branch
   touches the owner's data.

4. **`0600` on the store.** Done in this branch: `Db::open` now takes the
   database, `-wal` and `-shm` down to `0600`, and a directory Mach itself
   creates for them to `0700`. A directory that already existed is left alone,
   because one caller's parent is the shared temp directory. This is what the
   codebase already does for the MCP and QA tokens; the store was the one
   sensitive file it did not cover. Against T1 it buys nothing. Against T6 it is
   the difference between a second local account being able to read 40 000
   messages out of the QA store and not.

Not recommended: SQLCipher, an encrypted volume for the store, or column-level
body encryption. The first two are defensible and both come second to FileVault;
the third should not be built at all while `messages_fts` exists.

Smaller items, in passing: three stale `agent/mcp-*.json` session tokens are
still in the store directory although `agent/mcp.rs` says the file "is deleted
when the session ends" — they are inert, because the listener dies with the
session, but the cleanup is not firing on every exit. `.env.local` is `0644` and
copied into 14 worktrees. `~/Library/Caches/mach` is 652 MB of mostly dev-server
module cache and can be deleted whenever.
