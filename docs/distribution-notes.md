# Distribution notes

Two questions. Should Mach publish a Google OAuth app so users stop registering
their own, and how do comparable open-source Mac apps handle the dev loop and
shipping.

The answers: don't publish, and the blocker in `RELEASING.md` is the thing to
fix instead. A packaged build that can be told its own client id and secret
unblocks a release and shortens the install, and costs nothing per year.

Web search was unavailable while this was written — the session's budget was
already spent — so every source below was fetched from a known URL and read.
Anything I could not verify says so.

## Question 1 — publishing the OAuth app

### No version of Mach avoids a restricted scope

Every Gmail scope that can list or read a message is restricted:
`mail.google.com/`, `gmail.readonly`, `gmail.modify`, `gmail.metadata`,
`gmail.compose`, `gmail.insert`, `gmail.settings.basic`,
`gmail.settings.sharing`. The non-restricted ones are `gmail.labels`, two
add-on action scopes, and `gmail.send`
([Gmail API scopes](https://developers.google.com/workspace/gmail/api/auth/scopes)).

`gmail.addons.current.message.readonly` is sensitive rather than restricted and
does grant message access, but only to a Gmail add-on while the add-on is
running. A desktop client cannot use it.

So "publish with only the non-restricted scopes" is not available to a mail
reader. The most Mach could publish without restricted-scope review is `openid`,
`userinfo.email`, `gmail.send`, `calendar` and `calendar.events` — a calendar
client that can send mail and cannot show an inbox.

### What verification requires

From [Verification requirements](https://support.google.com/cloud/answer/13464321):

- A homepage "hosted on a verified domain you own", which "must describe your
  app's functionality to its users" and cannot be only a login page. Domain
  ownership is verified through Search Console by a project owner or editor.
- A privacy policy "hosted within the domain that hosts your homepage", linked
  from both the homepage and the consent screen, with the two links matching. It
  must disclose how the app accesses, uses, stores and shares Google user data,
  and comply with Limited Use.
- A demo video showing "the end-to-end flow of your app including the OAuth
  grant process", with the app's real name and branding, the complete consent
  screen showing the same exact scopes, and every requested scope exercised.
- A justification per scope: "You must provide a detailed justification for your
  requested scope(s)", explaining why a narrower scope will not do.

Restricted-scope verification is quoted at **6 weeks**, against 10 business days
for sensitive scopes and 2–3 days for brand verification
([FAQ](https://support.google.com/cloud/answer/13463817)). On rejection, "users'
access to the unapproved sensitive or restricted scopes in the app via OAuth
will no longer work."

machmail.dev is a domain Bruno owns, so the homepage and domain requirements are
satisfiable. The privacy policy and the demo video do not exist, and both would
have to be made and then kept accurate as the app changes.

### The security assessment

> "Every app that requests access to Google users' restricted data and has the
> ability to access data from or through a third-party server must go through a
> security assessment from Google-empanelled security assessors."
>
> — [Restricted scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification)

The third-party-server qualifier is real, and Mach has no server: mail moves
from Google to the user's own Mac and stops. Google publishes nothing saying a
client-only desktop app is excused, and the
[security assessment page](https://support.google.com/cloud/answer/13465431)
says only that "the Google Trust and Safety team will contact you when it is
time to initiate the security assessment process". So whether Mach would be
asked is unknown until it applies, six weeks into a review.

If asked, the assessment is CASA, revalidated every 12 months from the effective
date of the previous Letter of Validation
([annual re-verification](https://support.google.com/cloud/answer/13463816)).

Two things about CASA have changed since it was cheap:

- **The free self-scan is gone.** The App Defense Alliance's own guide says "The
  CASA self scanning process is **deprecated**". It survives only as a way to
  "check your application readiness for the lab verified CASA scan"
  ([dynamic scan guide](https://appdefensealliance.dev/casa/tier-2/ast-guide/dynamic-scan)).
- **Both remaining levels are lab work.** AL1 and AL2 are each "Lab Tested - Lab
  Verified", and the developer must "contact one of the ADA authorized labs"
  ([assurance levels](https://appdefensealliance.dev/casa/casa-tiering)). The
  assessment "tests the application, the application deployment infrastructure
  and any user data storage location for compliance", and "All applications must
  be revalidated every year."

There are nine authorized labs: TAC Security, Bishop Fox, KPMG, Leviathan
Security, NCC Group, NetSentries Technologies, Orange Cyberdefense South Africa,
Prescient Security, DEKRA
([assessors](https://appdefensealliance.dev/casa/casa-assessors)).

### The price, and what I could not find out

Google's own statement:

> "Google does not charge the developer any fees for security assessment."
>
> — [FAQ](https://support.google.com/cloud/answer/13463817)

That covers Google's invoice. The labs set their own rates and negotiate
privately. **I could not find a current published price anywhere.** None of the
nine assessors puts a rate card at a URL I could fetch, the ADA publishes no
pricing, and Google's help pages carry no figure. The five-figures-a-year number
in the back of my head is unsourced and I am labelling it recalled, not cited.

What is cited is the shape of the commitment: a lab engagement, repeated every
year, paid by the developer, against an app with no revenue and one maintainer.

### A desktop app cannot hold the secret

> "Installed apps are distributed to individual devices, and it is assumed that
> these apps cannot keep secrets."
>
> — [OAuth 2.0 for Mobile & Desktop Apps](https://developers.google.com/identity/protocols/oauth2/native-app)

The same page marks `client_secret` optional at the token endpoint and notes it
"is not applicable to requests from clients registered as Android, iOS, or
Chrome applications" — the client types that are issued no secret. Desktop
clients get one and Google's token endpoint expects it.
`src-tauri/src/auth/mod.rs` already documents this, cites RFC 8252 §8.5, and is
why the flow is Authorization Code with PKCE and a loopback redirect.

Publishing therefore means compiling `GOCSPX-…` into a binary distributed from a
public MIT repository, where anyone can read it out of the bundle. From there:

- Every user of Mach shares one Cloud project's Gmail quota: 1,200,000 units per
  minute for the project, 6,000 per user per minute
  ([quota](https://developers.google.com/workspace/gmail/api/reference/quota)).
  Today each user has a project and a ceiling of their own, and one heavy user
  cannot slow anybody else down.
- Anything done with the extracted client is attributed to the verified project,
  which is Bruno's.
- The consent screen would say "Mach" for software that is not Mach.

Mailspring, an Electron Gmail client with 17.7k stars, ships exactly this and is
honest about it in
[`onboarding-constants.ts`](https://github.com/Foundry376/Mailspring/blob/master/app/internal_packages/onboarding/lib/onboarding-constants.ts):

> ```
> // we really do need to embed this in the application and it's more an
> // extension of the Client ID than a proper Client Secret.
> //
> // Note: This is not a security risk for the end-user -- it just means someone
> // could "fork" Mailspring and re-use it's Client ID and Secret. For now, it
> // seems we're on the honor code - Please don't do this.
> ```

Their secret is AES-obfuscated with a key that sits three lines above it in the
same file.

### The 100-user cap, precisely

> "the user cap applies over the entire lifetime of the project, and it cannot
> be reset or changed."
>
> — [FAQ](https://support.google.com/cloud/answer/13463817)

It is 100 *new* users total, counted from the first time the unverified-app
screen is shown, per Cloud project
([unverified apps](https://support.google.com/cloud/answer/7454865)). Not
concurrent, not annual, not resettable.

For one shared project that number ends the idea. For the current design it does
not bind, because each user has their own project and spends the cap on their
own accounts. The README already describes this correctly.

Verification is not required at that scale either:

> "If the app is for your personal use (**fewer than 100 users)**, you and your
> limited number of users can continue using the app"
>
> — [When is verification not needed](https://support.google.com/cloud/answer/13464323)

The other exemptions are development/testing/staging, service-account-only apps,
Workspace-internal apps, and admin-installed domain-wide apps. There is nothing
for open source, nothing for native desktop apps, nothing for single developers.

One part of the setup publishing would not fix: the 7-day refresh token expiry
belongs to the **Testing** publishing status, not to being unverified. "A Google
Cloud Platform project with an OAuth consent screen configured for an external
user type and a publishing status of 'Testing' is issued a refresh token
expiring in 7 days"
([OAuth 2.0 overview](https://developers.google.com/identity/protocols/oauth2)).
Publishing to production clears it, and `skills/mach-setup/SKILL.md` step 7
already does that.

### Answer

**No.**

The bill: a privacy policy, a demo video, six weeks of review, an annual lab
assessment at a price nobody publishes, and re-verification whenever the app's
configuration changes.

The purchase: no unverified-app interstitial, and no Google Cloud setup in the
install.

Against that, a published Mach hands out a client secret it cannot keep, puts
every user on one shared quota, and makes one person answerable for whatever is
done with an extracted credential. The site's existing line — "I'm not putting a
personal mail client through Google's verification" — survives the research.

The interstitial costs one click per account. The Google Cloud setup is the
expensive half, and it can be shortened without any of this. See the
recommendation.

## Question 2 — dev ergonomics and shipping in comparable projects

Repositories read for this section, all fetched and confirmed active:
[gitbutlerapp/gitbutler](https://github.com/gitbutlerapp/gitbutler),
[spacedriveapp/spacedrive](https://github.com/spacedriveapp/spacedrive),
[CapSoftware/Cap](https://github.com/CapSoftware/Cap),
[mountain-loop/yaak](https://github.com/mountain-loop/yaak),
[clash-verge-rev/clash-verge-rev](https://github.com/clash-verge-rev/clash-verge-rev),
[rustdesk/rustdesk](https://github.com/rustdesk/rustdesk),
[pot-app/pot-desktop](https://github.com/pot-app/pot-desktop) (Tauri);
[Foundry376/Mailspring](https://github.com/Foundry376/Mailspring),
[laurent22/joplin](https://github.com/laurent22/joplin) (Electron);
[vercel/turborepo](https://github.com/vercel/turborepo),
[nushell/nushell](https://github.com/nushell/nushell),
[DioxusLabs/dioxus](https://github.com/DioxusLabs/dioxus) as reference points.

### Nobody hot-reloads Rust

Tauri's own documentation: "`tauri dev` watches your `src-tauri` folder and its
dependent crates in the workspace for changes, so your application is
automatically rebuilt and restarted whenever you modify them"
([develop](https://v2.tauri.app/develop/)). The only escape hatch is
`--no-watch`.

[Dioxus `subsecond`](https://github.com/DioxusLabs/dioxus/tree/main/packages/subsecond)
is the one working Rust hot-patcher, and it requires wrapping call sites in
`subsecond::call(…)`. None of the Tauri apps above uses it. None uses
`cargo-watch` or `bacon` for the app loop either.

What they do instead is cut the rebuild down with cargo profiles. Mach's
`src-tauri/Cargo.toml` has **no `[profile.dev]` section at all** and no
`.cargo/config.toml` beyond the generated `target-dir` line from
`bin/worktree-setup`. A one-line touch of `src/main.rs` and `cargo build` takes
**32.5 seconds** here against a warm cache and 440 dependencies.

Spacedrive's block is the most complete, and its comment says what it is for:

```toml
# Make compilation faster on macOS
[profile.dev]
codegen-units   = 256
debug           = 0
incremental     = true
lto             = false
opt-level       = 0
split-debuginfo = "unpacked"
strip           = "none"

[profile.dev.build-override]
opt-level = 3

[profile.dev.package."*"]
incremental = false
opt-level   = 3

[profile.dev-debug]        # keep a slow profile for when you need a debugger
inherits = "dev"
debug    = "full"
```

GitButler's is smaller and goes the other way on one knob:

```toml
[profile.dev]
incremental = false
debug = "line-tables-only"

[profile.dev.package]
gix = { opt-level = 3 }
serde = { opt-level = 3 }
# ~20 more hot crates, including one of their own workspace crates
```

Cap's is two lines: `[profile.dev] debug = 1` with `[profile.dev.package."*"]
debug = 0`. RustDesk ships `debug = 1` and nothing else. Yaak (18.9k stars) and
clash-verge-rev and pot-desktop ship no dev profile at all.

The knob every one of them touches is debug info. Dropping it for dependencies
is the cheapest macOS link-time win and carries no toolchain risk.

Nobody in this set uses a faster linker on macOS. Spacedrive's generated
`.cargo/config.toml` sets `lld` on Windows and Linux and leaves macOS on the
stock linker. Nushell's mold recipe for macOS exists only as commented-out
documentation. The one project running `-Zshare-generics=y`, `-Zthreads=8` and
`linker = "rust-lld"` on macOS is Turborepo, and it pays for that with a pinned
nightly toolchain. No desktop app here was willing to.

Two structural tricks worth more than the profile edits:

- **A Rust-only loop that never launches the app.** GitButler's
  `DEVELOPMENT.md` opens with a CLI-only section — `cargo run -p but -- status`
  — framed as the easiest way to get started, so most backend work never pays
  for a Tauri launch. Mach already has the equivalent in
  `cargo run --example oauth_smoke` and `cargo test`, and `bin/worktree-setup`
  already tells you to run `bun run dev` against the built binary for frontend
  work.
- **Separate target directories per mode.** GitButler sets
  `CARGO_TARGET_DIR=…/target/tauri` for `tauri dev` so the CLI build and the app
  build stop invalidating each other. Given how the shared target dir has
  already blown up disk here, that one is a trade rather than a win.

Only Cap wires up sccache, and it is opt-in and off by default: `scripts/setup.js`
generates `.cargo/config.toml` with an `rustc-wrapper` line only when both
`sccache` is on PATH and `CAP_USE_SCCACHE=1` is set.

### Credentials in dev versus in a shipped build

Nobody reads a `.env` at runtime from the shipped `.app`. Four strategies:

**Bake a constant with an env override.** Mailspring, quoted above:
`process.env.MS_GMAIL_CLIENT_ID || '662287800555-….apps.googleusercontent.com'`.
Contributors override, the shipped build falls back to the maintainer's.

**Commit plaintext credentials per environment.** Joplin's
`packages/lib/parameters.ts` holds OneDrive and Dropbox ids and secrets for
`dev`, `prod` and `test` in source with no obfuscation, selected by
`electron . --env dev`. Its `onedrive-api.ts` explains why they don't care:
"Mobile and desktop apps are considered 'public'", so the secret is not sent.

**Bake at compile time through `option_env!`.** Cap's Rust reads
`option_env!("VITE_OPENPANEL_CLIENT_ID")` into an `Option<&str>`, with a comment
saying "Baked in at compile time so self-built binaries send nothing." Dev runs
under `dotenv -e ../../.env -- pnpm tauri dev`; CI writes `.env` from repository
secrets immediately before `tauri build`. The `Option` shape matters: a fork
that builds without the secret gets `None` and can degrade rather than carry
somebody else's credential.

**Keep no credential in the client.** GitButler's committed `.env.production`
holds only endpoints and a write-only analytics key. The OAuth client lives on
`app.gitbutler.com` and the desktop app receives a token back. This costs a
server, which is the whole reason it is not an option for Mach.

None of these is what Mach needs, because all four assume the maintainer's
credentials ship. The gap that `RELEASING.md` names — a packaged build that can
be *told* a client id and secret by its user — is not represented in this prior
art, because everyone else went through verification instead.

### Signing dev builds, and the Keychain prompt

**None of these projects signs its dev builds.** No `signingIdentity` appears in
the `tauri.conf.json` of GitButler, Cap, Spacedrive, Yaak, clash-verge-rev or
pot-desktop; `APPLE_SIGNING_IDENTITY` is set in CI only. Cap ad-hoc signs
(`-s "-"`) its vendored FFmpeg and ONNX dylibs in `scripts/setup.js`, because
macOS 13+ will not load unsigned dylibs from a bundle — an ad-hoc signature
changes with the binary, so this does nothing for Keychain ACLs.

The rebuild-prompt problem is real, and GitButler names it in
`crates/but-secret/src/secret.rs`:

```rust
// In debug builds, use a git-credential based implementation when platform keychains are
// either noisy (macOS rebuild prompts) or unavailable (headless e2e Linux containers).
// Release builds keep using the platform keychain.
if cfg!(debug_assertions)
    && (cfg!(target_os = "macos") || std::env::var_os("E2E_TEST_APP_DATA_DIR").is_some())
{
    git_credentials::setup().ok();
}
```

They implement `keyring`'s `CredentialApi` over `gix::credentials`, described in
the same file as "useful on Systems that nag the user with popups if the
underlying binary changes". They stack two more mitigations: a distinct dev
bundle identifier (`com.gitbutler.app.dev`, `productName: "GitButler Dev"`) and
a build-kind prefix on every Keychain handle. Their open issue
[#10784](https://github.com/gitbutlerapp/gitbutler/issues/10784) is a user
pointing out the keys they forgot to namespace.

The Electron projects avoid the prompt by collapsing N items into one.
Mailspring's `app/src/key-manager.ts`: "Consolidating this prevents a ton of key
authorization popups for each and every key we want to access" — everything goes
into a single `safeStorage`-encrypted blob. Joplin does the same, writing the
ciphertext into its own SQLite store so the OS Keychain holds only Electron's
master key.

So there are three known answers and Mach uses a fourth. `scripts/sign` — a
stable self-signed identity with a fixed identifier, so the ACL's designated
requirement keeps matching — is not something any of these projects tried. It is
the only approach that keeps the real Keychain in the dev loop. Whether the
prompt fires differently for unsigned versus ad-hoc versus stably-signed dev
binaries is unverified in the prior art; nobody tested it, they all sidestepped
it.

Only Mailspring uses `keychain-access-groups`, and `app/build/build.js` shows
the bill: an Apple provisioning profile embedded at
`Contents/embedded.provisionprofile`, plus a split entitlements scheme, because
"amfid will reject any helper that carries restricted entitlements it cannot
match to a profile scoped to that binary." Their dev builds get no signing and
no entitlements at all.

### Shipping

| Project | macOS artifact | Updater | Where it lives |
|---|---|---|---|
| GitButler | notarized DMG | Tauri updater, minisign | own S3 + own release API |
| Cap | notarized DMG | Tauri updater | CrabNebula Cloud (paid) |
| Spacedrive | notarized DMG + `.app` | Tauri updater | GitHub Releases |
| Yaak | notarized | Tauri updater, behind a cargo feature | GitHub Releases, Flathub, npm |
| clash-verge-rev | signed + notarized | Tauri updater | GitHub Releases |
| pot-desktop | signed + notarized | Tauri updater | GitHub Releases |
| Mailspring | signed + notarized ZIP | Squirrel.Mac (unverified) | own S3 |
| Joplin | DMG + PKG + ZIP | electron-updater (unverified) | GitHub Releases |

**Nobody uses Sparkle.** Every Tauri project uses the built-in updater with a
minisign keypair, the public key committed to `tauri.conf.json` and the private
key in `TAURI_SIGNING_PRIVATE_KEY`. Tauri supports a static JSON manifest with
no server: "When using static, you just need to return a JSON containing the
required information"
([updater plugin](https://v2.tauri.app/plugin/updater/)). That is the cheapest
updater story available, and it is roughly one file per release.

The floor for a notarized build is small. Spacedrive's and Yaak's whole macOS
release job is `tauri-apps/tauri-action@v0` plus
`apple-actions/import-codesign-certs`, six repository secrets, and about forty
lines. Mach's `.github/workflows/release.yml` is already at that shape and is
already gated on the certificate secrets being present.

GitButler's trick worth copying: release-profile tuning applied through the
environment rather than committed, so local `--release` stays fast and only the
shipped build pays.

```yaml
env:
  CARGO_PROFILE_RELEASE_LTO: fat
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS: 1
  CARGO_PROFILE_RELEASE_OPT_LEVEL: s
```

Cap's workflow documents what fat LTO then costs: the final link "peaks well
above the 16GB stock Linux runner and gets OOM-killed", so they allocate a 16G
swapfile, and macOS runs on paid `macos-latest-xlarge` runners.

All seven casks are community-maintained in `Homebrew/homebrew-cask` rather than
in the projects' own taps, and `auto_updates true` makes them near-zero
maintenance once merged. Mach cannot get one yet: Homebrew requires "at least 30
forks, 30 watchers or 75 stars", rising to "at least 90 forks, 90 watchers or
225 stars for a self-submission by the repository owner", and "A code repository
less than 30 days old is normally not eligible"
([package acceptance policy](https://github.com/Homebrew/brew/blob/master/docs/Package-Acceptance-Policy.md)).
`bborn/mach` is six days old with 4 stars and 0 forks.

## What Mach should do next

**1. Let a packaged build receive an OAuth client.** This is the one that
matters. It unblocks the release described in `RELEASING.md`, and it keeps every
property that makes publishing unnecessary: each user brings their own project,
their own quota, their own 100-user cap. A first-run sheet writing to the
Keychain fits the existing `auth::tokens::KeychainTokenStore` and the
`keychain_service()` namespacing, and `AppConfig::from_values` already treats
missing credentials as a state rather than a crash, so the plumbing exists.
Keep `.env.local` under `debug_assertions` as the dev path.

Do **not** use Cap's `option_env!` bake for the Google client. It works, but for
Mach it would mean shipping Bruno's credentials with none of the verification.
That carries every cost of publishing and none of its benefit: the secret is
still extractable, the quota is still shared, and the unverified-app
interstitial still appears.

**2. Add a `[profile.dev]` block.** Nothing here is exotic and none of it needs
a nightly toolchain:

```toml
[profile.dev]
debug = "line-tables-only"
split-debuginfo = "unpacked"

[profile.dev.package."*"]
debug = 0
opt-level = 3

[profile.dev.build-override]
opt-level = 3
```

Measure against the 32.5s baseline above. Spacedrive's `dev-debug` companion
profile is the right escape hatch if a debugger ever needs full symbols.

**3. Buy the Apple Developer Program before anything else.** $99/year moves the
download from "Apple could not verify Mach is free of malware" to "downloaded
from the Internet, are you sure", which is the dialog users actually get past.
`RELEASING.md` already documents the five secrets and the workflow already gates
on them. Compared with an unpriced annual lab assessment, this is the cheap
purchase that removes a real install failure.

**4. Leave the Homebrew cask alone.** The repo is six days old and 221 stars
short of the self-submission threshold. Revisit after 30 days and some traction.

**5. On the Keychain prompt** — `scripts/sign` is a better answer than anything
in the prior art, and its weakness is that it needs a one-time manual trust step
on each machine. GitButler's `cfg!(debug_assertions)` backend swap needs no
certificate and no setup, at the cost of dev builds no longer exercising the
real Keychain path. If the current fix proves fragile for contributors, that is
the fallback.
