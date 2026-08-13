# Security

Mach holds a Google OAuth refresh token per account and a full local copy of
your mail. That is worth more than the code is, so this file says what is
stored where, and how to tell me if you find a way to reach it.

## Reporting a vulnerability

Use GitHub's private reporting:
[**Report a vulnerability**](https://github.com/bborn/mach/security/advisories/new).
It goes to me and nobody else, and it does not open a public issue.

Please do not file a public issue for anything that could be used against
somebody else's mailbox before I have had a chance to fix it.

This is one person's side project. I will read the report and I will fix what
is real, but there is no bounty, no SLA, and no guarantee I get to it this
week. If a week goes by with no answer, mail me at the address on my GitHub
profile.

## What Mach holds

| Thing | Where | Notes |
|---|---|---|
| OAuth refresh tokens | macOS login Keychain, service `com.mach.mail.oauth`, one entry per account | Never written to disk by Mach, never logged |
| Access tokens | Memory only | Refreshed 60s before expiry |
| Mail, calendar, contacts, attachments | SQLite under the app's data directory | Not encrypted beyond FileVault and the filesystem's own permissions |
| Your Google OAuth client id and secret | `.env.local`, gitignored | Yours, not mine — Mach ships no credentials |

The store is a plain SQLite file. Anything running as your user can read it,
and Mach adds no second layer over that.

## In scope

- Anything that reads a refresh token from a process that should not have one.
- Anything that lets a message body, an attachment, a calendar invitation or a
  plugin escape its sandbox, reach the network, run script in the app's origin,
  or dispatch a command.
- Anything that widens a plugin's or the agent's authority past what its
  manifest was granted.
- SQL injection through the search parser, or anywhere else a user or sender
  value reaches the database.
- Anything that sends, deletes or modifies mail or calendar data without the
  user asking.

## Out of scope

These are known and are the design, not bugs:

- **The "Google hasn't verified this app" screen.** Mach is not submitted to
  Google for verification and never will be. You register your own OAuth client
  and consent to your own app.
- **Local reads of the SQLite store** by something already running as your
  user. See above.
- **Unsigned or un-notarized builds.** A build you made yourself is not
  notarized, and Gatekeeper will say so. See `CONTRIBUTING.md`.
- **The dev signing identity** in `scripts/sign`. It is a self-signed
  certificate whose only job is to stop the Keychain re-prompting on every
  rebuild. It grants nothing and is not used for distribution.
- **The QA loopback port.** It is `#[cfg(debug_assertions)]`, so it is not in a
  release build at all. In a debug build it opens only when `MACH_DATA_DIR` is
  set, binds `127.0.0.1`, requires a 32-byte token from a `0600` file compared
  in constant time, refuses a request carrying an `Origin` or a non-loopback
  `Host`, and has no verb that runs code.
- **`MACH_QA_INPUT=1` and `MACH_QA_FOREGROUND=1`.** Both are escape hatches, set
  by a person on a command line. Nothing an agent runs sets either.

## Supported versions

Whatever is on `main`. There is no back-porting.
