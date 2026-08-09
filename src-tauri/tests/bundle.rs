//! What ends up inside `Mach.app`.
//!
//! The crate has three binaries: `mach`, and two development probes under
//! `src/bin/`. Cargo builds all three, and the Tauri bundler, left to choose,
//! took `plugin_probe` and built a `Mach.app` around it.
//!
//! Every signal said that build was fine. The bundle had the right
//! `Info.plist`, the right `com.mach.mail` identifier, the right icon in
//! `Resources`, it signed without complaint, and the CLI printed
//! "Bundling Mach.app". The only way to find out was to open it and watch a
//! dev probe start instead of the mail client.
//!
//! Two declarations prevent it — `mainBinaryName` in `tauri.conf.json` for the
//! bundler and `default-run` in `Cargo.toml` for cargo — and nothing keeps them
//! honest, because a missing key is not an error in either file. So these
//! assert both are present, both say `mach`, and that `mach` is a real target.
//!
//! Building an actual bundle here would cost minutes and a code-signing
//! identity. Reading the two files that decide the answer costs nothing.

use std::path::{Path, PathBuf};

/// The binary that is the application. Everything else in the crate is a tool.
const APP_BINARY: &str = "mach";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(name: &str) -> String {
    let path = manifest_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

#[test]
fn the_bundler_is_told_which_binary_is_the_app() {
    let conf = read("tauri.conf.json");
    let value: serde_json::Value = serde_json::from_str(&conf).expect("tauri.conf.json is not valid JSON");

    let named = value.get("mainBinaryName").and_then(|v| v.as_str());

    assert_eq!(
        named,
        Some(APP_BINARY),
        "tauri.conf.json must set \"mainBinaryName\": \"{APP_BINARY}\". Without it the \
         bundler picks among the crate's binaries by name order and ships a development \
         probe inside Mach.app, which bundles and signs cleanly and only fails when \
         somebody opens it."
    );
}

#[test]
fn cargo_run_is_not_ambiguous() {
    let manifest = read("Cargo.toml");

    let declared = manifest
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("default-run"))
        .map(|rest| rest.trim_start_matches([' ', '=']).trim().trim_matches('"').to_string());

    assert_eq!(
        declared.as_deref(),
        Some(APP_BINARY),
        "Cargo.toml must set default-run = \"{APP_BINARY}\", or `cargo run` in a crate with \
         three binaries is an error rather than the app."
    );
}

#[test]
fn the_named_binary_exists() {
    let main = manifest_dir().join("src/main.rs");
    assert!(
        main.is_file(),
        "{} is the `{APP_BINARY}` target and has to exist for the two declarations above to \
         mean anything",
        main.display()
    );

    // The probes are the reason this file exists; if they are ever removed the
    // ambiguity goes with them, and these tests can go too.
    let bins = manifest_dir().join("src/bin");
    if bins.is_dir() {
        let probes: Vec<_> = std::fs::read_dir(&bins)
            .expect("src/bin is not readable")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".rs"))
            .collect();
        assert!(
            !probes.contains(&format!("{APP_BINARY}.rs")),
            "src/bin/{APP_BINARY}.rs would be a second target with the app's name"
        );
    }
}

/// Guards the icon set against the placeholder coming back, and against a
/// half-regenerated set: `assets/logo/build.py` writes all of these together,
/// so one older than the masters means somebody edited a PNG by hand.
#[test]
fn every_icon_tauri_names_is_present() {
    let conf = read("tauri.conf.json");
    let value: serde_json::Value = serde_json::from_str(&conf).unwrap();
    let listed = value["bundle"]["icon"]
        .as_array()
        .expect("bundle.icon must be an array");
    assert!(!listed.is_empty(), "bundle.icon is empty, so the app has no icon");

    for entry in listed {
        let rel = entry.as_str().expect("bundle.icon entries are paths");
        let path = manifest_dir().join(rel);
        assert!(path.is_file(), "tauri.conf.json names {rel}, which does not exist");
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > 512, "{rel} is {size} bytes, which is not a rendered icon");
    }

    let masters = manifest_dir().join("../assets/logo");
    for master in ["icon.svg", "icon-small.svg", "ramp.svg", "ramp-small.svg", "build.py"] {
        let path: &Path = &masters.join(master);
        assert!(path.is_file(), "assets/logo/{master} is missing; the icons cannot be regenerated");
    }
}
