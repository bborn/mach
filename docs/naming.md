# Naming

Research record for renaming this project before it goes public. Dated 2026-08-08.

## How to read this

Everything under **Verified** was checked by hitting an API or fetching a page on
2026-08-08, and the method is named. Everything under **Not verified** is
inference or absence-of-evidence, and is labelled as such. No trademark register
(USPTO/TESS, EUIPO) was queried — every trademark statement below is an
observation about a live commercial site, not a register search. General web
search was unavailable for most of this work, so product-collision checks were
done by fetching candidate domains directly and reading what came back, which is
narrower than a search sweep.

Methods used:

| Check | Method |
|---|---|
| `.com` registration | Verisign RDAP (`rdap.verisign.com`), 200 = registered, 404 = free |
| `.app` / `.dev` / `.email` | Authoritative NS lookup via `dig @1.1.1.1`. NS present ⇒ registered. NS absent ⇒ almost certainly free, but delegation-less registrations would look the same |
| npm | `registry.npmjs.org/<name>` + package metadata |
| crates.io | `crates.io/api/v1/crates/<name>` + description and download count |
| Homebrew | `formulae.brew.sh/api/{formula,cask}/<name>.json` |
| GitHub | GitHub API: org/user existence, `bborn/<name>` existence, repo search by name sorted by stars |
| Existing product | Direct HTTPS fetch of the domain, reading `<title>` and OG tags |

---

## Verdict on "Mach": disqualified

Not a close call. Three independent reasons, any one of which would be enough.

**1. It is the name of the kernel this app runs on.** Mach is the XNU
microkernel underneath macOS. Every executable on the machine is a Mach-O
binary. The high-resolution timer is `mach_absolute_time()`. Mach ports, Mach
messages, and `mach_task_self()` are everyday vocabulary for anyone writing
native macOS code. This is a macOS-only app whose entire pitch is speed, which
means every search a user or contributor makes — "Mach performance macOS",
"Mach memory", "Mach timing" — lands on kernel documentation forever. The name
is unsearchable in precisely the territory the project occupies.

**2. The Rust ecosystem collision is concrete, not theoretical.**

- **Verified:** the crates.io crate `mach` is described as *"A Rust interface to
  the user-space API of the Mach 3.0 kernel that underlies OSX"* — 42,496,286
  downloads. Its successor `mach2` has 52,644,009. These are among the most
  depended-on crates in the macOS Rust world.
- **Verified:** this project's own Cargo package is literally `name = "mach"`
  (`src-tauri/Cargo.toml:2`), producing a `mach` binary and a `mach_lib`
  library. A Rust macOS project named `mach` sits directly on top of the
  best-known `mach` crate in Rust. Publishing is impossible and local
  conversation is ambiguous.

**3. The name is commercially crowded.**

- **Verified:** `mach.com` resolves and serves a site titled "MACH".
  `mach.app` resolves and serves a site titled "MACH!". Neither was
  investigated further.
- **Not verified (recall, not checked):** Gillette's Mach3 razor line is a
  long-standing consumer trademark, and MACH Alliance is an established
  e-commerce architecture consortium. Treat as background, not as a finding.

**Clean namespaces for "mach" anyway, for the record:** Homebrew formula and
cask are both free (404). This does not rescue the name.

---

## Verdict on "Machmail": also disqualified

The compound genuinely fixes the *kernel* problem. `machmail` is not a kernel
symbol, is highly searchable, and the developer namespaces are the cleanest of
anything in this document:

**Verified free:** npm `machmail` (404) · crates.io `machmail` (404) · Homebrew
formula and cask (404/404) · GitHub org `machmail` (404) · `bborn/machmail`
(404) · `machmail.email` (no NS). GitHub has exactly one repo named `machmail`:
`predicate-logic/machmail`, 0 stars, "Python attachment client for Machinima" —
negligible.

That is a better slate than Spool, Telex, or any other finalist below. If the
only question were namespace hygiene, Machmail would win.

**But `machmail.app` is a waitlist for this exact product.**

**Correction to an earlier draft of this document:** that draft called
`machmail.app` "a live email client." That was an inference from OG metadata
presented as a verified fact — precisely the error this document's own
methodology note warns against. Correcting it by actually fetching and reading
the JavaScript bundle:

**Verified** by fetching `https://machmail.app` and its
`/assets/index-C9AZUwUh.js` (487 KB) on 2026-08-08.

Page metadata: `og:title` "Mach Mail", `og:description` "The email client for
people who take their time seriously." HTTP 200 from Google App Engine,
`Last-Modified: Thu, 19 Mar 2026`. The OG image is hosted on
`mach-mail-rspyr.replit.app` and `twitter:site` is the unmodified Replit
template default, `@replit`.

Bundle string counts: `waitlist` ×6, `Waitlist` ×2, `Early Access`,
`coming soon`, `Pricing` ×2. The marketing copy includes *"Join thousands of
professionals taking back control of their time with Mach Mail"* — language
nobody writes once they actually have users.

**So it is vapor.** A Replit-built waitlist page with no shipped product, no
commercial standing, and no trademark rights. That is the correct read and it
should not be overstated again.

The problem is what the waitlist is *for*. Marketing strings extracted verbatim
from that bundle:

- *"Fully local architecture ensures every action happens instantly. No loading spinners. Ever."*
- *"Never touch the mouse. Command palette and shortcuts for everything."*
- *"Manage your events right inside your email client. No more switching between apps."*
- *"Only see what matters. Automated triage that learns from your behavior."*
- *"OAuth token storage in macOS Keychain."*
- *"Available for macOS. iOS coming soon."*
- *"Local AI mode processes data on-device."*

Plus `googleapis` ×8, `gmail.` ×6, `OAuth` ×4, and a privacy policy citing the
Google API Services User Data Policy and the Gemini API.

That is this project's design spec restated as a landing page: local-first, no
spinners, keyboard-only, command palette, macOS, Gmail over OAuth, tokens in the
Keychain, calendar folded into the mail client, AI triage.

**Why "it's vapor" does not settle the question.** A dead shipped product fades
from search. A waitlist page with a pricing section and a privacy policy is
someone actively trying to launch — and it holds `machmail.app`, the canonical
TLD for a Mac app, while this project would hold `.dev`. If a genuinely working,
genuinely fast, open-source macOS Gmail client ships as Machmail, the confusion
runs in the direction that costs this project: readers land on a stranger's
waitlist and take it for the official site; or they take this repo for the free
tier of someone's forthcoming paid product; or they take its author for the
person harvesting those email addresses. There is no recourse, because they
registered first and hold the better domain.

Secondary finding, **verified** by WHOIS: `machmail.com` was created 2002-04-06
and its Registrant Organization is **Swishmail (Websak, LLC)** — an email
hosting company. It serves no site, but a mail company has held the `.com` for
24 years. Between the two, both canonical domains for this name are permanently
out of reach.

**Conclusion:** Machmail is *usable*. Unlike bare Mach it is not disqualified —
the compound genuinely solves the kernel collision, and the package namespaces
are the cleanest in this document. But the name can never own its own `.com` or
`.app`, and the `.app` is a waitlist for the identical product.

**The real argument is that the decision cost is asymmetric in time.** Today the
repo is private, nothing is published to any registry, and there are no stars,
forks, or inbound links; switching costs an afternoon. Six months after going
public it costs redirects, dead links, a stale name on aggregators, and confused
users. So the question is not "is Machmail bad enough to abandon" — it is "am I
confident enough that it is fine to lock in permanently." That is a judgement
call, not a research finding, and it belongs to the author.

*(Worth ruling out first: confirm `machmail.app` is not yours. Given the March
2026 timestamp and the untouched Replit scaffolding, it almost certainly is not.)*

---

## Also rejected, for cause

Three candidates that looked strong and died on inspection. Recorded so they are
not re-proposed.

- **Posthaste** — perfect meaning (from the postal endorsement "haste, post,
  haste"), and the namespaces were nearly clean: crates.io free, `posthaste.dev`
  free, no GitHub repo above 23 stars. **Killed by verification:**
  `https://posthaste.app` returns HTTP 200 with `<title>Posthaste | Email, AI,
  and Notifications</title>`. An existing email product.
- **Nib** — the single best availability profile found anywhere: `.app`, `.dev`
  and `.email` all free (verified, no NS records), three letters, Homebrew clear.
  **Killed on principle:** `.nib` is the macOS Interface Builder file format,
  present in every Mac app bundle. That is the identical mistake as Mach — a
  macOS system component — so it fails for the reason we are renaming.
- **Apace** — means "swiftly", crates.io free. **Killed:** `apace.app` serves the
  "APACE Congress" app, and for a developer audience the name is a persistent
  misread of "Apache".

---

## Shortlist

All five were checked with the methods in the table above. **Every short English
word has all its major TLDs registered** — this turned out to be a
non-differentiator, so the ranking weights product collisions, system-component
collisions, and CLI/package namespaces instead.

### 1. Spool — recommended

**What it means.** `/var/spool/mail` is where mail lives on disk before anything
reads it. A spool is a local store that a background process drains into. That is
not a metaphor for this architecture, it is a literal description of it: the UI
renders only from local SQLite, and the network is a sync loop writing into that
store. "Spooling" also carries the sense of feeding continuously and fast. It
says *local-first* and *mail* at once, in one syllable, with no decoration.

**Verified.**

| Check | Result |
|---|---|
| `spool.dev` | **Free** (no NS records) |
| `spool.email` | **Free** (no NS records) |
| `spool.com` | Registered — serves "Merrimac Spool & Reel \| Custom Spool Solutions" (industrial reels; unrelated class) |
| `spool.app` | Registered — domain broker "offbyone names", listed at **$6,900**. Do not engage; `.dev` is free |
| Homebrew formula | **Free** (404) |
| Homebrew cask | **Free** (404) |
| crates.io | Taken — "Git-native, event-sourced task management", **372 downloads** total. Effectively dormant |
| npm | Taken — "Thread arguments through pure functions", v3.0.0, updated 2026-07-25. A trivial utility, actively published |
| GitHub org `spool` | Taken |
| `bborn/spool` | **Free** (404) |
| GitHub repos named spool | No exact-name repo of size. Top hits are `Donkie/Spoolman` (2.7k, 3D-printer filament), `SpoolSample`/`SpoolFool` (Windows Print Spooler CVEs), `paperboytm/spool` (588, self-hosted archive for AI coding sessions) |

**Honest downside.** Two search-noise sources: 3D-printer filament spools
dominate consumer results, and "spool" in a security context means the Windows
Print Spooler exploit family. `paperboytm/spool` (588 stars) is a live dev tool
using the exact name, though in a different category. And the word is slightly
passive — it describes the plumbing, not the speed, which undersells the pitch to
anyone who does not already know what a mail spool is. Its virtue is that it is
true and unpretentious; it is not a name that shouts.

### 2. Telex — runner-up

**What it means.** The telex network (TELegraph EXchange) was how the world sent
text fast before fax and email. It says *message* and *speed* in one five-letter,
two-syllable word — the best pure meaning-fit of anything considered, and the
only candidate that captures both halves of the pitch.

**Verified.** `bborn/telex` free · Homebrew formula and cask both free ·
crates.io taken but by a stub: "Rust-native RPC where traits are the schema (not
yet released)", v0.0.0, **26 downloads** · npm taken by a dead 2022 package
(v0.0.1) · `.com`, `.app`, `.dev`, `.email` all registered · top GitHub repo is
`ewust/telex` at 142 stars.

**Honest downside, and why it is not first.** `https://telex.email` returns 200
and is a **live disposable-email service** (aliased to short-mail.de). That is
small and low-prestige, but it is an email product operating under the exact
name — the same failure class that killed Posthaste and clouded Machmail, differing only
in degree. On top of that, `telex.com` serves **Telex — Radio Dispatch &
Aviation Solutions**, a real commercial operation in comms hardware (*not
verified against a trademark register*), and `telex.hu` is a major Hungarian news
outlet that will own the search results. Separately, Telex is the standard
Vietnamese keyboard input method — an odd shadow for a keyboard-first app. Great
name, too much freight.

### 3. Steno

**What it means.** Stenography is writing at the speed of speech, on a chorded
keyboard. It is the only candidate that says *keyboard* and *speed* together,
and "taking dictation" is a quietly apt image for the built-in agent.

**Verified.** Homebrew formula and cask both free · `.dev` registered, `.com` and
`.app` registered · npm taken (typicode's small file writer) · crates.io taken by
a substantial library — "distributed saga implementation", **481,849 downloads**.

**Honest downside.** That crates.io occupant is a real, widely-used
infrastructure crate, not a squat — a live collision in exactly this project's
language. And "steno" evokes a 1950s secretarial pool; it is a clipped
abbreviation rather than a word, which makes it feel dated rather than spare.

### 4. Ramjet

**What it means.** The engine that only works above about Mach 3 — a way to keep
the original idea while dropping the broken word.

**Verified.** `ramjet.email` free · Homebrew formula and cask both free ·
`bborn/ramjet` free · crates.io taken by a 64-download crate ("Thread-per-core,
completion-based networking runtime") · npm taken · `.com`/`.app`/`.dev`
registered.

**Honest downside.** Says nothing about mail, calendar, or local-first — only
speed, and in an aggressive mechanical register that reads more like a game or a
GPU than a mail client. Seven letters and a hard consonant cluster.

### 5. Whippet

**What it means.** The fastest-accelerating dog. Pure speed, light on its feet.

**Verified.** crates.io **free** · Homebrew formula and cask both free ·
`whippet.email` free · `bborn/whippet` free · npm taken by a dead 2022 static
site generator · `.com`, `.app`, `.dev` all registered but neither `.com` nor
`.app` returned a response.

**Honest downside.** "Whippet" is also slang for a nitrous-oxide canister — an
unforced association for a public project. `wingo/whippet` (239 stars) is Andy
Wingo's garbage collector, which puts it in adjacent systems-programming
conversation. And a dog name says nothing about mail or calendar.

---

## Recommendation

**Spool.** Runner-up: **Telex**.

Spool wins on the two criteria that actually separated the field. First, no
same-category occupant: nothing named Spool is a mail, calendar, or productivity
product — the industrial-reel company on the `.com` is a different class
entirely, and this is the failure that eliminated Posthaste, demoted Telex, and
leaves Machmail permanently locked out of its own `.com` and `.app`. Second,
Spool can actually hold its own canonical domain. Third, the shipping
namespaces are open where it counts: both
Homebrew namespaces free, `bborn/spool` free, `spool.dev` and `spool.email` free
and registerable today for normal prices. The crates.io and npm occupants are a
372-download hobby crate and a one-function utility, and neither matters for an
app that is built from source and published to neither registry.

It also survives the trap list cleanly. It is not a macOS system component — the
mistake being corrected. It is not so common a word as to be ungoogleable
("spool mail client" is a fine query). And its `.com` is a real business rather
than a broker holding it hostage, so there is no $6,900 conversation — unlike
`spool.app`, which should simply be ignored in favour of `.dev`.

And it is the only candidate whose meaning is a plain description of the
architecture rather than an aspiration about it. `/var/spool/mail` is what this
app *is*: a local store that the network drains into, that the UI reads from and
never waits on. For an audience that will read the README before they read the
landing page, that is worth more than a name that merely gestures at speed.

Take Telex instead if the disposable-email site reads as beneath notice and the
romance of the name is worth the search noise. It has the better story; it has
the worse neighbourhood.

---

## What the rename costs

### Scale

**Verified** by grep across tracked source, excluding `node_modules/`, `target/`,
`dist/`, `.git/`, and both lockfiles, and filtering out the word "machine":

- **547 occurrences across 97 files.**
- Two paths need renaming: `skills/mach-setup/` and `src/hooks/useMach.tsx`.
  (Everything else matching `*mach*` on disk is build output under
  `src-tauri/target/`.)

Highest-density files: `src-tauri/tests/render.rs` (42), `skills/mach-setup/SKILL.md`
(26), `src-tauri/tests/ipc.rs` (24), `src-tauri/tests/agent.rs` (18),
`src-tauri/src/ipc/feedback.rs` (17), `src/lib/data.ts` (15),
`src-tauri/src/commands/mail.rs` (13), `src/hooks/useMach.tsx` (12),
`src/lib/message-body.ts` (12), `src/lib/ipc.ts` (12).

Most of this is mechanical: prose in doc comments, test fixture strings, and the
`MACH_*` environment prefix. The load-bearing declarations are few:

| File | Line | Value |
|---|---|---|
| `src-tauri/tauri.conf.json` | 3 | `"productName": "Mach"` |
| `src-tauri/tauri.conf.json` | 5 | `"identifier": "com.mach.mail"` |
| `src-tauri/tauri.conf.json` | 15 | `"title": "Mach"` |
| `src-tauri/Cargo.toml` | 2 | `name = "mach"` |
| `src-tauri/Cargo.toml` | 15 | `name = "mach_lib"` |
| `package.json` | 2, 6 | `"name": "mach"`, repo URL `bborn/mach` |
| `index.html` | 7 | `<title>Mach</title>` |
| `src-tauri/src/config.rs` | 32 | `DATABASE_FILE_NAME = "mach.sqlite3"` |
| `src-tauri/src/auth/tokens.rs` | 28 | `KEYCHAIN_SERVICE = "com.mach.mail.oauth"` |
| `src-tauri/src/auth/tokens.rs` | 31 | `QA_KEYCHAIN_PREFIX = "com.mach.mail.oauth.qa-"` |

Environment variables in use, all of which would want a new prefix:
`MACH_GOOGLE_CLIENT_ID` (19 references), `MACH_GOOGLE_CLIENT_SECRET` (10),
`MACH_TY_BIN` (6), `MACH_AGENT_EFFORT` (3), `MACH_AGENT_BASE_URL`,
`MACH_AGENT_MAX_TOKENS`, `MACH_AGENT_MODEL`, `MACH_DATA_DIR`, `MACH_REPO_ROOT`
(2 each), `MACH_CDP_PORT`, `MACH_FEEDBACK_DIR`, `MACH_VIEWPORT`, `MACH_WEB_URL`.

### The bundle identifier orphans the database

**This is the one that bites.** Tauri derives the app-data directory from
`identifier` in `tauri.conf.json`. Changing the identifier silently changes where
the app looks for its database, and it will boot into an empty one rather than
error.

**Verified on this machine:**

```
2.2G  ~/Library/Application Support/com.mach.mail/
        mach.sqlite3       2,302,218,240 bytes
        mach.sqlite3-shm
        mach.sqlite3-wal
```

Also present, both disposable caches: `~/Library/Caches/mach` and
`~/Library/WebKit/mach`.

The fix is a `mv`, done while the app is not running so the WAL is checkpointed
cleanly — and the WAL and SHM files must move with the database:

```sh
mv ~/Library/Application\ Support/com.mach.mail \
   ~/Library/Application\ Support/com.spool.mail
cd ~/Library/Application\ Support/com.spool.mail
for f in mach.sqlite3*; do mv "$f" "spool${f#mach}"; done   # only if DATABASE_FILE_NAME changes
```

Skipping the `mv` costs a full 12-month backfill across every account, which is
the expensive failure, not a data loss. `MACH_DATA_DIR` (renamed) remains an
escape hatch for pointing at the old directory instead.

### The keychain service orphans the OAuth tokens

Less obvious and sharper, because there is no `mv` equivalent. `KEYCHAIN_SERVICE`
is `"com.mach.mail.oauth"` and every account's refresh token is a keychain entry
under it. Change that constant and every account must be re-authorized — which
means walking the **"Google hasn't verified this app" → Advanced → Go to …
(unsafe)** interstitial once per account again.

`QA_KEYCHAIN_PREFIX` is the same string with `.qa-` and the instance name
appended, and it is what every non-`main` instance uses. It holds nothing the
owner would miss, so a rename can leave it wherever it lands.

Three options, cheapest first:

1. **Leave `KEYCHAIN_SERVICE` alone.** It is an opaque internal string; nothing
   about the rename requires touching it. Zero cost, slight inconsistency.
2. Copy the entries to the new service name with `security` before switching.
3. Just re-authorize. Tolerable for a personal install, tedious across five
   accounts.

### Outside the repo

- **GitHub:** rename `bborn/mach` → `bborn/<name>`. GitHub redirects the old URL,
  so nothing breaks. Do it *before* going public so the old name never enters
  anyone's history.
- **Google Cloud OAuth consent screen:** the app name shown on the consent
  interstitial is configured in the console, not in code. Cosmetic, but it is the
  string users read while deciding to trust the app — worth updating, and worth a
  line in `skills/mach-setup/SKILL.md`.
- `docs/superpowers/specs/2026-08-07-mail-calendar-client-design.md` is marked a
  historical record and already says "Working name: Mach (rename freely)". Leave
  its prose alone; add a one-line note that the name resolved to `<name>`.
