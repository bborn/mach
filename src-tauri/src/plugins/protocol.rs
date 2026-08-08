//! The `plugin://` custom protocol — the origin a plugin lives on.
//!
//! Two static files, three response headers, and one decision worth writing
//! down: **the Content-Security-Policy is a response header, not a `<meta>`
//! tag.** The policy is then a property of the transport rather than of a file,
//! and — more importantly — the conformance probe is exercising the mechanism
//! the design actually relies on. A `<meta>` tag in `guest.html` would mask a
//! WebView that ignores the header, and a sandbox that passes its own test for
//! the wrong reason is worse than one that fails it.
//!
//! # Why one origin per plugin
//!
//! The host component of the URL is the plugin id, so `quick-file` is served
//! from `plugin://quick-file/` and `snooze-until-free` from
//! `plugin://snooze-until-free/`. Storage in a WebView is partitioned by
//! origin, so this is what stops plugin A reading plugin B's `localStorage` —
//! the PoC found that one origin for every plugin is one storage partition for
//! every plugin. See `docs/plugin-poc/README.md`.
//!
//! # Platform limits, recorded rather than solved
//!
//! - **Windows** collapses custom schemes to `http://<scheme>.localhost/`, so
//!   the plugin id would become a *path*, not a host — every plugin would share
//!   one origin and one storage partition. Mach is macOS-only today; this must
//!   be checked before Windows ships, not after.
//! - **Linux and Android**: Tauri cannot reliably distinguish an iframe from
//!   the window itself, and the whole sandbox is an iframe. The isolation is
//!   weaker there, and the plugin list should say so rather than pretend.
//!
//! Both are recorded in [`PLATFORM_LIMITS`] so the UI can show them without a
//! second copy of the sentence.

use tauri::http::{Request, Response};

/// The guest's policy.
///
/// `connect-src 'none'` is the load-bearing directive: it is what removes
/// `fetch`, `XMLHttpRequest`, `WebSocket`, `EventSource` and `sendBeacon` in one
/// line. `script-src 'self' blob:` allows exactly the two things the loader
/// needs — the guest's own relay script, and blob URLs for the worker and the
/// plugin module — and nothing that could reach the network. There is no
/// `https:` anywhere, which is the point: a plugin is one pre-bundled file and
/// cannot pull in code later.
///
/// `child-src` is listed alongside `worker-src` because WebKit shipped
/// `worker-src` late and falls back to `child-src` without it; naming both costs
/// nothing and removes a silent difference between engines.
pub const GUEST_CSP: &str = "default-src 'none'; \
     script-src 'self' blob:; \
     worker-src blob:; \
     child-src blob:; \
     connect-src 'none'; \
     img-src 'none'; \
     style-src 'none'; \
     font-src 'none'; \
     media-src 'none'; \
     frame-src 'none'; \
     object-src 'none'; \
     base-uri 'none'; \
     form-action 'none'";

/// The sandbox flags on the iframe itself.
///
/// `allow-same-origin` is correct here and is *not* the combination
/// `docs/message-rendering-invariants.md` forbids. That rule is about content
/// served from **the app's own origin**, where the pair lets a frame reach app
/// storage and strip its own `sandbox` attribute. The guest is served from a
/// different origin, so the flag means "keep your own, foreign origin" rather
/// than "become us". It has to be granted, because an *opaque* origin cannot run
/// a worker at all: `blob:null/…` is not a fetchable script URL.
pub const GUEST_SANDBOX: &str = "allow-scripts allow-same-origin";

/// The scheme. `plugin://<id>/guest.html` on macOS.
pub const SCHEME: &str = "plugin";

/// Known weaknesses of the boundary, per platform. Shown in the plugin list.
pub const PLATFORM_LIMITS: &[(&str, &str)] = &[
    (
        "windows",
        "Windows collapses custom protocols to http://plugin.localhost/, so every \
         plugin would share one origin and one storage partition. Not shipped.",
    ),
    (
        "linux",
        "Tauri cannot reliably tell an iframe from the window itself on Linux and \
         Android, and the sandbox is an iframe. Isolation is weaker there. Not shipped.",
    ),
];

const GUEST_HTML: &str = include_str!("assets/guest.html");
const SANDBOX_JS: &str = include_str!("assets/sandbox.js");

/// The worker shim, handed to the guest over `postMessage` rather than served.
///
/// It is deliberately *not* reachable at a URL: the worker is created from a
/// `blob:` URL so that it inherits the guest document's policy container by
/// construction (HTML §7.1.7) instead of parsing its own headers. A worker
/// fetched over the network would be a second place the policy could differ.
pub const WORKER_JS: &str = include_str!("assets/worker.js");

/// The conformance plugin. Loaded like any other plugin, which is the point:
/// if the canary can be run at all, the channel it runs over is the real one.
pub const CANARY_JS: &str = include_str!("assets/canary.js");

/// Serve one request on the `plugin://` scheme.
///
/// Pure, and takes the request rather than a Tauri context, so the routing and
/// the headers are unit-testable without a WebView.
pub fn respond(request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = request.uri().path();
    match path {
        "/" | "/guest.html" => asset(GUEST_HTML, "text/html; charset=utf-8"),
        "/sandbox.js" => asset(SANDBOX_JS, "text/javascript; charset=utf-8"),
        _ => Response::builder()
            .status(404)
            .header("content-type", "text/plain; charset=utf-8")
            // The policy goes on every response, including the refusals: a
            // 404 body is still a document, and a document without the policy
            // is a document with the network.
            .header("content-security-policy", GUEST_CSP)
            .body(b"not found".to_vec())
            .expect("static 404 response"),
    }
}

fn asset(body: &'static str, content_type: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(200)
        .header("content-type", content_type)
        .header("content-security-policy", GUEST_CSP)
        .header("x-content-type-options", "nosniff")
        // No `access-control-allow-origin`. Nothing should be reading these
        // cross-origin, and saying so is free.
        .body(body.as_bytes().to_vec())
        .expect("static asset response")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(path: &str) -> Response<Vec<u8>> {
        let request = Request::builder()
            .uri(format!("plugin://quick-file{path}"))
            .body(Vec::new())
            .unwrap();
        respond(&request)
    }

    fn csp(response: &Response<Vec<u8>>) -> String {
        response
            .headers()
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn serves_the_guest_document() {
        let response = get("/guest.html");
        assert_eq!(response.status(), 200);
        assert!(String::from_utf8_lossy(response.body()).contains("sandbox.js"));
    }

    #[test]
    fn every_response_carries_the_policy() {
        for path in ["/", "/guest.html", "/sandbox.js", "/anything-else"] {
            assert!(
                csp(&get(path)).contains("connect-src 'none'"),
                "{path} was served without connect-src 'none'"
            );
        }
    }

    /// The whole security claim in one assertion: nothing in the policy names a
    /// network scheme, so there is no way to spell "reach a server".
    #[test]
    fn the_policy_names_no_network_scheme() {
        let policy = GUEST_CSP;
        for scheme in ["https:", "http:", "ws:", "wss:", "data:"] {
            assert!(!policy.contains(scheme), "GUEST_CSP allows {scheme}");
        }
        assert!(policy.contains("script-src 'self' blob:"));
        assert!(policy.contains("default-src 'none'"));
    }

    /// The guest must not carry its own policy: the header is the mechanism
    /// under test, and a meta tag would let a broken header pass unnoticed.
    #[test]
    fn the_guest_document_has_no_meta_policy() {
        assert!(!GUEST_HTML.contains("<meta http-equiv"));
    }

    #[test]
    fn unknown_paths_are_not_served() {
        assert_eq!(get("/../../etc/passwd").status(), 404);
        assert_eq!(get("/main.js").status(), 404);
    }
}
