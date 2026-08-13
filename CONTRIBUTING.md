# Contributing

This is my mail client. I built it for me and put it up here in case it's
useful to anybody else. So before you spend an evening on something:

- I use this every day, and I'll break anything that's in the way of that.
- I might not merge your pull request. Mach has one user and I'm him, and a
  feature I won't use is a feature I won't maintain.
- There's no roadmap and no support. Issues are where you tell me something is
  broken. I read them, and that's the whole promise.

Forking is a completely reasonable outcome, and MIT is there for that.

## What is likely to get merged

- A bug you hit while reading your own mail in it, with the repro.
- A fix that makes an existing thing correct, small enough to read in one
  sitting.
- Anything that makes a failure visible that was silent before. Silent failure
  is the thing that has cost this project the most time.

## What is not in scope

Named in the README, and none of it is a gap I'm waiting for help with: IMAP or
SMTP, any provider that isn't Google, Windows, Linux, mobile, web, teams,
sharing.

## Before you open a pull request

Read [`CLAUDE.md`](CLAUDE.md). It carries the standing rules — everything is
keyboard navigable, shortcuts match Gmail and Google Calendar, the UI never
waits on Google, failure has to be visible, use the primitives in
`src/components/ui/`. It also has the rules for interface copy and for prose,
which are stricter than you'd expect.

Then run all four:

```sh
cd src-tauri && cargo test    # Rust core
bunx vitest run               # frontend
bunx tsc --noEmit             # types
bun run build                 # production frontend bundle
```

CI runs the first three. Both suites are offline against fakes; nothing in them
touches Google.

If the change touches full-window layout, a window edge, anything macOS also
draws, or anything WebKit decides, look at it in the real app before you call
it done. A screenshot from `bun run dev` is a browser tab — no traffic lights,
no overlay title bar, a different scrollbar engine. Four defects shipped that
way. `scripts/qa` drives a real window without taking your keyboard:

```sh
MACH_QA_INSTANCE=yours scripts/qa up
MACH_QA_INSTANCE=yours scripts/qa key 'mod+,'
MACH_QA_INSTANCE=yours scripts/qa shoot after
```

## Commit messages

The subject says what changed from the user's side, in the present tense, and
fits on one line: `fix(undo): ⌘Z puts the row back in 13ms instead of 966`. The
body says why, at whatever length the reasoning needs — recovering the
reasoning later is the expensive part, and the log is where I go looking for
it.

## Building it

The README covers the whole install, including the Google Cloud setup, which is
the fiddly part. Short version:

```sh
bun install
bun run tauri dev
```

You need macOS, [Rust](https://rustup.rs) stable with the Xcode command line
tools, [Bun](https://bun.sh), and your own Google OAuth client in `.env.local`.

### Signing your local builds

`scripts/sign` gives every dev build the same identity so the login Keychain
stops asking for your password on every rebuild. It's a self-signed certificate
that lives on your machine, it has nothing to do with distribution, and Mach
builds and runs fine without it — you just get the prompts. `scripts/sign
--setup` prints what to run.

### Running a build you made

A `.app` you built yourself carries no quarantine attribute, so it opens
normally. A `.dmg` you downloaded from a GitHub release does carry one, and
unless the release was notarized, macOS will refuse it. `RELEASING.md` covers
that end.

## License

MIT. By opening a pull request you're licensing your change under it. There's
no CLA.
