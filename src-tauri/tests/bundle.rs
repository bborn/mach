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

/// The body's share of the tile, low and high.
///
/// macOS does not draw an app icon to the edge of its tile. Apple's icons put
/// the rounded body inside a transparent margin, and every icon in the Dock is
/// on that grid — so one drawn full-bleed is larger than all of its neighbours
/// at the same tile size, which is how this shipped and what the owner
/// reported. Measured off Safari, Finder, Mail, Terminal and Notes, which all
/// agree exactly:
///
/// ```text
///   16px   14/16  = 0.8750      128px  104/128 = 0.8125
///   32px   28/32  = 0.8750      256px  206/256 = 0.8047
/// ```
///
/// The band is those numbers with room either side. It is wide because it is
/// not trying to pin the design — it is trying to catch an icon that has no
/// margin at all, or one so inset it reads as small.
const BODY_MIN: f64 = 0.78;
const BODY_MAX: f64 = 0.90;

/// Alpha at or above this is body. Below it is the rim hairline's antialiased
/// fringe, which reaches about half a pixel further out and would otherwise be
/// measured as part of the shape.
const SOLID: u8 = 128;

/// The opaque bounding box of a PNG: `(canvas, left, top, right, bottom)`.
fn opaque_bounds(path: &Path) -> (u32, u32, u32, u32, u32) {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .unwrap_or_else(|e| panic!("{} is not a readable PNG: {e}", path.display()));

    assert_eq!(
        reader.output_color_type(),
        (png::ColorType::Rgba, png::BitDepth::Eight),
        "{} is not 8-bit RGBA, so it has no alpha channel to measure",
        path.display()
    );

    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    let (w, h) = (info.width, info.height);
    assert_eq!(w, h, "{} is {w}x{h}; an app icon is square", path.display());

    let (mut left, mut top, mut right, mut bottom) = (w, h, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            if buf[((y * w + x) * 4 + 3) as usize] >= SOLID {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    assert!(right > left, "{} has no opaque pixels at all", path.display());
    (w, left, top, right, bottom)
}

/// Catches the icon that is the wrong size inside its own tile.
///
/// The test above this one asks whether the files exist and are big enough to
/// be renders rather than placeholders. Both were true of the full-bleed set:
/// nothing was missing, nothing was empty, and the only symptom was that Mach
/// looked heavier than Safari and Slack on either side of it in the Dock. What
/// distinguishes the two is geometry, so that is what this measures.
///
/// Only the PNGs are read. `.icns` and `.ico` are containers whose sizes come
/// out of the same `build.py` run from the same two masters, so a PNG on the
/// grid means they are too — and decoding either would need a dependency this
/// crate has no other use for.
#[test]
fn every_icon_sits_inside_the_platform_grid() {
    let conf = read("tauri.conf.json");
    let value: serde_json::Value = serde_json::from_str(&conf).unwrap();

    let mut paths: Vec<String> = value["bundle"]["icon"]
        .as_array()
        .expect("bundle.icon must be an array")
        .iter()
        .filter_map(|e| e.as_str())
        .filter(|p| p.ends_with(".png"))
        .map(str::to_owned)
        .collect();
    // The 1024 render is not bundled, but it is what the store listing and the
    // site use, and it is the size the grid is defined at.
    paths.push("icons/icon.png".to_owned());

    assert!(!paths.is_empty(), "bundle.icon names no PNG, so nothing here is measured");

    for rel in paths {
        let path = manifest_dir().join(&rel);
        let (canvas, left, top, right, bottom) = opaque_bounds(&path);
        let side = f64::from(canvas);
        let width = f64::from(right - left) / side;
        let height = f64::from(bottom - top) / side;

        for (axis, share) in [("wide", width), ("tall", height)] {
            assert!(
                (BODY_MIN..=BODY_MAX).contains(&share),
                "{rel} is {canvas}px and its body is {share:.4} of that {axis}, outside \
                 {BODY_MIN}–{BODY_MAX}. macOS draws the body inside a transparent margin \
                 — Safari and Finder are 0.8047 at 256 and 0.8750 at 16 and 32 — so an \
                 icon at 1.0 is bigger than everything beside it in the Dock. Fix it in \
                 assets/logo/build.py's BODY and re-run it; do not edit the PNG."
            );
        }

        // A body the right size but pushed off centre is the same defect seen
        // from the side, and costs one subtraction to rule out.
        let slack = f64::from(canvas) / 64.0 + 1.0;
        for (axis, near, far) in [("horizontally", left, canvas - right), ("vertically", top, canvas - bottom)] {
            let drift = f64::from(near).abs() - f64::from(far).abs();
            assert!(
                drift.abs() <= slack,
                "{rel} has {near}px of margin on one side and {far}px on the other {axis}; \
                 the body is not centred in its tile"
            );
        }
    }
}
