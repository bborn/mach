# Mach

A fast, keyboard-driven Gmail and Google Calendar desktop client for macOS, with
an agent that has real tools over your mail and calendar.

Tauri v2 shell, Rust core, React + TypeScript UI, SQLite + FTS5 store. It talks
to the Gmail and Google Calendar REST APIs directly over OAuth. Several accounts
are unified into one stream.

## What this is, and is not

This is one person's mail client, built for their own use and published because
someone else may find it useful. It is not a product, there is no support, and
there are no releases — you build it from source.

Deliberately **not** in scope, and not planned:

- **No IMAP or SMTP.** Gmail API only.
- **No other providers.** No Outlook, Fastmail, iCloud, or generic mail.
- **No mobile, no web, no Windows, no Linux.** macOS desktop only — refresh
  tokens live in the macOS Keychain and the UI is WKWebView.
- **No teams, sharing, or settings screens** for anything a constant expresses.

Design records, kept because they explain why the architecture is what it is:
[`docs/superpowers/specs/2026-08-07-mail-calendar-client-design.md`](docs/superpowers/specs/2026-08-07-mail-calendar-client-design.md)
and [`docs/calendar-ux-brief.md`](docs/calendar-ux-brief.md).

## State

Working and in daily use by its author: multi-account sync, unified thread list,
reading pane with sanitized HTML bodies, triage with undo, FTS5 search and a ⌘K
palette, composer with scheduled send and an undo window, a week/day/month
calendar with drag-to-create and drag-to-move, and an in-app agent.

Rough edges you should expect: no packaged build or auto-update, no first-run
onboarding beyond "add an account", and marketing-email HTML that renders
imperfectly. `cargo test` and `bunx vitest run` are the safety net; there is no
end-to-end suite.

One feature will not work for you out of the box: `⌘K → Send feedback` captures
the window, lets you annotate it, and files the result as a task for a coding
agent to pick up. It shells out to the author's own task runner — a `ty` binary
found on `PATH` or via `MACH_TY_BIN` — which you almost certainly do not have.
Without it the command reports that it could not file the task rather than
failing silently. The capture and annotation half works regardless.

## You must bring your own Google OAuth app

Mach ships **no credentials**. Every user registers their own Google Cloud
project and OAuth client, because this app is not submitted to Google for
verification and never will be. In practice that means:

- The first time you authorize each account, Google shows a **"Google hasn't
  verified this app"** interstitial. You click **Advanced → Go to Mach
  (unsafe)** and continue. It is a one-time cost per account, and it is not a
  failure.
- An unverified app has a **lifetime cap of 100 consenting users**. You are
  one of them, several times over if you add several accounts. Not a constraint
  for personal use.
- You must **publish the app to "In production"** even though it is unverified.
  In *Testing* status Google expires every refresh token after 7 days, which
  means re-authorizing every account every week. Publishing removes that.

The setup skill below encodes all of this. It is genuinely the fiddly part.

## Setup

### Prerequisites

- macOS (Apple silicon or Intel)
- [Rust](https://rustup.rs) stable, with the Xcode command line tools
- [Bun](https://bun.sh)
- A Google account you can create a Cloud project with
- Optional: an Anthropic API key, if you want the in-app agent

### 1. Google Cloud

**Hand this to your coding agent:** [`skills/mach-setup/SKILL.md`](skills/mach-setup/SKILL.md).
Point Claude Code (or any agent that can drive a browser) at it and it will
walk the Google Cloud console with you — creating the project, enabling the two
APIs, configuring the consent screen, creating a **Desktop app** OAuth client,
adding the six scopes, publishing to production, and writing `.env.local`. It
stops and hands the keyboard back for the parts that are yours: your password,
and accepting the User Data Policy.

If you would rather do it by hand, the skill file is also a precise click-by-click
checklist.

### 2. Credentials

The skill writes this for you; this is what it contains:

```sh
# .env.local — gitignored, never commit it
MACH_GOOGLE_CLIENT_ID=<id>.apps.googleusercontent.com
MACH_GOOGLE_CLIENT_SECRET=GOCSPX-<secret>

# optional, only for the in-app agent
ANTHROPIC_API_KEY=sk-ant-...
```

Missing credentials are a *state*, not a crash: the app boots, the window
paints, and the UI tells you it cannot add an account.

### 3. Build and run

```sh
bun install
bun run tauri dev
```

Then press `⌘K` → **Add account**, or use the account rail.

Prove the OAuth round trip on its own, before touching the UI:

```sh
cd src-tauri
set -a && source ../.env.local && set +a
cargo run --example oauth_smoke
```

It prints an authorization URL, waits for the loopback redirect, and reports
which account consented and whether a refresh token was issued.

### 4. Test

```sh
cd src-tauri && cargo test    # Rust core
bunx vitest run               # frontend
bunx tsc --noEmit             # types
bun run build                 # production frontend bundle
```

Both suites run offline against fakes. Nothing in them touches Google.

## Architecture

Two ideas carry the whole design.

**The UI never waits on Google.** Every pixel renders from local SQLite.
Opening a thread is a local read — sub-millisecond, works offline. The network
is a background sync loop writing into that database, and the UI reacts to the
changes. This is not an optimization added later; it is the reason the app is
worth building.

Sync is a 12-month backfill per account, then incremental `users.history.list`
from a stored watermark, with a full resync when Google expires the history ID.
Search is two tiers: FTS5 over the local window answers every keystroke in
under 10 ms, and `messages.list?q=` fires in parallel and merges in as "older
results" from the server about 150 ms later.

**Every action is a typed, undoable command — and that is also the agent's tool
surface.** Archive, label, snooze, send, create event, RSVP, move event: each is
a variant of one Rust `Command` enum. The UI dispatches commands and never calls
the Google APIs itself. Each command returns its own undo. The agent gets no
privileged path to Google — it calls the same commands the keyboard does, so
everything it does is undoable and shows up in the same log as manual actions.

```
┌─ React / TypeScript ─────────────────┐
│  mail mode · calendar mode · ⌘K      │   renders ONLY from local state
└──────────────┬───────────────────────┘
               │  typed commands
┌──────────────▼───────────────────────┐
│  Rust core                           │
│   command layer  (= the agent's tools)│
│   sync engine · search · MIME        │
│   tokens in the macOS Keychain       │
└──────┬────────────────────┬──────────┘
       │                    │
┌──────▼──────┐    ┌────────▼─────────┐
│ SQLite +FTS5│    │ Gmail API        │
│ local truth │◄───│ Calendar API     │
└─────────────┘    └──────────────────┘
```

Layout: `src-tauri/src/{auth,google,sync,db,commands,compose,render,agent,ipc}`
for the Rust core, `src/{components,lib,hooks}` for the UI. The `Command` union
in `src/lib/data.ts` mirrors the Rust enum in
`src-tauri/src/commands/types.rs`; Rust is authoritative.

## Scopes

Mach requests six, and no more:

| Scope | Tier | Why |
|---|---|---|
| `openid` | non-sensitive | identity |
| `userinfo.email` | non-sensitive | which account just authorized, so tokens key by email |
| `gmail.modify` | **restricted** | read, label, archive, trash — deliberately narrower than `mail.google.com`, which would also grant permanent delete |
| `gmail.send` | sensitive | the composer |
| `calendar` | sensitive | calendar list and settings |
| `calendar.events` | sensitive | event read and write |

## License

MIT. See [LICENSE](LICENSE).
