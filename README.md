<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo/mark-on-dark.png">
  <img src="assets/logo/mark-on-light.png" alt="Mach" width="64">
</picture>

# Mach

A fast, keyboard-driven Gmail and Google Calendar client for macOS, with an
agent that can act on your mail and calendar rather than just describe them.

Tauri v2 shell, Rust core, React + TypeScript UI, SQLite + FTS5 store. It talks
to the Gmail and Google Calendar REST APIs directly over OAuth, and puts all
your accounts in one list.

## What this is, and is not

This is my mail client. I built it for me and put it up here in case it's useful
to anybody else. It isn't a product, there's no support, and there are no
releases. You build it from source.

Not in scope, and not planned:

- **No IMAP or SMTP.** Mach uses the Gmail API.
- **No other providers.** No Outlook, Fastmail, iCloud, or generic mail.
- **No mobile, no web, no Windows, no Linux.** macOS desktop only. Refresh
  tokens live in the macOS Keychain and the UI is WKWebView.
- **No teams and no sharing.** One user, and it's me.

Design records, which explain why the architecture is what it is:
[`docs/superpowers/specs/2026-08-07-mail-calendar-client-design.md`](docs/superpowers/specs/2026-08-07-mail-calendar-client-design.md)
and [`docs/calendar-ux-brief.md`](docs/calendar-ux-brief.md).

## State

Working, and what I read my mail in every day:

- Multi-account sync into one unified thread list, with a reading pane over
  sanitized HTML bodies.
- Triage from the keyboard on Gmail's own bindings, with `⌘Z` over an undo stack
  that does not expire.
- Search in Gmail's operator syntax, parsed as you type and compiled to SQL
  against the FTS5 index, plus a `⌘K` palette for everything else.
- Composer with signatures, a send delay you can cancel, and scheduled send.
- Attachments: fetch, cache, open, save, and inline `cid:` images.
- Day, week and month calendar with drag-to-create and drag-to-move, and an
  event view carrying guests and their answers, recurrence, reminders and the
  Meet link.
- New-mail notifications and a Dock badge, under a narrow rule: unread, in the
  inbox, not from you, and either in Gmail's Personal category or continuing a
  thread you have written to.
- Preferences for theme, default account, per-account signatures, undo window,
  send delay, week start, working hours and sync interval.
- An in-app agent that runs on the Claude Code CLI by default and needs no API
  key. See [`docs/agent-backends.md`](docs/agent-backends.md).
- A sandboxed plugin runtime. See [`PLUGINS.md`](PLUGINS.md).

Expect rough edges: nothing packaged, nothing that updates itself, no onboarding
past "add an account", and marketing mail that renders like a ransom note more
often than I'd like. `cargo test` and `bunx vitest run` are the whole safety net
— there is no end-to-end suite.

One feature will not work for you out of the box. `⌘K → Send feedback` captures
the window, lets you annotate it, and files the result as a task for a coding
agent to pick up. It shells out to the author's own task runner, a `ty` binary
found on `PATH` or via `MACH_TY_BIN`, which you probably do not have. Without it
the command reports that it could not file the task. The capture and annotation
half works regardless.

## You must bring your own Google OAuth app

Mach ships **no credentials**. Every user registers their own Google Cloud
project and OAuth client, because this app is not submitted to Google for
verification and never will be. In practice that means:

- The first time you authorize each account, Google shows a **"Google hasn't
  verified this app"** interstitial. You click **Advanced → Go to Mach
  (unsafe)** and continue. This happens once per account.
- An unverified app has a **lifetime cap of 100 consenting users**. You are one
  of them, several times over if you add several accounts. It does not constrain
  personal use.
- You must **publish the app to "In production"** even though it is unverified.
  In *Testing* status Google expires every refresh token after 7 days, which
  means re-authorizing every account every week. Publishing removes that.

The setup skill below encodes all of this. It is the fiddly part of the install.

## Setup

### Prerequisites

- macOS (Apple silicon or Intel)
- [Rust](https://rustup.rs) stable, with the Xcode command line tools
- [Bun](https://bun.sh)
- A Google account you can create a Cloud project with
- For the in-app agent, either the [Claude Code](https://claude.com/claude-code)
  CLI or an Anthropic API key. Mach uses the CLI when it finds one.

### 1. Google Cloud

**Hand this to your coding agent:** [`skills/mach-setup/SKILL.md`](skills/mach-setup/SKILL.md).
Point Claude Code (or any agent that can drive a browser) at it and it will walk
the Google Cloud console with you: creating the project, enabling the two APIs,
configuring the consent screen, creating a **Desktop app** OAuth client, adding
the six scopes, publishing to production, and writing `.env.local`. It stops and
hands the keyboard back for the parts that are yours, which are your password
and accepting the User Data Policy.

If you would rather do it by hand, the skill file is also a precise click-by-click
checklist.

### 2. Credentials

The skill writes this for you; this is what it contains:

```sh
# .env.local — gitignored, never commit it
MACH_GOOGLE_CLIENT_ID=<id>.apps.googleusercontent.com
MACH_GOOGLE_CLIENT_SECRET=GOCSPX-<secret>

# optional. Only needed if you want the agent to use the Anthropic API
# instead of the Claude Code CLI.
ANTHROPIC_API_KEY=sk-ant-...
```

If the credentials are missing the app still boots and the window still paints.
The UI tells you it cannot add an account.

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

Two things shape the design.

**The UI never waits on Google.** Every view renders from local SQLite. Opening
a thread is a local read: under a millisecond, and it works offline. The network
is a background sync loop writing into that database, and the UI reacts to the
changes.

Sync is a 12-month backfill per account, then incremental `users.history.list`
from a stored watermark, with a full resync when Google expires the history ID.

Search is local. The query is parsed in TypeScript
(`src/lib/search-query.ts`) so the box can show its interpretation as you type,
and the AST crosses the IPC seam for Rust to compile against the FTS5 index
(`db::queries::compile_search`). Every user value is bound rather than
interpolated. Most queries answer in single-digit to low-tens of milliseconds
against a 61k-message store.

**Every action is a typed, undoable command, and the same commands are the
agent's tool surface.** Archive, label, snooze, send, create event, RSVP, move
event: each is a variant of one Rust `Command` enum. The UI dispatches commands
and never calls the Google APIs itself. Each command returns its own undo. The
agent gets no second path to Google. It calls the same commands the keyboard
does, so everything it does is undoable and shows up in the same log as manual
actions. `docs/agent-backends.md` covers which backend runs, what it is allowed
to do, and where approval is enforced.

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

Layout: `src-tauri/src/{auth,google,sync,db,commands,compose,render,agent,
attachments,notify,handoff,plugins,ipc}` for the Rust core,
`src/{components,lib,hooks}` for the UI. The `Command` union in
`src/lib/data.ts` mirrors the Rust enum in `src-tauri/src/commands/types.rs`;
Rust is authoritative.

## Scopes

Mach requests six scopes:

| Scope | Tier | Why |
|---|---|---|
| `openid` | non-sensitive | identity |
| `userinfo.email` | non-sensitive | which account just authorized, so tokens key by email |
| `gmail.modify` | **restricted** | read, label, archive, trash. Narrower than `mail.google.com`, which would also grant permanent delete |
| `gmail.send` | sensitive | the composer |
| `calendar` | sensitive | calendar list and settings |
| `calendar.events` | sensitive | event read and write |

## License

MIT. See [LICENSE](LICENSE).
