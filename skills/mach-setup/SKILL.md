---
name: mach-setup
description: Set up Mach's Google OAuth credentials end to end — create a Google Cloud project, enable the Gmail, Calendar and People APIs, configure the Google Auth Platform consent screen, create a Desktop app OAuth client, add the eight scopes, publish to production so refresh tokens stop expiring after 7 days, write .env.local, and verify with the oauth_smoke example. Use when someone is setting up or re-setting-up Mach, hits "not configured", sees "Google hasn't verified this app", is being asked to re-authorize every week, or asks how to get Mach's Google credentials.
---

# Setting up Mach's Google OAuth client

Mach ships no credentials. Every user registers their own Google Cloud project,
because Mach is not submitted to Google for verification and never will be.

You are driving this **with** the user, not for them. Most of it you can do in a
browser; a few steps are legally and practically theirs. Read "Hand the keyboard
back" before you start.

## Hand the keyboard back — non-negotiable

Stop and let the human act for these. Do not work around them.

- **Signing in to Google.** Never type, request, read, or store their password,
  2FA code, or recovery code. If a sign-in screen appears, say so and wait.
- **Accepting the "Google API Services: User Data Policy" checkbox** on the
  consent screen, and the equivalent agreement when publishing. That is a legal
  agreement about how *their* app handles *other people's* data. They tick it.
- **Granting consent** in the final authorization flow. They choose which of
  their mailboxes Mach may read.

Two more rules:

- **Never paste the client secret into chat, a commit, an issue, or a log.**
  Write it straight to `.env.local` and confirm only that it was written.
- **Never commit `.env.local`.** It is already gitignored; keep it that way.

## Before you start

```sh
cd <the mach checkout>
test -f .env.local && grep -c MACH_GOOGLE .env.local   # already set up?
```

If `.env.local` already has both `MACH_GOOGLE_CLIENT_ID` and
`MACH_GOOGLE_CLIENT_SECRET`, skip to **Step 8 (verify)**. If verification fails
with an auth error, the likely cause is that the app is in *Testing* status —
jump to **Step 7**, that alone fixes the weekly re-authorization problem.

Ask the user which Google account should **own** the Cloud project. It does not
have to be a mailbox Mach will read, and using a personal account with **no
organization** is the simplest path — an org can impose policies that block
External apps.

## How to drive the console

Use whatever browser tooling you have (Playwright, Chrome DevTools MCP,
`claude-in-chrome`, `agent-browser`). Navigate by **direct URL**; the console's
navigation shifts and the sidebar labels change, but these paths are stable:

| Purpose | URL |
|---|---|
| Create project | `https://console.cloud.google.com/projectcreate` |
| Enable Gmail API | `https://console.cloud.google.com/apis/library/gmail.googleapis.com` |
| Enable Calendar API | `https://console.cloud.google.com/apis/library/calendar-json.googleapis.com` |
| Auth Platform overview | `https://console.cloud.google.com/auth/overview` |
| Branding (app name, support email) | `https://console.cloud.google.com/auth/branding` |
| Audience (user type, publishing status) | `https://console.cloud.google.com/auth/audience` |
| Data access (scopes) | `https://console.cloud.google.com/auth/scopes` |
| Clients (OAuth clients) | `https://console.cloud.google.com/auth/clients` |

Append `?project=<project-id>` to keep the right project selected.

**If you cannot drive a browser**, do not guess and do not stall: paste the
steps below to the user as a checklist, one step at a time, and wait for them to
report back what they see. Every step is written to be followed by a human.

**Read the screen, not your memory of it.** Google renames things. If a button
you expect is not there, screenshot or dump the page text and describe what you
actually see before clicking anything.

---

## Step 1 — Create the project

Go to `https://console.cloud.google.com/projectcreate`.

- **Project name**: `mach` (anything works)
- **Location / Organization**: **No organization**, if offered

Creating takes a few seconds and shows a notification when done. Capture the
**project ID** — it is the name plus a numeric suffix, e.g. `mach-4f2c81`. You
need it for the `?project=` query parameter.

## Step 2 — Enable the three APIs

Mach uses exactly three. Enabling anything else widens the app for no reason.

1. `https://console.cloud.google.com/apis/library/gmail.googleapis.com?project=<id>` → **Enable**
2. `https://console.cloud.google.com/apis/library/calendar-json.googleapis.com?project=<id>` → **Enable**
3. `https://console.cloud.google.com/apis/library/people.googleapis.com?project=<id>` → **Enable**

Each takes 5–20 seconds. The button becomes **Manage** when it has worked. If it
still says "Enable" after a reload, it did not work — retry before continuing.

> The Calendar API's service name is `calendar-json.googleapis.com`, not
> `calendar.googleapis.com`. Searching the library for "Google Calendar API" is
> the reliable way to find it.

> The People API is the one that carries `contacts.other.readonly`. Granting the
> scope without enabling the API is the failure that looks like nothing at all:
> consent succeeds, the token comes back carrying the scope, and every call
> returns 403 `SERVICE_DISABLED`.

## Step 3 — Configure the consent screen

Google moved this. It is no longer under "APIs & Services → OAuth consent
screen"; it now lives under **Google Auth Platform**, at
`https://console.cloud.google.com/auth/overview?project=<id>`.

On a fresh project this shows a **Get started** flow with four short panels:

1. **App Information** — App name (`Mach`), User support email (pick the owner's
   address from the dropdown).
2. **Audience** — choose **External**.
   *Internal* is only offered inside a Google Workspace organization, and it is
   the better option if the user has one: it skips verification entirely. With a
   personal account it will not be selectable. Pick External and move on.
3. **Contact Information** — an email address for Google to reach them at. The
   same address is fine.
4. **Finish** — the **User Data Policy** agreement checkbox.
   **This one is the user's.** Ask them to read and tick it, then continue.

If the project has already been through this, the same fields are editable at
`/auth/branding` and `/auth/audience` instead.

## Step 4 — Create the OAuth client: **Desktop app**

`https://console.cloud.google.com/auth/clients?project=<id>` → **Create client**.

- **Application type**: **Desktop app** ← this matters, see below
- **Name**: `Mach desktop`

Then **Create**.

### Why Desktop app and nothing else

Mach's OAuth flow is authorization-code + PKCE against a **loopback redirect on
an ephemeral port**: it binds `127.0.0.1:0`, lets the OS pick a free port, and
tells Google to redirect to `http://127.0.0.1:<that port>/oauth/callback`. The
port is different every run, so it cannot be pre-registered.

**Desktop app is the only client type Google allows to do that.** A "Web
application" client requires every redirect URI to be registered up front and
rejects an unregistered port with `redirect_uri_mismatch`. Choosing Web here is
the single most common way to get stuck, and the error arrives late — at the
first authorization, not at creation time.

You do not need to enter any redirect URI for a Desktop client. If the form is
asking you for one, you picked the wrong type.

### Capture the credentials now

The dialog that appears after **Create** shows the **Client ID** and the
**Client secret**.

**The client secret is unrecoverable once this dialog closes.** The console will
show the ID again forever, and the secret never. If it is lost, the only fix is
to create a new client (or use **Download JSON** from the dialog, which contains
both).

Write them to `.env.local` immediately — Step 6 — before doing anything else.

## Step 5 — Add the eight scopes

`https://console.cloud.google.com/auth/scopes?project=<id>` → **Add or remove
scopes**. Paste all eight into the "manually add scopes" box, comma- or
newline-separated, then **Add to table** → **Update** → **Save**.

```
openid
https://www.googleapis.com/auth/userinfo.email
https://www.googleapis.com/auth/gmail.modify
https://www.googleapis.com/auth/gmail.send
https://www.googleapis.com/auth/gmail.settings.basic
https://www.googleapis.com/auth/calendar
https://www.googleapis.com/auth/calendar.events
https://www.googleapis.com/auth/contacts.other.readonly
```

How Google tiers them — this determines what warnings appear later:

| Scope | Tier | What it is for |
|---|---|---|
| `openid` | non-sensitive | identity |
| `userinfo.email` | non-sensitive | learn which account authorized, so tokens key by email |
| `gmail.modify` | **restricted** | read, label, archive, trash |
| `gmail.send` | sensitive | the composer |
| `gmail.settings.basic` | sensitive | filters, and only filters |
| `calendar` | sensitive | calendar list and settings |
| `calendar.events` | sensitive | event read and write |
| `contacts.other.readonly` | sensitive | the sender's picture, where there is one |

`gmail.modify` is the one restricted scope, and it is deliberate: the broader
`https://mail.google.com/` would also grant permanent delete, and Mach does not
need it. Do not "simplify" by requesting the wider scope.

Restricted and sensitive scopes are what trigger Google's verification
requirement. You are not going to satisfy it, and you do not need to — see the
next step.

## Step 6 — Write `.env.local`

At the repository root (next to `package.json`), not inside `src-tauri/`:

```sh
cat > .env.local <<'EOF'
MACH_GOOGLE_CLIENT_ID=<paste the client id>
MACH_GOOGLE_CLIENT_SECRET=<paste the client secret>
EOF
chmod 600 .env.local
```

Optionally add `ANTHROPIC_API_KEY=sk-ant-...` for the in-app agent. Mach runs
fine without it; the agent reports a missing key as a typed error on the one
action that needs it.

Confirm without echoing the values:

```sh
grep -c '^MACH_GOOGLE_' .env.local          # expect 2
git check-ignore -v .env.local              # expect a .gitignore hit
```

If `git check-ignore` prints nothing, **stop** — the file is not ignored and is
one `git add .` away from being published. Fix `.gitignore` first.

## Step 7 — Publish to production (the counter-intuitive one)

`https://console.cloud.google.com/auth/audience?project=<id>` → under
**Publishing status**, click **Publish app** and confirm.

**Do this even though the app is unverified.** This is the single most valuable
thing in this document.

While publishing status is **Testing**, Google issues refresh tokens that
**expire after 7 days**. Mach stores a refresh token in the macOS Keychain and
relies on it to mint access tokens forever without user interaction; a 7-day
expiry means re-authorizing every account, every week, indefinitely. It looks
like a bug in the app. It is not.

Moving to **In production** removes that expiry. Google permits the transition
with a restricted scope attached: it warns that verification is required and
offers a Verification Center, but it **does not block publishing and does not
revert the status**. You can ignore the verification prompt.

What you accept by staying unverified:

- Each account sees a **"Google hasn't verified this app"** interstitial on
  first authorization. Click **Advanced** → **Go to Mach (unsafe)**. One time
  per account. Tell the user this in advance so it does not read as a failure.
- A **lifetime cap of 100 consenting users**. Irrelevant for personal use.
- Verification, if you ever wanted it, means a CASA Tier 2 security assessment
  for the restricted scope — currently $500–$4,500/year. Not needed.

While you are on this page, you can also add test users, but once the app is
**In production** that list is no longer used.

## Step 8 — Verify end to end

Do not declare success from the console. Prove it:

```sh
cd src-tauri
set -a && source ../.env.local && set +a
cargo run --example oauth_smoke
```

It prints a redirect URI and an authorization URL, then blocks on the loopback
listener. Give the URL to the user and let *them* open it, sign in, click
through the unverified warning, and grant consent.

A successful run prints:

```
  authorized  : <the account that consented>
  scopes      : ... eight scopes ...
  expires_at  : <a timestamp>
  refresh tok : issued and saved to the Keychain
```

Three things must be true:

1. **`refresh tok : issued and saved to the Keychain`.** If it says
   `NOT ISSUED — the app cannot stay signed in`, the account has an existing
   grant from a previous attempt. Revoke it at
   `https://myaccount.google.com/permissions` and run the smoke test again;
   Google only issues a refresh token on first consent.
2. **All eight scopes** are listed. A missing one means Step 5 did not save.
3. **macOS may prompt for Keychain access.** That is expected — the user
   clicks Allow (or Always Allow). Each freshly compiled unsigned binary is a
   new item to macOS and can prompt again.

Then run the app itself:

```sh
bun install
bun run tauri dev
```

`⌘K` → **Add account**. Repeat for each mailbox; each one gets its own consent
and its own Keychain entry.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `redirect_uri_mismatch` | OAuth client is type **Web application** | Create a new **Desktop app** client (Step 4). The type cannot be changed after creation. |
| `access_blocked` / "app is blocked" | Account is in a Workspace org whose admin restricts third-party apps, or the project is under an org with a policy | Use an account outside that org, or have the admin allow the client ID |
| Re-authorizing every ~7 days | Publishing status is still **Testing** | Step 7 |
| `refresh tok : NOT ISSUED` | Account already granted consent | Revoke at `https://myaccount.google.com/permissions`, retry |
| App boots and says it is "not configured" | `.env.local` missing, in the wrong directory, or the variables misspelled | Repo root, next to `package.json`; names are `MACH_GOOGLE_CLIENT_ID` / `MACH_GOOGLE_CLIENT_SECRET` |
| "Google hasn't verified this app" | Expected and permanent | **Advanced** → **Go to Mach (unsafe)**. Once per account. |
| Calendar API cannot be found in the library | Wrong service name | It is `calendar-json.googleapis.com` |
| `403 accessNotConfigured` at sync time | An API was not actually enabled | Reload Step 2's pages; the button must read **Manage** |
| Secret lost | The dialog is shown once | Create a second client and update `.env.local`; delete the old client |

## When you are done

Report to the user:

- the project ID, and that the app is **In production / unverified**
- that `.env.local` was written and is gitignored (not its contents)
- the `oauth_smoke` result, quoting the `refresh tok` line
- that the "hasn't verified this app" screen is expected, once per account

Do not describe setup as complete until `oauth_smoke` has printed
`refresh tok : issued and saved to the Keychain`.
