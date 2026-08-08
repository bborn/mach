# Mach — a fast Google-only mail and calendar client

**Date:** 2026-08-07
**Status:** Approved design, pre-implementation
**Working name:** Mach (rename freely)

> Kept as a historical record. It is the argument behind the architecture — why
> local-first, why a command layer, why Google-only — and the measurements that
> settled the open questions. It is not maintained as documentation; where it
> and the code disagree, the code is right.

## What this is

A desktop mail and calendar client for one person, talking directly to
the Gmail and Google Calendar REST APIs over OAuth. Five Google accounts,
unified. No IMAP, no SMTP, no other providers, no mobile, no teams.

It replaces Gmail and Spark entirely. Success means the browser tab stays
closed.

## Why build rather than buy

Shortwave and Superhuman already solve most of this, and either would be
cheaper than building. The one thing neither sells is an agent with real tools
over your own mail and calendar, wired into your own stack — one you can hand
new tools, point at other systems, and run headless. That is the reason this
exists.

The agent is not designed in this spec. The architecture is built so it can be
added without rework (see "The command layer").

Open source was evaluated and rejected: Mail-0/Zero (10.8k stars) has no commit
since August 2025 and is a Cloudflare Workers web app; Velo (676 stars) is
abandoned since March 2026 and its Rust core is `async-imap` + `lettre` — the
IMAP architecture this design rejects; Mailspring is Electron on a 2017
codebase.

## Stack

| Layer | Choice |
|---|---|
| Shell | Tauri v2 (WKWebView, not Chromium) |
| Core | Rust |
| UI | React + TypeScript |
| Components | shadcn/ui on Base UI, Tailwind v4 |
| Store | SQLite + FTS5 |
| APIs | Gmail API, Google Calendar API |

Swift/SwiftUI was seriously considered — Mimestream proves it is the fastest
path for this exact app. Rejected because there is no component foundation for
AppKit, SwiftUI fights dense keyboard-driven list UI, a webview would be
embedded for message bodies and the composer anyway, and there is no existing
Swift code in this workspace to build on.

## Architecture

```
┌─ React / TypeScript ─────────────────┐
│  mail mode · calendar mode · ⌘K      │   renders ONLY from local state
└──────────────┬───────────────────────┘
               │  typed commands
┌──────────────▼───────────────────────┐
│  Rust core                           │
│   command layer                      │
│   sync engine · search · MIME        │
│   tokens in macOS Keychain           │
└──────┬────────────────────┬──────────┘
       │                    │
┌──────▼──────┐    ┌────────▼─────────┐
│ SQLite +FTS5│    │ Gmail API        │
│ local truth │◄───│ Calendar API     │
└─────────────┘    └──────────────────┘
```

### The speed thesis

The UI never waits on Google. Every pixel renders from local SQLite. The
network is a background sync loop writing into that database; the UI reacts to
changes. Opening a thread is a local read — sub-millisecond, works offline.

This is the whole design. It is not an optimization added later.

### The command layer

Every action — archive, label, snooze, send, create event, RSVP — is a typed
command in Rust. The UI dispatches commands; it never calls the APIs directly.
Commands are individually undoable and log their effects.

This is the agent seam. When the agent is designed, its tools are these same
commands — already built, already tested, already undoable. No hooks are added
now; the only rule is that action logic never leaks into React.

### Data model

One SQLite database. Core tables carry an `account_id`:

- `accounts` — id, email, oauth token ref, `history_id` watermark, colour
- `threads` — account_id, gmail thread id, participants, subject, snippet, ts
- `messages` — thread_id, headers, body_html, body_text, labels
- `labels` — account_id, gmail label id, name, type
- `attachments` — message_id, filename, mime, size, local path if fetched
- `events` — account_id, calendar_id, start, end, attendees, rrule source
- `messages_fts` — FTS5 index over subject + body_text

The unified stream is one indexed query across accounts ordered by timestamp.
Per-account colour bar on each row. Replies infer the from-address from the
account the thread arrived on.

### Sync

Gmail: initial backfill of the last 12 months per account, then incremental
`users.history.list` from the stored `historyId` (2 quota units per call versus
5 for `messages.list`). On HTTP 404 the history ID has expired — fall back to a
full resync. Each account syncs independently with its own watermark. Older mail
is reachable through server-side search and pulled into the local store on
demand.

Calendar: `events.list` with `singleEvents=true`, which expands recurring
events into concrete instances for the requested window. Incremental via
`syncToken`.

### Search

Two tiers, because neither one alone is good enough.

**Gmail's server search is genuinely good** — `messages.list?q=` accepts the full
Gmail query syntax and hits Google's own index across the entire mailbox.
Measured against a 548k-message account: **136–230 ms** per query. Fast for
a network call.

**But it is not instant, and it returns only message ids.** Hydrating a result
costs another ~138 ms each, so a naive 25-result page is 3.5 seconds. That, plus
the fact that it does not work offline and you feel 150 ms on every keystroke,
is why local search has to carry the common case.

So:

| Tier | Source | Latency | Covers |
|---|---|---|---|
| 1 | SQLite FTS5 over synced bodies | <10 ms | the local window, offline |
| 2 | `messages.list?q=` | ~150 ms | the entire mailbox |

Every keystroke queries tier 1 and renders immediately. Tier 2 fires in parallel
and merges in as "older results" when it lands, marked as coming from the
server. Only the visible rows are hydrated — roughly ten, concurrently, ~300 ms
— and the rest lazily on scroll. Never 25 up front.

The result: instant answers always, with the long tail arriving a beat later
without the user asking twice.

#### Why the 12-month window is the right size (measured, 2026-08-07)

| Window | Messages in the account this was measured on |
|---|---|
| 1 month | 3,228 |
| 3 months | 9,701 |
| 12 months | ~38,000 |
| everything | 547,968 |

Twelve months is about **7%** of that mailbox. At Gmail's ceiling of 250 quota
units/sec — `messages.get` costs 5, so 50/sec — that is:

| Strategy | Backfill (at the ceiling) | Storage |
|---|---|---|
| 12 months | ~13 min | ~1 GB |
| everything | ~3 hours | ~14 GB |

**Measured, not achieved.** The first real backfill ran at **11 msg/sec**, not
the 50/sec the quota allows — roughly 75% of the budget unused, making 12
months ~55 minutes rather than ~13. The limit is the engine's concurrency, not
Google's. Tracked as work, not accepted as the number.

Per account, and the five run concurrently because rate limits are per user. So
~13 minutes once, ever; after that `history.list` deltas at 2 units a call.

Nothing older is lost — it is one tier-2 query away, at 150 ms.

## Interface

Two modes in one window, one keystroke apart. Both stay mounted; switching is a
CSS swap, never a load.

### Mail mode

Unified stream across all accounts. Three panes: account/label rail, thread
list, reading pane. Per-account colour bar on the left edge of each row.
Density target is Linear-like — 16+ rows visible where Spark shows 9.

Message bodies render in a sandboxed iframe: HTML sanitized, remote images
blocked until allowed, quoted history detected and collapsed.

### Calendar mode

Full-width week grid as the default view. Drag to create, drag to move,
overlapping events laid out side by side rather than stacked. Day and month
views on `1`/`2`/`3`. All accounts' calendars overlaid with per-calendar colour,
individually toggleable.

Mail/calendar links:
- An invite in a thread shows real availability inline; RSVP without leaving.
- A date mentioned in a message is a jump target.
- Creating an event from a thread carries subject, attendees and a thread link.

### ⌘K

One box, resolved in layers. Only the last layer touches the network.

| Input | Behaviour |
|---|---|
| `tawny` | instant local ranked results — threads, events, people, commands |
| `from:tawny has:attachment` | operator query with live value autocomplete |
| `>` | commands — archive all, add account, settings |
| `⇥` on a sentence | hands the query to the agent |

Ordinary typing is always instant and local. Natural language works but costs a
model round-trip, so it is an explicit keystroke, never triggered by a pause.
Typing a sentence shows local matches immediately with `⇥ ask` available.

### Agent sessions

`⌘K` from anywhere must let you talk to the agent about **whatever you are
looking at**, without saying what that is. "reply to this next tues" has to
work: `this` is the open thread, `next tues` resolves against the real calendar,
and the result is a scheduled reply.

**Context is implicit but visible.** Whatever is selected — thread, message,
event, day, search results — is attached automatically, and the session says so
in a line the user can read and remove: `Re: Series A data room · added to
context`. Implicit attachment is what makes it feel like talking about the
screen; showing it is what stops the agent silently working from the wrong
thing.

**Sessions are concurrent and named.** Asking is not modal — the agent works
while you keep reading mail. Running sessions live as pills in a bottom bar,
each titled by its task ("Reply to Tawny next Tuesday"), each expanding into a
drawer with the conversation, minimizable and closable. Reference: Linear's
agent sessions, which get this shape right.

**Its tools are the command layer.** "Reply next Tuesday" is a compose command
plus a schedule-send command — the same typed, undoable commands the UI
dispatches. The agent gets no privileged path to Google, so anything it does is
undoable and appears in the same log as manual actions.

**This is the same machinery as the feedback loop.** A feedback session is an
agent session whose context is an annotated screenshot and whose tools edit the
repo instead of the mailbox. One session model, one bottom bar, two tool sets —
which is why neither is special-cased.

### Composer

Inline, growing at the bottom of the thread being answered, so the message
being replied to stays visible. No modal, no separate window. Markdown-ish
shortcuts, no toolbar. `⌘⏎` sends, `⌃S` schedules.

Sending is optimistic: local write first, then a 10-second undo window during
which the message sits in a local outbox, then the API call. Undo cancels
before anything leaves.

### The feedback loop

Once the app is in daily use, this becomes the way it improves — and it is a
product feature, not tooling. The target is **minutes from noticing something to
seeing it fixed in the running app**, without leaving the app.

`⌘K` → `feedback` (or a dedicated key) opens a capture surface:

1. Screenshot the current window automatically — the context is almost always
   "this thing, right here".
2. Let the user scribble on it. Arrows and circles carry intent that prose
   cannot: "*this* one bigger" is a gesture, not a sentence.
3. Take a line of text.
4. File it as a TaskYou task and queue it for immediate execution:
   `ty create "<title>" --body "<text + annotated screenshot path>"
   --project mach --execute`

An agent picks it up in the `mach` project, works in a git worktree, and
implements it.

**Why the round trip is actually fast.** The app runs in dev mode, so Vite HMR
applies frontend edits to the running window with no restart and no lost state
— which covers nearly every "move this, make that bigger, change this colour"
request. Rust changes need a recompile and a relaunch, so those are minutes
rather than seconds. Worth knowing which kind of request you just made.

**Why it belongs in the command layer.** Feedback is just another typed command.
That means the same catalogue the UI dispatches and the agent calls also carries
"file feedback" — the loop that improves the app is built from the same parts as
the app.

Requires a checkout of this repo and a TaskYou project named `mach`. The `ty`
binary is looked up on `PATH` or via `MACH_TY_BIN`; without it the feedback
command reports that it cannot file the task rather than failing silently.

### Keyboard

Superhuman's map, since that muscle memory may already exist: `e` archive,
`j`/`k` move, `r` reply, `h` snooze, `⌘⏎` send.

## Hard parts

Only two survive scrutiny. Everything else is CRUD against SQLite and two
documented JSON APIs.

1. **Rendering hostile HTML.** The API returns raw message bodies written by
   strangers. Google does not expose Gmail's sanitizer. A sandboxed iframe plus
   a sanitizer covers most of it quickly; the long tail is marketing-email
   polish. Unavoidable on any stack.

2. **OAuth on the personal account.** ~~Feared blocker~~ — **resolved
   2026-08-07 by running the spike.** See "Phase 0 result" below.

## Phase 0 result (2026-08-07)

Run against a real project rather than reasoned about. Outcome: **the feared
7-day token expiry is avoided.**

| Item | Value |
|---|---|
| GCP project | a personal project, **no organization** (the simplest path) |
| APIs enabled | Gmail API, Google Calendar API |
| OAuth client | Desktop app — the only type Google allows a loopback redirect on an arbitrary port |
| Publishing status | **In production** |
| User type | External (Internal unavailable — no org) |

**Scope tiers, as Google actually classifies them** (this refines the earlier
guess that both Gmail scopes were restricted):

| Tier | Scopes |
|---|---|
| Non-sensitive | `openid`, `userinfo.email` |
| Sensitive | `calendar`, `calendar.events`, **`gmail.send`** |
| Restricted | **`gmail.modify`** only |

**What was proven:** Google permitted the app to move to *In production* with a
restricted scope attached. It warns that verification is required and offers a
Verification Center, but does not block publishing, and does not revert.

**What is inferred, not yet proven:** that refresh tokens are now long-lived.
Google documents the 7-day expiry as a property of *Testing* status, which no
longer applies — but confirming it empirically needs a real authorization plus
seven days of waiting. Treat as very likely, not certain.

`TokenManager::refresh` already surfaces an expired grant as a typed error
rather than silently dropping the credential, so if the inference is wrong the
app prompts for re-authorization instead of failing mysteriously.

**Consequences to design around:**

- The app is unverified, so first authorization of each account shows Google's
  "hasn't verified this app" interstitial. Clicking through Advanced is a
  one-time cost per account. Onboarding copy should say so plainly rather than
  letting it look like a failure.
- A lifetime cap of 100 consenting users applies while unverified. Five
  accounts against a cap of 100 is not a constraint.
- CASA Tier 2 assessment ($500–$4,500/yr) remains the escape hatch only if
  Google later enforces verification. Not needed now.

## Build order

Each phase is usable on its own.

| Phase | Deliverable |
|---|---|
| 0 | DONE 2026-08-07. Published to production; 7-day expiry avoided. |
| 1 | One account: sync, list, read. Proves the speed thesis. Dogfoodable as a reader. |
| 2 | All accounts, unified stream, triage actions with undo. Replaces Spark for reading and triage. |
| 3 | FTS5 search and ⌘K. |
| 4 | Composer and send. **The switch point** — Gmail stops being opened. |
| 5 | Calendar mode, week grid, invites. |
| 6 | In-app feedback loop: annotated screenshot -> TaskYou task -> agent. |
| 7 | Agent. Separate spec, built on the command layer. |

Phases 1–3 produce a better Spark. Phase 4 is where the switch actually
happens; stalling before it leaves a nice reader that still sends from Gmail.

## Explicitly not building

IMAP, SMTP, non-Google providers, mobile, sharing, team features, themes, and
any settings screen for something a constant can express.

**Mobile is a settled non-requirement.** Gmail for iOS already covers the phone
and is fine. This removed the main argument for a TUI (running over mosh from a
phone), and with it the case for anything other than a desktop GUI. Do not
reopen.

## Rejected alternatives

| Option | Why not |
|---|---|
| Buy Shortwave / Superhuman | Closest fit and cheaper, but neither exposes an agent you can give your own tools to. That is the reason to build. |
| Native Swift / SwiftUI | Fastest possible (Mimestream proves it), but no component foundation for AppKit, SwiftUI fights dense keyboard list UI, a webview gets embedded for bodies and composer anyway, and there is no existing Swift code here. |
| TUI (Ratatui) | Better for the mail half, but no HTML layout and no drag-capable week grid — and calendaring is a headline requirement. Its best argument was phone access over mosh, which Gmail iOS already covers. |
| Ghostty / WezTerm + tmux | Kitty graphics gets images back, but terminals have no layout engine so HTML still fails, and tmux breaks graphics passthrough — the images and the persistence cancel each other out. |
| Fork Mail-0/Zero, Velo, Mailspring | All dead or wrong architecture. Zero: no commits since Aug 2025, is a Workers web app. Velo: abandoned Mar 2026, IMAP core. Mailspring: Electron on a 2017 codebase. |
