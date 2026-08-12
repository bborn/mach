//! Where a QA instance loads its frontend from, decided at runtime.
//!
//! # `devUrl` was a compile-time constant, and that hurt twice
//!
//! `tauri.conf.json` says `"devUrl": "http://localhost:1420"`, and
//! `generate_context!` expands that into the binary. Two consequences, both of
//! which cost real time before this file existed.
//!
//! The first was expensive for the person using the machine. Port 1420 is where
//! the owner's own app loads from, and every instance built from that constant
//! wanted it. `scripts/qa up` would start a dev server from whatever checkout
//! invoked it if nothing was listening — so an agent working in a worktree
//! served its in-progress frontend into the window the owner was reading real
//! mail in. Twice in one day.
//!
//! The second was expensive for agents. Pointing an instance at another port
//! meant editing the config and rebuilding, and the rebuild did not happen:
//! `touch src-tauri/src/lib.rs` recompiles the crate but not the *context*, so
//! the binary kept the old URL. `touch src-tauri/build.rs` is also required,
//! which one agent worked out after losing a cycle to it. Until then the app
//! silently loaded the wrong frontend, which looks exactly like "my change did
//! not take".
//!
//! So the URL is an environment variable now, applied to the config before the
//! builder ever sees it. [`Manager::get_app_url`] reads
//! `config.build.dev_url` at window-creation time, and `Context::config_mut`
//! is the supported way to reach it — `shell::suppress_configured_window`
//! already mutates the same struct a line earlier.
//!
//! # It fails loudly, because the silent failure is the dangerous one
//!
//! A QA instance with no `MACH_DEV_URL`, or one pointing at 1420, exits before
//! it opens a window. Rendering somebody else's frontend is not a degraded
//! mode worth continuing in: the screenshots would be of the wrong code, and
//! the last time this happened nobody noticed for an unknown stretch.
//!
//! `main` — the owner's instance, no `MACH_DATA_DIR` — is left exactly as it
//! was: the compiled-in 1420, unless he sets `MACH_DEV_URL` himself.

/// The port the owner's app loads from. Off limits to every QA instance.
pub const OWNER_PORT: u16 = 1420;

/// Which checkout compiled this binary.
///
/// Every worktree shares the parent's target directory — a private one costs
/// several minutes and about 2GB per agent — and they all uplift their
/// artifact to the same `target/debug/mach`. Whoever built last owns that
/// name, so an agent can be screenshotting a window running somebody else's
/// Rust with nothing on screen to say so. One agent worked it out by grepping
/// the running binary for a symbol it did not recognise; there was no
/// cheaper way, because a debug build with `split-debuginfo=unpacked` carries
/// no path strings at all.
///
/// This is that symbol, put there on purpose: baked in at compile time,
/// printed at startup, and greppable from the outside — which is how
/// `scripts/qa` refuses to launch an instance from a binary this checkout did
/// not build.
pub const BUILT_FROM: &str = env!("CARGO_MANIFEST_DIR");

/// What `scripts/qa` sets, and what this module reads.
pub const DEV_URL_VAR: &str = "MACH_DEV_URL";

/// Apply `MACH_DEV_URL` to the context, or explain why we are not starting.
///
/// Returns `Err` with a message meant to be read in `.qa/<instance>/mach.log`
/// by somebody who has just watched `qa up` fail. The caller exits on it.
pub fn resolve(is_qa: bool, requested: Option<&str>) -> Result<Option<tauri::Url>, String> {
    let Some(raw) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        if is_qa {
            return Err(format!(
                "{DEV_URL_VAR} is not set, and a QA instance must not fall back to the \
                 compiled-in http://localhost:{OWNER_PORT} — that is the port the owner's \
                 own window loads from. Launch through `scripts/qa up`, which derives a \
                 dev-server port for this instance and serves it."
            ));
        }
        return Ok(None);
    };

    let url = tauri::Url::parse(raw)
        .map_err(|e| format!("{DEV_URL_VAR} is not a URL: {raw:?} ({e})"))?;

    // `port_or_known_default` rather than `port`, so a bare `http://localhost`
    // is understood as 80 rather than as "no port and therefore fine".
    if is_qa && url.port_or_known_default() == Some(OWNER_PORT) {
        return Err(format!(
            "{DEV_URL_VAR}={raw} points at port {OWNER_PORT}. That is the owner's dev \
             server, feeding the window he is reading mail in; a QA instance may never \
             load from it. `scripts/qa up` picks a port in 1430–1928 for each instance."
        ));
    }

    Ok(Some(url))
}

/// Read the environment, decide, and either write the config or exit.
///
/// Called from `run()` before the builder is constructed. Exits the process
/// rather than returning an error, because there is no caller above this that
/// could do anything more useful with one, and a half-started mail client with
/// the wrong frontend is the outcome being prevented.
pub fn apply(context: &mut tauri::Context) {
    // First line in the log, so "which code is this?" is answerable from
    // `qa logs` without anybody having to think of grepping a binary.
    eprintln!("mach: built from {BUILT_FROM}");

    let requested = std::env::var(DEV_URL_VAR).ok();
    match resolve(crate::shell::is_qa_instance(), requested.as_deref()) {
        Ok(None) => {}
        Ok(Some(url)) => {
            eprintln!("qa dev url: {url}");
            context.config_mut().build.dev_url = Some(url);
        }
        Err(why) => {
            eprintln!("mach: {why}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owners_instance_keeps_the_compiled_in_url() {
        assert_eq!(resolve(false, None), Ok(None));
        assert_eq!(resolve(false, Some("   ")), Ok(None));
    }

    /// The owner may still move his own dev server; nothing here stops him.
    #[test]
    fn the_owners_instance_may_still_be_pointed_somewhere() {
        let resolved = resolve(false, Some("http://localhost:1420")).expect("allowed");
        assert_eq!(resolved.map(|u| u.port()), Some(Some(1420)));
    }

    #[test]
    fn a_qa_instance_without_one_refuses_to_start() {
        let refusal = resolve(true, None).expect_err("must not fall back to 1420");
        assert!(refusal.contains("1420"), "{refusal}");
    }

    /// The property the whole file exists for.
    #[test]
    fn a_qa_instance_may_never_load_the_owners_port() {
        for pointed_at_the_owner in [
            "http://localhost:1420",
            "http://127.0.0.1:1420",
            "http://[::1]:1420/",
        ] {
            assert!(
                resolve(true, Some(pointed_at_the_owner)).is_err(),
                "{pointed_at_the_owner} must be refused"
            );
        }
    }

    #[test]
    fn a_derived_port_is_accepted() {
        let resolved = resolve(true, Some("http://localhost:1573")).expect("accepted");
        assert_eq!(resolved.expect("some").port(), Some(1573));
    }

    #[test]
    fn nonsense_is_a_refusal_rather_than_a_fallback() {
        assert!(resolve(true, Some("not a url")).is_err());
    }
}
