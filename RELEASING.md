# Releasing

A release here is a `.dmg` and a `.app.tar.gz` attached to a GitHub release,
built by `.github/workflows/release.yml` when a `v*` tag is pushed. Nothing
auto-updates. There is no beta channel and no schedule.

## Read this before the first tag

**A packaged build has no way to receive an OAuth client, so today a `.dmg`
boots straight into "cannot add an account" and stays there.**

`config.rs` loads `.env.local` under `#[cfg(debug_assertions)]` only, and the
comment says why: a shipped app must not pick up a file that happens to sit next
to it. Correct, and it leaves a release build reading the process environment —
which, for an app launched from Finder, `launchd` hands over empty. There is no
preferences field for the client id and secret, and no onboarding step that asks
for them.

The only things that work right now are ugly:

```sh
launchctl setenv MACH_GOOGLE_CLIENT_ID <id>.apps.googleusercontent.com
launchctl setenv MACH_GOOGLE_CLIENT_SECRET GOCSPX-<secret>
# then launch Mach. Undone by a reboot.
```

or running the executable inside the bundle from a shell that has them exported:

```sh
set -a && source .env.local && set +a
/Applications/Mach.app/Contents/MacOS/mach
```

Neither is something to put in a release note. The machinery below is ready and
the workflow is safe to run; a release is worth publishing once a packaged build
can be told its credentials — a preferences field writing to the Keychain, or a
first-run sheet, would do it.

## Cutting one

```sh
scripts/version 0.2.0          # package.json, tauri.conf.json, Cargo.toml
$EDITOR CHANGELOG.md           # rename [Unreleased] to [0.2.0] - YYYY-MM-DD
git commit -am "chore: 0.2.0"
git tag v0.2.0
git push origin main --tags
```

The tag is the trigger. The workflow builds a universal bundle, creates a
**draft** release, and stops — nothing is public until you press publish, so a
build that comes out wrong costs a deleted draft rather than a retracted
release.

To prove the workflow works without releasing anything, run it from the Actions
tab (`workflow_dispatch`). Same build, no release, the bundle comes back as a
workflow artifact.

**Do that once before the first tag.** The build is
`--target universal-apple-darwin`, which cross-compiles the x86_64 half on an
Apple silicon runner, and `ring` and `aws-lc-sys` are in the tree with C and
assembly in them. That should work and has not been run here. If the x86_64
slice fails, the fix is a matrix on native runners — `macos-latest` for
`aarch64-apple-darwin` and `macos-15-intel` for `x86_64-apple-darwin` — which
gives two `.dmg`s instead of one, and needs the release created by a job ahead
of the matrix so two parallel builds do not race to create the same one.

If the tag and the versions in the three files disagree, the workflow fails
before it builds anything and tells you to run `scripts/version`.

## The ad-hoc build, which is what this is

Mach has no Apple Developer ID, so releases are not notarized. macOS attaches
`com.apple.quarantine` to anything downloaded from the internet and refuses to
open a quarantined app it cannot verify.

**There are three states, and they are not degrees of the same thing:**

| The build is | The dialog says | Can the user get past it? |
|---|---|---|
| Unsigned | "Mach is damaged and can't be opened. You should move it to the Trash." | **No.** There is no Open Anyway button |
| Ad-hoc signed, not notarized | "Apple could not verify Mach is free of malware…" | Yes, through System Settings |
| Signed and notarized | "Mach is an app downloaded from the Internet. Are you sure?" | Opens |

The first row is the trap. An unsigned arm64 binary is refused by the kernel
before Gatekeeper is consulted, so `xattr` does not help and the user is told
their download is corrupt. This is why the workflow sets
`APPLE_SIGNING_IDENTITY=-` when there is no certificate. Tauri's own
distribution docs say the same: configure an ad-hoc identity, or macOS treats
Apple silicon builds downloaded from GitHub releases as damaged.

Ad-hoc costs nothing, needs no account and no secret, and moves the user from
row one to row two. It is not security — an ad-hoc signature proves nothing
about who built the app.

What the user has to do, once, per download:

**Drag Mach to Applications first.** Files copied out of a quarantined `.dmg`
get their own fresh quarantine attribute, so clearing it on the `.dmg` does
nothing; and an app launched from Downloads runs under App Translocation from a
randomised read-only path, where nothing you do to the original applies. Then
either:

```sh
xattr -dr com.apple.quarantine /Applications/Mach.app
```

or double-click it, let macOS refuse, then open **System Settings → Privacy &
Security**, go to Security, and press **Open Anyway**. That button only appears
after the refused attempt and Apple documents it as lasting about an hour, so
the order matters.

Three things that generate support questions:

- `xattr -c` takes no attribute name, so `xattr -c com.apple.quarantine <app>`
  reads the attribute as a filename. `-dr` with the name is the form to publish.
- `xattr` without `-r` only touches the top level of the bundle.
- macOS may answer `Operation not permitted` from App Management (TCC). `sudo`
  does not fix that — granting the terminal App Management does, which is why
  so many projects publish a `sudo` that isn't doing anything.

Control-click → Open is not the answer any more. Apple removed that override in
macOS Sequoia: "users will no longer be able to Control-click to override
Gatekeeper when opening software that isn't signed correctly or notarized.
They'll need to visit System Settings > Privacy & Security." Never publish
`spctl --master-disable` either; it stopped modifying the assessment database in
macOS 15.

The release workflow puts these instructions in the release body, generated from
whether the build was actually notarized, so the note cannot say one thing while
the artifact is another.

An ad-hoc build is also unverifiable: nothing about the download proves it came
from this repository. Building from source is the higher-trust path.

If the terminal step is the part that annoys you, a Homebrew cask installed with
`brew install --no-quarantine` never gets the attribute in the first place. That
is a second distribution channel to maintain, so it is not here.

## Turning signing on

You need the Apple Developer Program, which is $99/year. A free Apple ID cannot
issue a Developer ID certificate and cannot notarize.

1. In the Apple Developer account, create a **Developer ID Application**
   certificate and install it in your login Keychain.
2. Export it from Keychain Access as a `.p12` with a password.
3. Base64 it:

   ```sh
   openssl base64 -in DeveloperID.p12 -out DeveloperID-base64.txt
   ```

4. Create an app-specific password for your Apple ID at appleid.apple.com.
5. Add these repository secrets:

   | Secret | Value |
   |---|---|
   | `APPLE_CERTIFICATE` | contents of `DeveloperID-base64.txt` |
   | `APPLE_CERTIFICATE_PASSWORD` | the `.p12` export password |
   | `APPLE_ID` | your Apple ID email |
   | `APPLE_PASSWORD` | the app-specific password |
   | `APPLE_TEAM_ID` | the team ID from the Apple Developer account page |

Nothing in the workflow changes. It checks whether the certificate secrets have
content and only then puts them in the environment; the notarization secrets
are gated on the certificate as well, because notarizing an unsigned bundle
cannot succeed. Remove the secrets and it goes back to unsigned builds without
an edit.

The first two secrets alone get you a Developer ID signature without
notarization, which lands in the middle row of the table above — the same place
ad-hoc already puts you, with a $99 receipt. Notarization is the part users
feel.

The ad-hoc identity is dropped automatically once a certificate is present. It
is set only in the no-certificate branch, and never alongside
`APPLE_CERTIFICATE`, because the bundler compares a supplied identity against
the certificate's own and fails the build when they differ.

## `scripts/sign` is not this

`scripts/sign` uses a self-signed certificate called "Mach Dev Signing" that
lives on one machine. Its whole job is to give every dev build the same code
identity so the login Keychain stops asking for your password on every rebuild.
It is not a Developer ID, no other machine trusts it, and using it for a release
would produce a bundle that fails Gatekeeper in a more confusing way than an
unsigned one. The two have nothing to do with each other.

## What the workflow does not do

- **No updater.** `uploadUpdaterJson` is off and there is no updater plugin in
  the app. If that ever changes it needs `TAURI_SIGNING_PRIVATE_KEY`, which is a
  different key from everything above.
- **No Homebrew cask, no Sparkle, no App Store.**
- **No `cargo test`.** CI ran it on the commit being tagged. The release
  workflow runs `vitest`, and `tauri build` typechecks on its way through
  `bun run build`.
