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

Refresh tokens live in the login Keychain, and macOS decides whether to ask for
your password by looking at the code signature of whatever is reading them.
`cargo build` produces an ad-hoc signature that changes with every build, so
every rebuild looks like a different program and you get a password prompt per
account per launch. `scripts/sign` is what fixes that. Mach builds and runs
without it; you just get the prompts.

There are two identities it can use, and they are not equally good.

**A Developer ID Application certificate** is the one that actually stops the
prompts. Every Keychain item carries a *partition list* naming the signing
identities allowed to use it, and the only entry macOS can write for a
certificate with no Apple Team ID is a hash of the binary — which your next
build invalidates. A Developer ID carries a real Team ID, so the entry is
`teamid:<TEAM>`, and that survives rebuilding.

You need an Apple Developer account. In Xcode: Settings → Accounts → your Apple
ID → Manage Certificates → **+** → **Developer ID Application**. Or create and
download one at
<https://developer.apple.com/account/resources/certificates> and double-click
it to import. Creating a Developer ID certificate may need the Account Holder
role on the team.

Check that it landed:

```sh
security find-identity -v -p codesigning
```

You want a line like `Developer ID Application: Your Name (AB12CD34EF)`. **The
Team ID in parentheses is the part that matters** — a certificate without one is
not the certificate you need. `scripts/sign` finds it by shape, so there's
nothing to configure; run it and it tells you which identity it used and what
team that is:

```
sign: identity "Developer ID Application: Your Name (AB12CD34EF)" — team AB12CD34EF
sign: mach → com.mach.mail (teamid:AB12CD34EF)
```

The first launch after that still prompts **once per account**, because your
existing Keychain items were last approved against a binary hash and macOS has
to write the new `teamid:` entry into their partition lists. Click **Always
Allow**, not Allow, on each. One prompt per account, and then it stops,
including across rebuilds.

**The self-signed fallback** (`Mach Dev Signing`) is what you get if no
Developer ID is installed. It gives every build the same identity, which is
worth having on its own, and the Keychain keeps asking because a self-signed
certificate has no team. `scripts/sign` says so plainly when it falls back, and
`scripts/sign --setup` prints the setup for both routes.

`MACH_SIGN_IDENTITY` overrides the choice if you want a specific one.

Signing enables the hardened runtime (`--options runtime`). No entitlements are
needed: Mach isn't sandboxed, so the login Keychain needs no entitlement to
reach, and Mach loads no plugin dylibs — plugins and agents are separate
processes — so library validation has nothing to reject. One consequence, and
it applies to the self-signed route too: a hardened binary can't be attached to
by a debugger. If you want `lldb`, sign with an entitlements file containing
`com.apple.security.get-task-allow`, and never ship that build.

### Notifications in a development build

Mail banners used to arrive wearing Terminal's name and Terminal's icon.
`scripts/dev-bundle` is what stops that. It writes a minimal `Mach.app` into the
target directory and registers it with LaunchServices, and both ways of starting
the app already run it: `scripts/qa up` before it launches, and
`beforeDevCommand` for `bun run tauri dev`.

A development build is a bare binary with no bundle, and `NSUserNotification`
will not deliver for a process without a bundle identifier.
`mac-notification-sys` gets around that by swizzling
`-[NSBundle bundleIdentifier]` to return a borrowed string, and it accepts only
a string LaunchServices can resolve. When it cannot, it falls back to a literal
`com.apple.Terminal` compiled into the crate, which is where the icon came from.

The bundle is an `Info.plist` and an icon. Nothing executes from it: the binary
is a plain Mach-O, signed the way it always was. Moving it inside the bundle
would make the process *bundle code* as far as the Security framework is
concerned, and the login Keychain matches its ACLs against code identity — a
wall of password prompts for a wrong icon.

The path, on its own, is not part of that identity. `scripts/qa up` launches
every instance from a private copy under `.qa/<instance>/bin/mach` rather than
from `target/debug/mach`, so that one agent's `cargo build` cannot swap the code
another instance is running. The copy is signed with the same identifier and the
same Developer ID, which is the whole of what the ACL and the partition list
check: `codesign -d --requirements -` on the copy and on the artifact print the
same designated requirement, and neither mentions a path.

Expect one thing once. `com.mach.mail` is a new application as far as macOS is
concerned, so the first banner asks for notification permission; allow it.
System Settings → Notifications grows a "Mach" entry, and whatever was set
against Terminal no longer governs Mach's mail.

`cargo run --bin notify_live` puts one real banner on screen through the same
code the sync loop uses, and prints which conversation the click routed to. It
is the only way to look at a banner, and

```sh
/usr/bin/log show --last 5m --predicate 'process == "usernoted"' --style compact | grep Presenting
```

is how you check whose name is on it. Use the full path; `log` is shadowed in
zsh.

### Running a build you made

A `.app` you built yourself carries no quarantine attribute, so it opens
normally. A `.dmg` you downloaded from a GitHub release does carry one, and
unless the release was notarized, macOS will refuse it. `RELEASING.md` covers
that end.

## License

MIT. By opening a pull request you're licensing your change under it. There's
no CLA.
