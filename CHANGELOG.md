# Changelog

Newest first. The format is [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the versions are [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

`Unreleased` is what is on `main` and not in a release yet.

## [Unreleased]

### Added

- Multi-account Gmail sync into one thread list, with a reading pane over
  sanitized HTML bodies. Twelve-month backfill per account, then incremental
  `users.history.list` from a stored watermark.
- Gmail's own keyboard bindings, checked against `support.google.com` rather
  than recalled. `?` lists every binding live at the moment it was pressed,
  filterable, from anywhere rather than only the calendar.
- Multi-select: shift-click for a range, ⌘-click to toggle, `x` to tick,
  ⇧J/⇧K to extend while moving, ⌘A for everything loaded — and it says so when
  pages remain unfetched instead of implying it selected 41,746.
- Search in Gmail's operator syntax, parsed in TypeScript so the box can show
  its interpretation as you type, then compiled to SQL against the FTS5 index.
- A ⌘K palette for everything that has no key.
- A composer on Squire: signatures, several drafts as a tab strip, attachments
  in and out, inline images, a send delay you can cancel, scheduled send,
  reply to one message in a thread, and a subject you can retitle without
  breaking threading.
- Address completion from the whole store — every sender, plus everyone you
  have written to, ranked so people you write to outrank people who have only
  written to you.
- Drafts mirror into the thread list as a local message row and push to Gmail
  in the background, so they appear at once and also reach the phone.
- Discard, confirmed and not undoable, asked in the composer footer where
  discard was pressed.
- Report spam, from the keyboard, the context menu and the palette.
- Snooze that asks when.
- Attachments: fetch, cache, open, save, and inline `cid:` images.
- Day, week and month calendar with drag-to-create, drag-to-move, alt-drag to
  copy, context menus, a sidebar, and an event view carrying guests and their
  answers, recurrence, reminders and the Meet link.
- Calendar invitations render inside the message, with RSVP in place and a note.
- `sendUpdates` on every calendar write, by value with no default. Before this,
  every event ever created in Mach with an attendee on it recorded the names
  and mailed nobody.
- Google's own calendar colours, verbatim in light and clamped into a
  measured OKLCH lightness band in dark.
- New-mail notifications and a Dock badge, under a narrow rule: unread, in the
  inbox, not from you, and either in Gmail's Personal category or continuing a
  thread this account has written to.
- ⌘Z / ⇧⌘Z over an undo stack that does not expire, with a toast, and a status
  line that names what ⌘Z would do once the offer lapses.
- Preferences for theme, default account, per-account signatures, undo window,
  send delay, week start, working hours and sync interval. Separately, the
  reading-pane split, the rail and the last mailbox survive a restart.
- An in-app agent that runs on the Claude Code CLI and needs no API key. Its
  tools are the same typed command layer the keyboard dispatches, so everything
  it does is undoable and lands in the same log as manual actions.
- Agent-suggested reply stances: two labelled stances on an eligible thread,
  each with its whole reply already written, so picking one is a local read.
  They live in local SQLite and never become a Gmail draft until one is picked. Generation is capped at twenty an hour and fifty a day, so a
  flood of mail cannot spend an afternoon's worth of quota.
- Unsubscribe, `⇧⌘U`: archives the conversation and unsubscribes behind it. One
  request where the sender supports RFC 8058 one-click, the unsubscribe mail
  sent for you where they offer only `mailto:`, and a button that opens the page
  where all they offer is a form — Mach does not click through a page it has not
  read. It will not offer at all on mail that looks like spam, where confirming
  the address is live is the wrong move; that gets the spam report instead.
- `⌥⌘C` copies what is on screen as plain text, quotes stripped, for pasting
  into a chat window somewhere else.
- Handoff: a terminal session running inside Mach, up to four at once. An empty
  handoff opens a field with the caret in it rather than an error panel.
- A plugin runtime. A plugin composes the command layer rather than extending
  it — no new primitive, no token, no path to Google — and one declared action
  becomes a ⌘K entry, a keybinding and an agent tool at once.
- Right-click menus whose items are looked up in the live keymap, so choosing
  one calls the same handler the key calls.
- A trackpad swipe changes the calendar period.
- A resizable account rail, reading-pane divider and agent drawer, all keyboard
  operable, all persisted.
- ⌘K → Send feedback: captures the window, lets you annotate the screenshot,
  and writes the result to `.feedback/inbox/` for a coding agent to pick up.
- Ramp as the app icon, drawn at two masters with the cut at 32px.
- The site at machmail.dev — one HTML file, no build step, no external requests.
- A QA harness. A dev instance opens a loopback port speaking three verbs —
  `key`, `click`, `ui` — so an agent can drive the real window without taking
  the keyboard.
- CI on pull requests and pushes to main: typecheck, vitest, and `cargo test`
  on macOS.

### Changed

- Bundle targets are `app` and `dmg`. Mach is macOS only.
- Address completion reads SQLite instead of folding a book out of whatever
  happened to be in memory.
- `calendarList.list` is fetched six-hourly rather than on every sync pass,
  which was about 1,440 requests a day per account for a dozen rows that change
  a few times a year.
- Message HTML ages out of the store and is refetched on demand.
- Snooze moves to `b` to match Gmail, with `h` kept as an undocumented alias.
  Mode switching moves to `g m`. Link insertion moves to ⇧⌘K so that ⌘K stays
  the palette.
- `x` ticks a row and leaves the cursor alone. Gmail's `x` does the same
  nothing.
- Toasts moved to the bottom right.
- One thread row: sender and date, subject, preview, at 13/14/11.
- A type ramp with five steps and a 4pt spacing grid, written down with three
  named exceptions.

### Removed

- `tauri-plugin-shell`.
- The Windows subsystem attribute, ten Windows tiles and the Vite favicon the
  Tauri template left behind.
- The density preference. There is one display.

### Fixed

- Mail banners carry Mach's name and icon instead of Terminal's. A development
  build is a bare binary with no bundle identifier, so `mac-notification-sys`
  borrowed one, and the only identifier it will accept is one LaunchServices can
  resolve. `scripts/dev-bundle` registers a minimal `Mach.app` — an `Info.plist`
  and the icon, with nothing executing from it — and the notifier now asks for
  Mach's own identifier. macOS asks for notification permission once, because
  this is a new application to it.
- ⌘Z puts an archived row back in 13ms instead of 966. Five causes, all in the
  seam between an optimistic guess and the list it is drawn against.
- One trackpad flick moves one period. The re-arm rule now reads speed, which
  is invariant to event coalescing and to refresh rate; it was reading
  magnitude, so a single flick moved two weeks and two moved four.
- The launch could hang with no window at all: `restore_accounts` read the
  Keychain on the launch thread, and macOS blocks that read behind a password
  dialog nobody could see.
- Links in messages were dead, in WebKit only. WebKit will not invoke a
  listener whose document has scripting disabled, and refuses `target="_blank"`
  from a frame without `allow-popups`. External navigation is now cancelled in
  a Tauri `on_navigation` hook, below the web engine, and opened in the system
  browser.
- ⏎ in the editor split the first block instead of the one being written in, so
  each new line landed above everything already typed, and a typed URL never
  became a link. Squire never heard the focus event, because StrictMode focused
  the element between two mounts.
- ⏎ and the arrows in a search result moved the result cursor while the caret
  should have been moving through a reply. `composer-keys.test.ts` reads the
  source now, so the next binding to take a key the editor owns fails a test
  rather than a message.
- Clicking anywhere in a message body killed every shortcut in the app until
  you clicked somewhere else. The frame forwards its keydowns into the one
  registry and keeps only the keys that mean something inside a document.
- A collapsed message showed 554 characters of the sender's CSS. Gmail's
  snippet is a required field on `Message` now.
- ⏎ at "hando" in the palette handed the half-typed keyword to the agent. An
  empty instruction is a refusal rather than a default.
- The calendar opened on a wall of night: a `scrollTop` write is a request, and
  an element with no layout takes it and keeps zero.
- Sending a draft-backed reply left a duplicate draft behind — two faults, one
  of them an autosave firing 13ms after ⌘⏎. A send now routes to `drafts.send`,
  and a deleted draft leaves a tombstone that later writes refuse.
- Snoozed conversations came back.
- `Mach.app` was built around `plugin_probe`. Nothing said which of the three
  binaries is the application, so the bundler chose by name order.
- Starring waits for nothing; it was going through IPC, the local write, the
  Gmail call, a change event and a refetch before it appeared.
- Gmail snippets rendered HTML-encoded, so a third of the store read
  "Sure I&#39;m free".
- The event modal could not save. ⌘⏎ was stopping propagation before the date
  field committed its buffered text, and Save was disabled on `!dirty`, which
  is indistinguishable from "you have not finished typing".
- `recurringEventId`, `htmlLink`, `isDraft` and `snippet` were each dropped
  silently by the IPC mapper. Rust now serializes fully-populated structs and
  each mapper runs against that sample behind a proxy that asserts every key
  was read.
- The preferences title sat under the macOS traffic lights. A GitHub table
  column collapsed to one letter per line, from a `word-break:break-word` that
  is the legacy spelling of `overflow-wrap:anywhere`. `e` archived a
  conversation behind an open dialog. All three were shipped off green
  headless-Chrome runs, which is why the QA harness drives a real window.
- A user write no longer queues behind the sync loop, and the test that says so
  asserts a distribution rather than one sample, so CI stops failing for no
  defect.
- A handoff's output could vanish entirely on Linux: the pty slave was dropped
  between spawning the child and attaching the reader.

### Security

- Plugins run in a hidden iframe on its own origin, behind a CSP response
  header with `connect-src 'none'`, reachable only by `postMessage`. 22 of 22
  escape attempts were blocked in a real Tauri window on macOS, with a positive
  control so a machine with no network cannot pass by accident. Authority comes
  from the manifest grant and not from the prose: a plugin describing itself as
  safe still parks for approval.
- Executable attachments are refused outright rather than put behind a confirm
  dialog, and an image's type is decided by sniffing magic bytes rather than
  the sender's `Content-Type`.
- Every user value in a search is bound, never interpolated.
- The QA port is `#[cfg(debug_assertions)]`, so a release build does not
  contain it. It binds loopback only, opens only when `MACH_DATA_DIR` is set,
  compares a 32-byte token in constant time, and has no verb that runs code.
  The frontend half drops out of the shipped bundle the same way.
- A QA instance cannot name the owner's Keychain entries or reach the owner's
  mailbox with synthetic input.

[Unreleased]: https://github.com/bborn/mach/commits/main
