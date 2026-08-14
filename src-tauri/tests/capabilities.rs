//! The configuration files, tested as a security boundary.
//!
//! `capabilities/default.json` and the `csp` in `tauri.conf.json` are the two
//! places where the webview's reach into the machine is decided, and both are
//! JSON that no compiler checks. A one-word addition to either — `fs:default`,
//! `opener:allow-open-path`, `'unsafe-eval'` — is a silent, total change in
//! what a page in that window can do, and nothing else in the tree would fail.
//!
//! So these tests are deliberately written as refusals rather than as a
//! description of the current file. Each one names a specific thing that must
//! not be granted and says what granting it would cost. Adding a permission the
//! app genuinely needs means adding it here too, with the reason — which is the
//! point.
//!
//! # Why the QA gate is checked from here
//!
//! [`the_qa_control_port_is_compiled_out_of_a_release_build`] reads `lib.rs` as
//! text, which is an unusual thing for a test to do. The alternative is nothing:
//! the gate is `#[cfg(debug_assertions)]`, tests only ever run *with*
//! `debug_assertions`, so no ordinary test can observe its absence. A loopback
//! port that drives the app is worth one ugly test.

use std::path::PathBuf;

use tauri::utils::acl::APP_ACL_KEY;

fn src_tauri() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path: PathBuf = src_tauri().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn json(relative: &str) -> serde_json::Value {
    serde_json::from_str(&read(relative)).expect("valid JSON")
}

fn granted_permissions() -> Vec<String> {
    json("capabilities/default.json")["permissions"]
        .as_array()
        .expect("permissions is an array")
        .iter()
        .map(|value| value.as_str().expect("a permission is a string").to_string())
        .collect()
}

fn csp() -> String {
    json("tauri.conf.json")["app"]["security"]["csp"]
        .as_str()
        .expect("a csp is configured")
        .to_string()
}

// ===========================================================================
// What JavaScript may ask Rust for
// ===========================================================================

/// The whole grant, listed. A diff on this test is the review.
///
/// Every entry has to be something the frontend actually calls:
///
/// * `mailbox` — Mach's own commands, defined in `permissions/mach.toml`. No
///   prefix, so it resolves against the app's own ACL manifest rather than a
///   plugin's. It grants nothing that was not already reachable: before that
///   file existed the app had no ACL manifest at all, which meant Tauri skipped
///   the ACL for every local-origin invoke and ran the handler. Naming the
///   commands is what makes the ACL authoritative — see the file's own header,
///   and `tests/browser.rs::the_page_window_is_refused_on_both_grounds`.
/// * `plugin-probe` — `probe_log`, for the separate `plugin_probe` binary,
///   which builds from the same `generate_context!` and therefore the same two
///   directories. The mailbox binary registers no such handler.
/// * `core:default` — window/webview/app introspection, `path`, `event`. All
///   read-only; it contains no way to create a window, navigate one, or touch
///   a file.
/// * `core:event:default` — `listen`/`emit`, for the three push channels.
///   Already inside `core:default`; named again because losing it silently
///   would break sync status with no error.
/// * `opener:allow-open-url` + `opener:allow-default-urls` — `openExternal`,
///   scoped by the second entry to `mailto:`, `tel:`, `http:` and `https:`.
#[test]
fn the_window_is_granted_exactly_these_and_nothing_else() {
    let mut granted = granted_permissions();
    granted.sort();
    assert_eq!(
        granted,
        vec![
            "core:default",
            "core:event:default",
            "mailbox",
            "opener:allow-default-urls",
            "opener:allow-open-url",
            "plugin-probe",
        ],
        "the capability file changed — every entry needs a reason in this test"
    );
}

/// The app ACL manifest exists, which is the thing `has_app_acl` is asking
/// about.
///
/// One boolean, deep inside `tauri::webview::Webview::on_message`, decides
/// whether an invoke from a *local* origin is checked against the ACL at all:
///
/// ```text
/// if (plugin_command.is_some() || has_app_acl_manifest || !is_local)
///     && invoke.acl.is_none() { reject }
/// ```
///
/// `has_app_acl_manifest` is `acl.contains_key("__app-acl__")`, and that key
/// appears only when `tauri-build` finds permission files under
/// `src-tauri/permissions/`. Delete that directory and every window in Mach —
/// including `browser::WINDOW_LABEL` — gets its app commands run unchecked at a
/// local origin again.
///
/// Deleting it outright does not compile: `capabilities/default.json` names
/// `mailbox`, and `tauri_build`'s `validate_capabilities` fails the build for a
/// permission that resolves to nothing. This test is for the way round that
/// *does* compile — the capability entry and the directory going together, in
/// one tidy-looking commit — which would leave the app working, every other
/// test passing, and the gate open.
#[test]
fn the_app_defines_an_acl_manifest_so_the_acl_is_never_skipped() {
    let manifests = json("gen/schemas/acl-manifests.json");
    let app = manifests.get(APP_ACL_KEY).unwrap_or_else(|| {
        panic!(
            "no {APP_ACL_KEY} in the generated manifests. src-tauri/permissions/ \
             is what produces it, and without it Tauri consults no ACL for a \
             local-origin invoke in any window."
        )
    });
    assert!(
        !app["permissions"]
            .as_object()
            .expect("the app manifest lists permissions")
            .is_empty(),
        "an app manifest with no permissions grants nothing to the mailbox either"
    );
}

/// The permission list and the handler list, compared both ways.
///
/// With an app ACL manifest in place these two have to agree exactly, and
/// nothing else in the build makes them. The two failures are different and
/// both are silent:
///
///  * a command in `generate_handler!` that no permission names is **refused at
///    runtime** — the mailbox calls it, Tauri answers "not allowed by ACL", and
///    the compiler had nothing to say about it;
///  * a command named by a permission that no longer exists is a grant pointing
///    at nothing, which is how a list stops describing the program.
///
/// `probe_log` is the one deliberate asymmetry, and it is named here rather
/// than tolerated by a wildcard: it belongs to `src/bin/plugin_probe.rs`, a
/// second binary that builds from the same `generate_context!` and therefore
/// needs its command in the same manifest.
#[test]
fn every_command_the_app_registers_is_named_here() {
    let manifests = json("gen/schemas/acl-manifests.json");
    let permissions = manifests[APP_ACL_KEY]["permissions"]
        .as_object()
        .expect("the app manifest lists permissions")
        .clone();

    let mut granted: Vec<String> = permissions
        .values()
        .flat_map(|permission| {
            permission["commands"]["allow"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .map(|value| value.as_str().expect("a command is a string").to_string())
        .collect();
    granted.sort();

    let mut registered = registered_commands();
    // The probe binary's, and only it. See the doc above.
    registered.push("probe_log".to_string());
    registered.sort();

    let missing: Vec<_> = registered.iter().filter(|c| !granted.contains(c)).collect();
    assert!(
        missing.is_empty(),
        "these commands are registered but no permission names them, so the \
         mailbox is refused when it calls them: {missing:?}. Add them to \
         src-tauri/permissions/mach.toml."
    );

    let stale: Vec<_> = granted.iter().filter(|c| !registered.contains(c)).collect();
    assert!(
        stale.is_empty(),
        "these commands are granted but nothing registers them any more: \
         {stale:?}. Remove them from src-tauri/permissions/mach.toml."
    );
}

/// The commands `lib.rs` hands to `generate_handler!`, read out of the source.
///
/// From the handler rather than from a second list, because the drift between
/// the two is the whole failure being guarded against.
fn registered_commands() -> Vec<String> {
    let source = read("src/lib.rs");
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

/// The one that would undo `attachments::names::is_dangerous` completely.
///
/// `opener:allow-open-path` lets JavaScript hand any path on the disk to
/// LaunchServices. Every check in `ipc::attachments::attachment_open` — the
/// extension list, the declared type, the byte sniff — exists because that
/// command is the only way to open a file, and this permission would add a
/// second way with no checks at all.
#[test]
fn javascript_cannot_ask_the_system_to_open_a_path() {
    let granted = granted_permissions();
    assert!(
        !granted.iter().any(|p| p.contains("open-path")),
        "opener:allow-open-path would let JS open any file, bypassing every \
         check in attachment_open: {granted:?}"
    );
    // `opener:default` is not a shorthand for the two entries the app uses — it
    // also carries `allow-reveal-item-in-dir`, which nothing calls.
    assert!(
        !granted.iter().any(|p| p == "opener:default"),
        "opener:default grants reveal-item-in-dir, which no code path uses"
    );
    assert!(
        !granted.iter().any(|p| p.contains("reveal-item")),
        "reveal-item-in-dir is granted but unused: {granted:?}"
    );
}

/// Two plugins are registered in `lib.rs` and neither is reachable from a page.
///
/// `tauri_plugin_dialog` is registered so `attachment_save` can put up the
/// system save panel from Rust; `tauri_plugin_notification` is wrapped by
/// `notify::host`. Granting either to JavaScript would turn "save this
/// attachment" into "open a file picker for anything", and "the sync loop may
/// post a banner" into "any script may".
#[test]
fn the_dialog_and_notification_plugins_are_reachable_only_from_rust() {
    let granted = granted_permissions();
    for plugin in ["dialog:", "notification:"] {
        assert!(
            !granted.iter().any(|p| p.starts_with(plugin)),
            "{plugin} is registered in Rust for one caller and must not be \
             reachable from a page: {granted:?}"
        );
    }
}

/// The capability families that are not in the build at all, asserted anyway.
///
/// None of these plugins is a dependency today, so none of these strings could
/// resolve. The test is about the day one of them is added for one small
/// reason and the default permission set comes with it.
#[test]
fn nothing_grants_the_filesystem_a_shell_or_a_raw_http_client() {
    let granted = granted_permissions();
    for family in ["fs:", "shell:", "http:", "process:", "updater:", "os:", "store:"] {
        assert!(
            !granted.iter().any(|p| p.starts_with(family)),
            "{family} must not be granted to the webview: {granted:?}"
        );
    }
}

/// The capability applies to one window, by name. A capability with no
/// `windows` key applies to every window there will ever be.
#[test]
fn the_grant_is_scoped_to_the_main_window() {
    let windows = json("capabilities/default.json")["windows"]
        .as_array()
        .expect("windows is listed explicitly")
        .clone();
    assert_eq!(windows, vec![serde_json::json!("main")]);
}

/// Every capability file in the directory, not just the one this test knows
/// about.
///
/// Tauri loads `capabilities/*.json` as a directory. A second file added later
/// would be picked up silently, and if it omitted `windows` — or wrote `"*"` —
/// it would apply to the page window too. So the invariant is asserted over the
/// whole directory rather than over one filename.
fn capability_files() -> Vec<(String, serde_json::Value)> {
    let dir = src_tauri().join("capabilities");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("capabilities/ exists") {
        let path = entry.expect("a directory entry").path();
        let is_capability = path
            .extension()
            .is_some_and(|e| e == "json" || e == "json5" || e == "toml");
        if !is_capability {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&path).expect("readable");
        // Only JSON is parsed here; a json5 or toml capability would need a
        // parser this crate does not have, so its presence is the failure.
        let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("{name} is not JSON this test can read ({e}) — it must be, or this file stops guarding anything")
        });
        out.push((name, value));
    }
    assert!(!out.is_empty(), "no capability files found");
    out
}

/// The page window's grant is empty, and it is empty by *absence*.
///
/// `browser::WINDOW_LABEL` is the window that renders a page a stranger chose,
/// with that stranger's JavaScript running in it. Tauri resolves an invoke's
/// permissions by window label, so the whole isolation is that no capability
/// mentions this one. There is nothing to revoke and nothing to get wrong at
/// runtime — the grant is the empty set because no file names it.
///
/// Three ways that could quietly stop being true, all asserted:
///
///  * a capability with no `windows` key, which applies to every window;
///  * a glob — `"*"`, `"mach-*"`, `"?ain"` — that happens to match;
///  * the label itself being added to a list.
///
/// `tests/browser.rs` asserts the consequence — an invoke from that window is
/// refused — through Tauri's own gate.
#[test]
fn no_capability_reaches_the_page_window() {
    let label = mach_lib::browser::WINDOW_LABEL;

    for (name, capability) in capability_files() {
        let windows = capability
            .get("windows")
            .and_then(|w| w.as_array())
            .unwrap_or_else(|| {
                panic!(
                    "{name} has no `windows` key, so it applies to every window \
                     Mach will ever open — including the one that runs a sender's script"
                )
            });

        for entry in windows {
            let pattern = entry.as_str().expect("a window label is a string");
            assert!(
                !pattern.contains(['*', '?', '[']),
                "{name} scopes a capability with the glob {pattern:?}. Globs are \
                 matched against every window label, and {label:?} must match none — \
                 write the labels out."
            );
            assert_ne!(
                pattern, label,
                "{name} grants permissions to the page window, which runs script \
                 written by whoever sent the mail"
            );
        }

        // `webviews` narrows a grant further; it can never widen one past
        // `windows`, but a file that used it without `windows` would be the
        // case above. Asserted so the shape stays one this test understands.
        assert!(
            capability.get("remote").is_none(),
            "{name} names remote origins, which would let a page invoke commands"
        );
    }
}

/// The two labels Mach opens a window under, and they are different strings.
///
/// A copy-paste that gave the page window the label `"main"` would hand it the
/// entire capability file, and every test above would still pass.
#[test]
fn the_page_window_is_not_the_main_window() {
    assert_ne!(
        mach_lib::browser::WINDOW_LABEL,
        "main",
        "the page window would inherit capabilities/default.json"
    );
    assert_eq!(
        mach_lib::shell::MAIN_WINDOW, "main",
        "capabilities/default.json names this label literally"
    );
}

/// No `remote` key. In Tauri 2 a capability may name remote origins that are
/// allowed to invoke commands; without one, only the app's own origin can.
#[test]
fn no_remote_origin_may_invoke_a_command() {
    let capability = json("capabilities/default.json");
    assert!(
        capability.get("remote").is_none(),
        "a remote origin would be able to call every command in the handler"
    );
}

// ===========================================================================
// The Content-Security-Policy
// ===========================================================================

/// Scripts come from the bundle and nowhere else.
///
/// `'unsafe-eval'` matters more here than in an ordinary web page: a message
/// body is rendered into a `srcdoc` frame, and an `about:srcdoc` document
/// inherits its creator's policy container. So this policy is *also* part of
/// the policy applied to sender HTML, and every relaxation of it is a
/// relaxation there.
#[test]
fn scripts_come_from_the_bundle_and_may_not_be_built_from_strings() {
    let policy = csp();
    assert!(
        policy.contains("script-src 'self'"),
        "script-src must be spelled out rather than inherited: {policy}"
    );
    for forbidden in ["'unsafe-eval'", "'unsafe-inline' 'self'", "script-src *"] {
        assert!(
            !policy.contains(forbidden),
            "the policy allows {forbidden}: {policy}"
        );
    }
    // `style-src` needs `'unsafe-inline'` — React writes inline styles — and
    // that is the only directive allowed to have it.
    let script_directive = policy
        .split(';')
        .find(|d| d.trim().starts_with("script-src"))
        .unwrap_or_default()
        .to_string();
    assert!(
        !script_directive.contains("unsafe-inline"),
        "script-src must not carry unsafe-inline: {script_directive}"
    );
}

/// The three directives that do **not** fall back to `default-src`.
///
/// A policy with `default-src 'self'` and nothing else leaves `base-uri`,
/// `form-action` and `object-src` unrestricted, which is not obvious from
/// reading it. `form-action` is the one with teeth: a phishing form inside a
/// message body would otherwise be free to POST the owner's typing to whoever
/// sent it.
#[test]
fn the_directives_that_do_not_inherit_from_default_src_are_set() {
    let policy = csp();
    for directive in ["base-uri 'self'", "form-action 'none'", "object-src 'none'"] {
        assert!(
            policy.contains(directive),
            "missing {directive}, which does not fall back to default-src: {policy}"
        );
    }
}

/// The asset protocol is what would make the attachment cache reachable by a
/// URL the webview could construct: `asset://localhost/<any path>` reads a file
/// off the disk. It is disabled by omission, which is easy to undo by accident
/// and impossible to notice, so it is asserted.
#[test]
fn the_asset_protocol_is_not_enabled_so_the_cache_has_no_url() {
    let security = json("tauri.conf.json")["app"]["security"].clone();
    assert!(
        security.get("assetProtocol").is_none(),
        "assetProtocol would give every cached attachment a URL the page can build"
    );
    assert!(
        !csp().contains("asset:"),
        "the policy names asset:, which reads as though the cache were served"
    );
}

/// `connect-src` is the app's own origin and the IPC pseudo-origins. Widening
/// it is how the frontend would gain the ability to talk to a server directly,
/// and the invariant in `CLAUDE.md` — the network is a background loop in Rust
/// — depends on it not being able to.
#[test]
fn the_frontend_has_no_network_of_its_own() {
    let policy = csp();
    let connect = policy
        .split(';')
        .find(|d| d.trim().starts_with("connect-src"))
        .expect("connect-src is set")
        .trim()
        .to_string();
    assert_eq!(connect, "connect-src 'self' ipc: http://ipc.localhost");
    assert!(!connect.contains("https:"), "{connect}");
    assert!(!connect.contains('*'), "{connect}");
}

// ===========================================================================
// The QA control port
// ===========================================================================

/// The gate that keeps a loopback port that can drive the app out of a shipped
/// binary is one attribute, and no test that runs can observe it — tests are
/// built with `debug_assertions` on, so the module is always present while
/// testing. Reading the source is the only way to assert it is still there.
#[test]
fn the_qa_control_port_is_compiled_out_of_a_release_build() {
    let lib = read("src/lib.rs");
    let declaration = lib
        .find("pub mod qa;")
        .expect("the qa module is still declared in lib.rs");
    let before = &lib[..declaration];
    assert!(
        before.trim_end().ends_with("#[cfg(debug_assertions)]"),
        "`pub mod qa;` must be immediately preceded by #[cfg(debug_assertions)] — \
         without it, a release build contains a loopback port that drives the app"
    );

    // And every call site carries the same attribute, so removing one does not
    // leave the module compiled in to satisfy a reference.
    for call in ["qa::dev::apply", "qa::install"] {
        let at = lib.find(call).unwrap_or_else(|| panic!("{call} is called"));
        let preceding = &lib[..at];
        assert!(
            preceding.contains("#[cfg(debug_assertions)]"),
            "{call} is not behind a debug_assertions gate"
        );
        let last_gate = preceding.rfind("#[cfg(debug_assertions)]").unwrap();
        assert!(
            preceding[last_gate..].lines().count() <= 3,
            "the gate nearest {call} is too far above it to be guarding it"
        );
    }
}

/// The frontend half. `connectQaBridge` is what turns a POST on that port into
/// a keystroke, and it is kept out of a production bundle by an
/// `import.meta.env.DEV` guard at its only call site, which Vite replaces with
/// `false` so the import tree-shakes away.
#[test]
fn the_frontend_qa_bridge_is_behind_a_dev_guard_at_its_call_site() {
    let app: PathBuf = src_tauri().join("../src/App.tsx");
    let source = std::fs::read_to_string(&app).expect("App.tsx");
    let at = source
        .find("connectQaBridge(")
        .expect("App.tsx still connects the bridge");
    let window = &source[at.saturating_sub(200)..at];
    assert!(
        window.contains("import.meta.env.DEV"),
        "connectQaBridge is reached without a DEV guard, so the bridge ships"
    );
}

// ===========================================================================
// The plugin origin
// ===========================================================================

/// `frame-src plugin:` is what lets the plugin host create its iframe. It must
/// not become a scheme that can reach the network, and the guest's own policy
/// — served as a response header by `plugins::protocol` — is what removes
/// `fetch` inside it.
#[test]
fn plugins_are_framed_from_their_own_scheme_and_nothing_else() {
    let policy = csp();
    let frame = policy
        .split(';')
        .find(|d| d.trim().starts_with("frame-src"))
        .expect("frame-src is set")
        .trim()
        .to_string();
    assert_eq!(frame, "frame-src plugin:");

    // The guest policy, asserted from here as well as from its own unit test,
    // because the two together are the claim: the app frames only `plugin:`,
    // and `plugin:` documents have no network.
    let guest = mach_lib::plugins::protocol::GUEST_CSP;
    assert!(guest.contains("connect-src 'none'"), "{guest}");
    for scheme in ["https:", "http:", "ws:", "wss:"] {
        assert!(!guest.contains(scheme), "the guest policy allows {scheme}");
    }
}

/// The `plugin://` handler serves two files. Everything else is a 404, which is
/// what stops a plugin id or a path being turned into a file read.
#[test]
fn the_plugin_origin_serves_two_files_and_refuses_every_other_path() {
    use tauri::http::Request;

    let respond = |path: &str| {
        let request = Request::builder()
            .uri(format!("plugin://quick-file{path}"))
            .body(Vec::new())
            .unwrap();
        mach_lib::plugins::protocol::respond(&request).status()
    };

    assert_eq!(respond("/guest.html"), 200);
    assert_eq!(respond("/sandbox.js"), 200);
    for hostile in [
        "/../../../etc/passwd",
        "/../../src-tauri/tauri.conf.json",
        "/main.js",
        "/index.html",
        "/../mach.db",
    ] {
        assert_eq!(
            respond(hostile),
            404,
            "{hostile} was served from the plugin origin"
        );
    }
}

// ===========================================================================
// Installing a plugin — the one place the app writes files out of a package
// ===========================================================================

/// The nearest thing this app has to unpacking an archive.
///
/// `PluginStore::install` writes two files: `mach-plugin.json` under a
/// directory named for the plugin's id, and the module under the name the
/// manifest's `main` field gives. Both of those names come out of a file
/// somebody was persuaded to point the installer at, which is the same shape
/// as zip-slip — a name inside a package deciding where a byte lands.
///
/// Two things stop it, and they are different things:
///
/// * the id is `[a-z0-9-]` with no dot and no separator, because it is also an
///   origin's host component, so `root.join(id)` is a single component by
///   construction;
/// * `main` is refused outright if it contains `..` or starts with `/`.
#[test]
fn a_plugin_manifest_cannot_name_a_file_outside_its_own_directory() {
    use mach_lib::plugins::manifest::{is_plugin_id, parse, InstallKind};

    // The id, which becomes a directory name.
    for hostile in [
        "../../../etc",
        "..",
        ".",
        "a/b",
        "a\\b",
        "a.b",
        "/absolute",
        "A-Capital",
        "",
        "-leading",
        "trailing-",
    ] {
        assert!(!is_plugin_id(hostile), "{hostile:?} was accepted as a plugin id");
    }
    for good in ["quick-file", "a", "snooze-until-free", "x9"] {
        assert!(is_plugin_id(good), "{good:?} was rejected");
        assert_eq!(
            std::path::Path::new(good).components().count(),
            1,
            "{good:?} is not one path component"
        );
    }

    // `main`, which becomes a file name inside that directory.
    let manifest = |main: &str| {
        format!(
            r#"{{"id":"quick-file","name":"Quick File","version":"1.0.0","machApi":"1",
                "main":"{main}",
                "capabilities":{{"commands":["archive"]}},
                "contributes":{{"actions":[{{"id":"file","title":"File"}}]}}}}"#
        )
    };
    for hostile in [
        "../../../../Library/LaunchAgents/evil.plist",
        "../main.js",
        "a/../../b.js",
        "/etc/crontab",
        "/tmp/evil.js",
    ] {
        let error = parse(&manifest(hostile), InstallKind::Published, &["archive"])
            .expect_err(&format!("{hostile:?} was accepted as main"));
        assert!(
            error.to_string().contains("leaves its own directory"),
            "{hostile:?}: {error}"
        );
    }
    assert!(parse(&manifest("main.js"), InstallKind::Published, &["archive"]).is_ok());
}

// ===========================================================================
// The plugin conformance gate
// ===========================================================================

/// Whether untrusted code may run is answered from the evidence, not from the
/// boolean the report arrived with.
#[test]
fn a_conformance_verdict_is_derived_from_its_own_rows() {
    use mach_lib::plugins::runtime::{ConformanceControl, ConformanceReport, ConformanceRow};

    let row = |allowed: bool| ConformanceRow {
        scope: "worker".into(),
        name: "fetch (remote)".into(),
        allowed,
        detail: String::new(),
    };
    let control = |succeeded: bool| {
        Some(ConformanceControl {
            name: "host page can fetch what the guest could not".into(),
            succeeded,
            detail: String::new(),
        })
    };
    let report = |rows: Vec<ConformanceRow>, control| ConformanceReport {
        ok: true,
        at: 0,
        app_origin: "http://localhost:1420".into(),
        guest_origin: "plugin://conformance".into(),
        rows,
        control,
        failures: vec![],
        error: None,
    };

    assert!(report(vec![row(false)], control(true)).evidence_supports_a_pass());

    // An escape that worked.
    assert!(!report(vec![row(true)], control(true)).evidence_supports_a_pass());

    // No control: every check in the probe is a negative, so an unplugged
    // network passes all of them and proves nothing.
    assert!(!report(vec![row(false)], None).evidence_supports_a_pass());
    assert!(!report(vec![row(false)], control(false)).evidence_supports_a_pass());

    // Nothing was attempted.
    assert!(!report(vec![], control(true)).evidence_supports_a_pass());

    // A named failure the rows do not otherwise show.
    let mut noted = report(vec![row(false)], control(true));
    noted.failures.push("the control failed".into());
    assert!(!noted.evidence_supports_a_pass());
}
