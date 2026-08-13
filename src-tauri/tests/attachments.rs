//! Attachments, tested as a security boundary.
//!
//! Everything a sender controls arrives here: the filename, the bytes, the
//! declared type, the size. So most of this file is not "does the feature
//! work" — it is "what happens when the value was chosen by somebody who wants
//! the owner's mail". The three properties under test are:
//!
//! 1. **Containment.** No sender-supplied string can name a file outside the
//!    cache directory, by any spelling of traversal, and no sanitized name is
//!    ever anything but a single path component.
//! 2. **Honesty.** A name cannot render as one thing and be another. The
//!    right-to-left override case is the whole reason this matters.
//! 3. **Restraint.** Nothing is fetched, written or opened that was not asked
//!    for by name, executables are not opened at all, and a size a stranger
//!    declared cannot make the process allocate.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use mach_lib::google::gmail::GmailClient;
use mach_lib::google::{
    BoxFuture, GoogleError, HttpRequest, HttpResponse, HttpTransport, StaticTokenProvider,
    TransportError,
};
use mach_lib::ipc::attachments::store::names::{
    disambiguate, extension_of, is_dangerous, is_safe_component, is_valid_content_id,
    raster_extension, raster_mime, safe_filename, sniff_executable, sniff_raster_image,
    truncate_preserving_extension, FALLBACK_NAME, MAX_FILENAME_BYTES,
};
use mach_lib::ipc::attachments::store::{
    cache_key, AttachmentCache, PartKind, MAX_ATTACHMENT_BYTES,
};

// ===========================================================================
// Scaffolding
// ===========================================================================

/// A scratch directory that removes itself. Same shape as the one in
/// `tests/db.rs`; kept local so this file has no dependency on that one.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("mach-attachments-{label}-{nanos}-{:?}", std::thread::current().id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct ScriptedTransport {
    responses: Mutex<std::collections::VecDeque<Result<HttpResponse, TransportError>>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl ScriptedTransport {
    fn new(responses: Vec<Result<HttpResponse, TransportError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for ScriptedTransport {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        self.requests.lock().unwrap().push(request);
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(TransportError::new("script exhausted")));
        Box::pin(async move { next })
    }
}

fn gmail(transport: Arc<ScriptedTransport>) -> GmailClient {
    GmailClient::new(transport, Arc::new(StaticTokenProvider::new("test-token")))
        .with_base_url("https://gmail.test/gmail/v1")
}

/// Gmail's attachment envelope, base64url encoded the way the API sends it.
fn attachment_envelope(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    format!(
        r#"{{"size":{},"data":"{}"}}"#,
        bytes.len(),
        URL_SAFE_NO_PAD.encode(bytes)
    )
}

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

// ===========================================================================
// Filenames — containment
// ===========================================================================

/// The property every other filename test is a special case of.
fn assert_contained(raw: &str) {
    let name = safe_filename(raw);
    assert!(
        is_safe_component(&name),
        "safe_filename({raw:?}) produced {name:?}, which is not one path component"
    );
    // The name is joined onto a root, and the result must still be under it —
    // this is the actual guarantee, expressed as a path operation rather than
    // as a string one.
    let root = Path::new("/var/mach/attachments/ab/abcdef");
    let joined = root.join(&name);
    assert_eq!(
        joined.parent(),
        Some(root),
        "{raw:?} escaped its directory as {joined:?}"
    );
    assert!(
        !joined
            .components()
            .any(|c| matches!(c, Component::ParentDir)),
        "{raw:?} produced a parent-dir component"
    );
}

#[test]
fn relative_traversal_cannot_escape_the_cache_directory() {
    for raw in [
        "../../../etc/passwd",
        "..\\..\\..\\windows\\system32\\config\\sam",
        "....//....//etc/passwd",
        "foo/../../../bar.txt",
        "./../../secret.key",
        "..",
        "../",
        "../..",
        ".",
    ] {
        assert_contained(raw);
    }

    // And the surviving component is the leaf, not the path.
    assert_eq!(safe_filename("../../../etc/passwd"), "passwd");
    assert_eq!(safe_filename("foo/../../../bar.txt"), "bar.txt");
    // Nothing but dots is not a name at all.
    assert_eq!(safe_filename(".."), FALLBACK_NAME);
    assert_eq!(safe_filename("."), FALLBACK_NAME);
    assert_eq!(safe_filename("...."), FALLBACK_NAME);
}

#[test]
fn absolute_and_drive_and_unc_paths_lose_everything_but_the_leaf() {
    assert_eq!(safe_filename("/etc/passwd"), "passwd");
    assert_eq!(safe_filename("/"), FALLBACK_NAME);
    assert_eq!(
        safe_filename("C:\\Windows\\System32\\evil.dll"),
        "evil.dll"
    );
    assert_eq!(safe_filename("\\\\server\\share\\payload.txt"), "payload.txt");
    assert_eq!(safe_filename("~/.ssh/authorized_keys"), "authorized_keys");
    for raw in [
        "/etc/passwd",
        "C:\\Windows\\System32\\evil.dll",
        "\\\\server\\share\\payload.txt",
        "/",
        "\\",
    ] {
        assert_contained(raw);
    }
}

#[test]
fn nul_bytes_and_control_characters_are_removed() {
    // A NUL truncates a name in any C API below us, so `report\0.exe` would be
    // written as `report` by one layer and checked as `report\0.exe` by another.
    assert_eq!(safe_filename("report\u{0}.txt"), "report.txt");
    assert_eq!(safe_filename("re\u{0}port\u{7}.pdf"), "report.pdf");
    assert_eq!(safe_filename("line\nbreak\ttab.txt"), "line break tab.txt");
    assert_eq!(safe_filename("bell\u{7f}.txt"), "bell.txt");
    // C1 controls too. NEL is both a control and whitespace, and whitespace
    // wins: dropping it would silently join `x` and `y` into a word neither
    // the sender nor the reader wrote.
    assert_eq!(safe_filename("x\u{85}y.txt"), "x y.txt");
    // Runs collapse rather than padding the name out.
    assert_eq!(safe_filename("a\n\n\n\tb.txt"), "a b.txt");
    // Unicode line/paragraph separators are not `char::is_control`.
    assert_eq!(safe_filename("a\u{2028}b\u{2029}c.txt"), "abc.txt");

    for raw in ["report\u{0}.txt", "\u{0}", "\u{0}\u{0}\u{0}"] {
        assert_contained(raw);
    }
    assert_eq!(safe_filename("\u{0}"), FALLBACK_NAME);
}

#[test]
fn a_sanitized_name_is_never_a_path_separator_carrier() {
    for raw in [
        "a/b", "a\\b", "//", "\\\\", "a//b//c", "/a/", "..\\a", "a/../b",
    ] {
        let name = safe_filename(raw);
        assert!(!name.contains('/'), "{raw:?} -> {name:?}");
        assert!(!name.contains('\\'), "{raw:?} -> {name:?}");
        assert_contained(raw);
    }
}

// ===========================================================================
// Filenames — honesty
// ===========================================================================

#[test]
fn a_right_to_left_override_cannot_disguise_an_extension() {
    // `evil\u{202E}gnp.exe` renders as `evilexe.png` in every list view there
    // is. Removing the override is what makes the string the reader sees and
    // the string `is_dangerous` inspects the same string.
    let raw = "evil\u{202E}gnp.exe";
    let name = safe_filename(raw);
    assert_eq!(name, "evilgnp.exe");
    assert!(
        !name.chars().any(|c| c == '\u{202E}'),
        "the override survived: {name:?}"
    );
    assert!(
        is_dangerous(&name, "application/octet-stream"),
        "the real extension must be visible to the executable check"
    );
    assert_eq!(extension_of(&name).as_deref(), Some("exe"));
}

#[test]
fn every_bidi_and_invisible_control_is_stripped() {
    for control in [
        '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', // embeddings and overrides
        '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', // isolates
        '\u{200E}', '\u{200F}', '\u{061C}', // marks
        '\u{200B}', '\u{200C}', '\u{200D}', // zero width
        '\u{FEFF}', '\u{00AD}', '\u{180E}', '\u{2060}',
    ] {
        let raw = format!("in{control}voice.pdf");
        let name = safe_filename(&raw);
        assert_eq!(
            name, "invoice.pdf",
            "U+{:04X} survived sanitising",
            control as u32
        );
        assert!(is_safe_component(&name));
    }

    // A name made only of invisibles is not a name.
    assert_eq!(safe_filename("\u{202E}\u{200B}\u{FEFF}"), FALLBACK_NAME);
}

#[test]
fn a_stripped_override_does_not_leave_a_hidden_or_reserved_name() {
    // `\u{202E}.bashrc` would otherwise become `.bashrc` — invisible in Finder
    // and read by every login shell.
    assert_eq!(safe_filename("\u{202E}.bashrc"), "_.bashrc");
    assert_eq!(safe_filename("\u{200B}CON.txt"), "_CON.txt");
}

// ===========================================================================
// Filenames — reserved and awkward names
// ===========================================================================

#[test]
fn dot_files_and_flag_shaped_names_are_defused() {
    assert_eq!(safe_filename(".bashrc"), "_.bashrc");
    assert_eq!(safe_filename(".ssh"), "_.ssh");
    assert_eq!(safe_filename(".DS_Store"), "_.DS_Store");
    // A leading dash is what turns a saved file into an argument the next
    // command reads as a flag. Prefixed, not stripped: the name stays
    // recognisable and it is no longer the first thing an argv parser sees.
    assert_eq!(safe_filename("-rf"), "_-rf");
    assert_eq!(safe_filename("--force.txt"), "_--force.txt");
    // An ordinary name keeps its dots.
    assert_eq!(safe_filename("q3.final.pdf"), "q3.final.pdf");
}

#[test]
fn windows_device_names_are_prefixed() {
    for raw in ["CON", "con", "PRN", "AUX", "NUL", "COM1", "lpt9"] {
        let name = safe_filename(raw);
        assert!(
            name.starts_with('_'),
            "{raw:?} must not stay a device name; got {name:?}"
        );
    }
    assert_eq!(safe_filename("CON.txt"), "_CON.txt");
    assert_eq!(safe_filename("nul.pdf"), "_nul.pdf");
    // Not a device name, and must not be mangled.
    assert_eq!(safe_filename("console.log"), "console.log");
    assert_eq!(safe_filename("COM10.txt"), "COM10.txt");
}

#[test]
fn trailing_dots_and_spaces_cannot_hide_the_real_extension() {
    // Windows normalises these away, so `evil.exe. ` and `evil.exe` name the
    // same file there while comparing unequal to a naive check.
    assert_eq!(safe_filename("evil.exe. "), "evil.exe");
    assert_eq!(safe_filename("evil.exe..."), "evil.exe");
    assert_eq!(safe_filename("evil.exe   "), "evil.exe");
    assert_eq!(safe_filename("  spaced.pdf  "), "spaced.pdf");
    assert!(is_dangerous(&safe_filename("evil.exe. "), "text/plain"));
}

#[test]
fn characters_reserved_on_other_platforms_are_replaced_not_dropped() {
    // Dropped would let `in<voice>.pdf` and `invoice.pdf` collide; replaced
    // keeps them distinct and keeps the name readable.
    assert_eq!(safe_filename("in<voice>.pdf"), "in_voice_.pdf");
    assert_eq!(safe_filename("a:b|c?d*e\"f.txt"), "a_b_c_d_e_f.txt");
}

#[test]
fn an_overlong_name_is_truncated_without_losing_its_extension() {
    let long = format!("{}.pdf", "a".repeat(4000));
    let name = safe_filename(&long);
    assert!(name.len() <= MAX_FILENAME_BYTES, "{} bytes", name.len());
    assert!(name.ends_with(".pdf"), "{name:?}");
    assert!(is_safe_component(&name));

    // Multibyte characters must not be cut in half — a `String` sliced
    // mid-codepoint is a panic, and the input is a stranger's bytes.
    let emoji = format!("{}.pdf", "🙂".repeat(500));
    let name = safe_filename(&emoji);
    assert!(name.len() <= MAX_FILENAME_BYTES);
    assert!(name.ends_with(".pdf"));
    assert!(std::str::from_utf8(name.as_bytes()).is_ok());

    // An "extension" that is 4000 characters long is not an extension, and
    // spending the whole budget on it would throw the name away.
    let silly = format!("report.{}", "x".repeat(4000));
    let name = safe_filename(&silly);
    assert!(name.len() <= MAX_FILENAME_BYTES);
    assert!(name.starts_with("report."), "{name:?}");

    // The helper itself, directly.
    assert_eq!(truncate_preserving_extension("short.txt", 200), "short.txt");
    assert_eq!(truncate_preserving_extension("abcdefgh.txt", 8), "abcd.txt");
}

#[test]
fn an_empty_or_whitespace_only_name_becomes_the_fallback() {
    for raw in ["", "   ", "\t\n", "\u{00A0}", "   .  ", "/", "//"] {
        assert_eq!(safe_filename(raw), FALLBACK_NAME, "for {raw:?}");
    }
    assert!(is_safe_component(FALLBACK_NAME));
    // The fallback carries no extension, so it cannot be a guess about content.
    assert_eq!(extension_of(FALLBACK_NAME), None);
}

#[test]
fn ordinary_names_survive_completely_unchanged() {
    for raw in [
        "Q3-numbers.pdf",
        "q3.csv",
        "Screen Shot 2026-08-07 at 09.14.22.png",
        "Rapport financier — été 2026.docx",
        "契約書.pdf",
        "notes",
    ] {
        assert_eq!(safe_filename(raw), raw, "sanitising damaged {raw:?}");
    }
}

#[test]
fn is_safe_component_rejects_what_safe_filename_can_never_produce() {
    for bad in ["", ".", "..", "a/b", "a\\b", "/", "a\u{0}b", "a\u{202E}b", "a\nb"] {
        assert!(!is_safe_component(bad), "{bad:?} was accepted");
    }
    for good in ["a.txt", "_.bashrc", "attachment", "契約書.pdf"] {
        assert!(is_safe_component(good), "{good:?} was rejected");
    }
}

/// The blanket property: whatever goes in, what comes out is writable.
#[test]
fn every_hostile_name_in_the_corpus_produces_a_safe_component() {
    let very_long = "a".repeat(9000);
    let corpus = [
        "../../../etc/passwd",
        "..\\..\\..\\boot.ini",
        "/dev/null",
        "C:\\autoexec.bat",
        "\u{0}",
        "\u{202E}gnp.exe",
        "CON",
        "..",
        ".",
        "",
        "   ",
        ".hidden",
        "-flag",
        very_long.as_str(),
        "🙂/🙃/../🙂.png",
        "\u{FEFF}\u{200B}",
        "name\u{0}.png\u{202E}",
        "....",
        "a:b|c?d*e\"f<g>h.txt",
    ];
    for raw in corpus {
        assert_contained(raw);
        assert!(!safe_filename(raw).is_empty());
    }
}

// ===========================================================================
// Executables
// ===========================================================================

#[test]
fn programs_are_recognised_however_they_are_spelled() {
    for name in [
        "installer.exe",
        "Installer.EXE",
        "setup.msi",
        "thing.app",
        "run.command",
        "script.sh",
        "script.bash",
        "macro.scpt",
        "shortcut.webloc",
        "disk.dmg",
        "package.pkg",
        "lib.dylib",
        "applet.jar",
        "thing.desktop",
        "hook.ps1",
        "x.vbs",
        "x.lnk",
        // The classic: a real extension hiding behind a fake one.
        "report.pdf.exe",
        "photo.jpg.scr",
    ] {
        assert!(
            is_dangerous(name, "application/octet-stream"),
            "{name:?} should not be openable"
        );
    }
}

#[test]
fn documents_are_not_treated_as_programs() {
    for name in [
        "Q3-numbers.pdf",
        "q3.csv",
        "chart.png",
        "deck.key",
        "notes.txt",
        "contract.docx",
        "archive.zip",
        "photo.jpeg",
        "attachment",
    ] {
        assert!(
            !is_dangerous(name, "application/pdf"),
            "{name:?} should be openable"
        );
    }
}

/// The inverted case, and the reason it is inverted.
///
/// These four were refused until the owner pointed out what that costs: an
/// HTML report and an SVG diagram are things people are *sent*, and a client
/// that will not open the invoice it has just rendered is broken rather than
/// careful. Opening one hands the browser a page, which is where a page goes;
/// it is not the same act as running a program, and this list is about
/// programs. `render::sanitize` still refuses both inside the reading pane —
/// see `tests/render.rs`, which is where that decision is tested.
#[test]
fn documents_that_open_in_a_browser_are_ordinary_attachments() {
    for (name, mime) in [
        ("invoice.html", "text/html"),
        ("invoice.htm", "text/html"),
        ("report.xhtml", "application/xhtml+xml"),
        ("page.mhtml", "message/rfc822"),
        ("page.mht", "message/rfc822"),
        ("diagram.svg", "image/svg+xml"),
        ("diagram.svgz", "image/svg+xml"),
        ("statement.shtml", "text/html"),
        ("figure.xht", "application/xhtml+xml"),
    ] {
        assert!(
            !is_dangerous(name, mime),
            "{name:?} is a document — Mach refusing to open it is the bug that started this"
        );
    }
}

/// The floor under [`is_dangerous`]: whatever else is relaxed, these still
/// start a process when they are double-clicked, so they are still refused.
#[test]
fn things_that_execute_on_a_double_click_are_still_refused() {
    for name in [
        "Installer.app",
        "install.pkg",
        "disk.dmg",
        "run.command",
        "deploy.sh",
        "deploy.zsh",
        "macro.scpt",
        "tool.jar",
        "setup.exe",
        "go.bat",
        "go.cmd",
        "hook.ps1",
        "setup.msi",
        "saver.scr",
        "macro.vbs",
    ] {
        assert!(
            is_dangerous(name, "application/octet-stream"),
            "{name:?} runs on a double click and must not be openable"
        );
    }

    for mime in [
        "application/x-mach-binary",
        "application/x-executable",
        "application/x-msdownload",
        "application/x-sh",
        "application/x-apple-diskimage",
        "application/vnd.apple.installer+xml",
        "application/x-msi",
    ] {
        assert!(
            is_dangerous("attachment", mime),
            "{mime:?} describes a program and must not be openable"
        );
    }
}

/// The macOS formats where "open" hands the system something it acts on with
/// the user's authority, even though nothing about them is an ELF header.
///
/// `.mobileconfig` is the one that matters most. Double-clicking one opens
/// System Settings at the profile installer, and an installed profile can add a
/// root certificate, point every connection at a proxy, or enrol the machine in
/// somebody else's MDM. It arrives as a small XML file with a friendly name, it
/// is the standard macOS phishing payload, and a mail client that opens it for
/// you is doing the attacker's hardest step.
#[test]
fn macos_formats_that_configure_or_mount_the_machine_are_refused() {
    for name in [
        // Configuration profiles: root CAs, proxies, MDM enrolment.
        "wifi-setup.mobileconfig",
        // Signed archives macOS expands and runs — how Xcode ships.
        "tool.xip",
        // Mountable images, siblings of the .dmg and .iso already refused.
        "backup.sparseimage",
        "backup.sparsebundle",
        "disc.cdr",
        "installer.smi",
        // Executable bundles, siblings of .bundle and .app.
        "helper.appex",
        "helper.xpc",
        "Thing.framework",
        // Terminal settings files carry a command string.
        "session.term",
        // Apple Shortcuts.
        "handy.shortcut",
        // Java Web Start.
        "launch.jnlp",
    ] {
        assert!(
            is_dangerous(name, "application/octet-stream"),
            "{name:?} hands the system something it acts on and must not be openable"
        );
    }
}

/// Extensions that are the same interpreter as one already on the list, spelled
/// differently. Leaving these off means the list refuses `x.js` and opens
/// `x.mjs`, which is not a decision, it is an oversight.
#[test]
fn script_extensions_are_refused_in_every_spelling_of_the_same_interpreter() {
    for name in [
        "loader.mjs",
        "loader.cjs",
        "silent.pyw",
        "bundle.pyz",
        "script.tcl",
        "payload.vbscript",
        "run.settingcontent-ms",
        "app.appx",
        "app.msix",
        "app.appxbundle",
        "repair.diagcab",
    ] {
        assert!(
            is_dangerous(name, "application/octet-stream"),
            "{name:?} runs code and must not be openable"
        );
    }
}

/// The bytes get a vote, not just the name.
///
/// A sender who names the part `invoice` with no extension and declares it
/// `application/pdf` gets past both halves of [`is_dangerous`], because both
/// halves read what the sender wrote. This is the one check that reads what the
/// sender *sent*.
#[test]
fn a_program_is_recognised_from_its_bytes_whatever_it_is_called() {
    // Mach-O, both widths, both endiannesses, and the universal wrapper.
    assert!(sniff_executable(&[0xFE, 0xED, 0xFA, 0xCF, 0x0C]).is_some());
    assert!(sniff_executable(&[0xCF, 0xFA, 0xED, 0xFE, 0x0C]).is_some());
    assert!(sniff_executable(&[0xFE, 0xED, 0xFA, 0xCE, 0x0C]).is_some());
    assert!(sniff_executable(&[0xCE, 0xFA, 0xED, 0xFE, 0x0C]).is_some());
    assert!(sniff_executable(&[0xCA, 0xFE, 0xBA, 0xBE, 0x00]).is_some());
    assert!(sniff_executable(&[0xBE, 0xBA, 0xFE, 0xCA, 0x00]).is_some());
    // ELF and PE, because a saved attachment gets forwarded.
    assert!(sniff_executable(b"\x7fELF\x02\x01").is_some());
    assert!(sniff_executable(b"MZ\x90\x00").is_some());

    // Documents are not programs, and this must never be the thing that stops
    // the owner opening his own mail.
    for benign in [
        &b"%PDF-1.7 ..."[..],
        &b"\x89PNG\r\n\x1a\n"[..],
        &b"PK\x03\x04"[..],       // any zip: docx, xlsx, key, jar-shaped-but-named-.zip
        &b"<!doctype html>"[..],
        &b"Dear Bruno,"[..],
        &b"{\"a\":1}"[..],
        &b""[..],
        &b"M"[..],
    ] {
        assert_eq!(
            sniff_executable(benign),
            None,
            "a document was called a program: {:?}",
            String::from_utf8_lossy(&benign[..benign.len().min(12)])
        );
    }
}

/// Polyglots, stated honestly.
///
/// A file can be a valid zip *and* a valid Mach-O at once — the zip directory
/// lives at the end, so the front of the file is free. The sniff reads the
/// front, so it catches that one. It does **not** catch the reverse (a real zip
/// with a program appended), and it cannot: that file *is* a zip, and opening
/// it unarchives rather than executes. The extension is what LaunchServices
/// dispatches on and is checked first; the sniff is the second opinion for the
/// case where the sender declined to give an extension at all.
#[test]
fn the_sniff_reads_the_front_of_the_file_which_is_what_the_loader_reads() {
    let mut macho_then_zip = vec![0xFE, 0xED, 0xFA, 0xCF];
    macho_then_zip.extend_from_slice(&[0u8; 64]);
    macho_then_zip.extend_from_slice(b"PK\x05\x06");
    assert!(
        sniff_executable(&macho_then_zip).is_some(),
        "a Mach-O with a zip trailer is still a Mach-O to the loader"
    );

    // And the honest limit, asserted so nobody reads more into the defence than
    // it gives: a zip with a program inside is an ordinary zip.
    let mut zip_then_macho = b"PK\x03\x04".to_vec();
    zip_then_macho.extend_from_slice(&[0xFE, 0xED, 0xFA, 0xCF]);
    assert_eq!(sniff_executable(&zip_then_macho), None);
}

#[test]
fn a_harmless_name_with_an_executable_type_is_still_refused() {
    assert!(is_dangerous("notes", "application/x-mach-binary"));
    assert!(is_dangerous("readme", "application/x-msdownload"));
    assert!(is_dangerous("data", "application/x-sh; charset=utf-8"));
    assert!(is_dangerous("thing", "APPLICATION/X-EXECUTABLE"));
    assert!(!is_dangerous("thing", "application/pdf"));
}

// ===========================================================================
// Inline image sniffing
// ===========================================================================

#[test]
fn only_raster_images_are_accepted_inline_and_the_bytes_decide() {
    assert_eq!(sniff_raster_image(PNG_MAGIC), Some("image/png"));
    assert_eq!(sniff_raster_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
    assert_eq!(sniff_raster_image(b"GIF89a...."), Some("image/gif"));
    assert_eq!(sniff_raster_image(b"GIF87a...."), Some("image/gif"));
    assert_eq!(sniff_raster_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
    assert_eq!(sniff_raster_image(b"BM\0\0"), Some("image/bmp"));
    assert_eq!(sniff_raster_image(&[0, 0, 1, 0, 1, 0]), Some("image/x-icon"));

    // SVG is a document format with script capability. `render::sanitize`
    // refuses it for `data:` images; accepting it here would be a way around
    // that decision rather than an agreement with it.
    assert_eq!(sniff_raster_image(b"<svg xmlns=\"http://www.w3.org/2000/svg\">"), None);
    assert_eq!(sniff_raster_image(b"<?xml version=\"1.0\"?><svg/>"), None);
    assert_eq!(sniff_raster_image(b"<!doctype html><script>"), None);
    assert_eq!(sniff_raster_image(b"%PDF-1.7"), None);
    assert_eq!(sniff_raster_image(b""), None);
    assert_eq!(sniff_raster_image(b"\x7fELF"), None);
}

#[test]
fn a_cached_inline_image_round_trips_through_its_extension() {
    for mime in [
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/bmp",
        "image/x-icon",
    ] {
        assert_eq!(raster_mime(raster_extension(mime)), Some(mime));
    }
    assert_eq!(raster_mime("svg"), None);
    assert_eq!(raster_mime("exe"), None);
}

#[test]
fn a_content_id_must_look_like_one() {
    assert!(is_valid_content_id("chart-inline-001"));
    assert!(is_valid_content_id("image001.png@01D9.ABCDEF"));
    assert!(!is_valid_content_id(""));
    assert!(!is_valid_content_id("../../etc/passwd"));
    assert!(!is_valid_content_id("a b"));
    assert!(!is_valid_content_id("a\"b"));
    assert!(!is_valid_content_id("<angle>"));
    assert!(!is_valid_content_id(&"a".repeat(513)));
    assert!(is_valid_content_id(&"a".repeat(512)));
}

// ===========================================================================
// Cache keys
// ===========================================================================

#[test]
fn two_messages_with_the_same_attachment_id_do_not_collide() {
    let a = cache_key(1, "msg-a", PartKind::File, "ANGjdJ_001");
    let b = cache_key(1, "msg-b", PartKind::File, "ANGjdJ_001");
    assert_ne!(a, b, "the message id must be part of the key");

    // Nor do two accounts, whose id spaces are unrelated.
    let c = cache_key(2, "msg-a", PartKind::File, "ANGjdJ_001");
    assert_ne!(a, c, "the account id must be part of the key");

    // Nor a file and an inline part that happen to share an identifier.
    let d = cache_key(1, "msg-a", PartKind::Inline, "ANGjdJ_001");
    assert_ne!(a, d, "the part kind must be part of the key");

    // And the same three inputs always give the same answer, or nothing would
    // ever be a cache hit.
    assert_eq!(a, cache_key(1, "msg-a", PartKind::File, "ANGjdJ_001"));
}

#[test]
fn field_boundaries_in_the_key_cannot_be_shifted() {
    // Without length prefixes these two would hash the same bytes, and a
    // sender who controls part of one field could aim at another message.
    let a = cache_key(1, "ab", PartKind::File, "c");
    let b = cache_key(1, "a", PartKind::File, "bc");
    assert_ne!(a, b);

    let c = cache_key(11, "x", PartKind::File, "y");
    let d = cache_key(1, "1x", PartKind::File, "y");
    assert_ne!(c, d);
}

#[test]
fn a_key_is_hex_and_therefore_always_a_safe_path_component() {
    let key = cache_key(1, "../../etc", PartKind::File, "../../passwd");
    assert_eq!(key.len(), 64);
    assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(is_safe_component(&key));
}

// ===========================================================================
// The cache
// ===========================================================================

#[test]
fn bytes_round_trip_and_a_second_look_is_a_hit() {
    let dir = TempDir::new("roundtrip");
    let cache = AttachmentCache::new(dir.path());
    let key = cache_key(1, "m1", PartKind::File, "a1");

    assert!(cache.find(&key).is_none(), "nothing is cached yet");

    let stored = cache.store(&key, "Q3-numbers.pdf", b"%PDF-1.7 ...").unwrap();
    assert_eq!(stored.size_bytes, 12);
    assert!(stored.path.ends_with("Q3-numbers.pdf"));
    assert_eq!(std::fs::read(&stored.path).unwrap(), b"%PDF-1.7 ...");

    let hit = cache.find(&key).expect("a hit");
    assert_eq!(hit.path, stored.path);
    assert_eq!(hit.size_bytes, 12);

    // A different key is still a miss — the cache is not answering by filename.
    assert!(cache.find(&cache_key(1, "m2", PartKind::File, "a1")).is_none());
}

#[test]
fn a_hostile_filename_cannot_write_outside_the_cache_root() {
    let dir = TempDir::new("escape");
    let cache = AttachmentCache::new(dir.path());
    let root = cache.root().to_path_buf();

    // A canary the traversal would be aiming at.
    let canary = dir.path().join("canary.txt");
    std::fs::write(&canary, b"original").unwrap();

    for hostile in [
        "../../../canary.txt",
        "..\\..\\canary.txt",
        "/etc/passwd",
        "..",
        ".",
        "\u{202E}gnp.exe",
        "sub/dir/file.txt",
    ] {
        let key = cache_key(1, "m1", PartKind::File, hostile);
        let name = safe_filename(hostile);
        let stored = cache.store(&key, &name, b"attacker bytes").unwrap();

        let canonical = stored.path.canonicalize().unwrap();
        let canonical_root = root.canonicalize().unwrap();
        assert!(
            canonical.starts_with(&canonical_root),
            "{hostile:?} wrote to {canonical:?}, outside {canonical_root:?}"
        );
    }

    assert_eq!(
        std::fs::read(&canary).unwrap(),
        b"original",
        "the canary was overwritten"
    );
}

#[test]
fn the_cache_refuses_a_name_that_did_not_go_through_the_sanitizer() {
    let dir = TempDir::new("refuse");
    let cache = AttachmentCache::new(dir.path());
    let key = cache_key(1, "m1", PartKind::File, "a1");

    // This is the redundant check: `store` never trusts that its caller
    // remembered to sanitize.
    for raw in ["../escape.txt", "a/b.txt", "", "..", "x\u{0}y"] {
        let error = cache.store(&key, raw, b"bytes").unwrap_err();
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::InvalidInput,
            "{raw:?} was not refused"
        );
    }
    assert!(cache.find(&key).is_none(), "a refused write left an entry");
}

#[test]
fn restoring_an_entry_under_a_new_name_leaves_exactly_one_file() {
    let dir = TempDir::new("replace");
    let cache = AttachmentCache::new(dir.path());
    let key = cache_key(1, "m1", PartKind::File, "a1");

    cache.store(&key, "old-name.pdf", b"one").unwrap();
    let second = cache.store(&key, "new-name.pdf", b"two-two").unwrap();

    let files: Vec<_> = std::fs::read_dir(cache.entry_dir(&key))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(cache.find(&key).unwrap().path, second.path);
    assert_eq!(std::fs::read(&second.path).unwrap(), b"two-two");
}

#[test]
fn no_partial_file_is_ever_served_as_a_cache_hit() {
    let dir = TempDir::new("partial");
    let cache = AttachmentCache::new(dir.path());
    let key = cache_key(1, "m1", PartKind::File, "a1");

    // Simulate a crash mid-download: the temporary file exists, the rename
    // never happened.
    let entry = cache.entry_dir(&key);
    std::fs::create_dir_all(&entry).unwrap();
    std::fs::write(entry.join(".part-deadbeef"), b"half a pdf").unwrap();

    assert!(
        cache.find(&key).is_none(),
        "a half-written file was served as complete"
    );
}

#[test]
fn the_cache_evicts_oldest_first_and_never_the_entry_just_written() {
    let dir = TempDir::new("evict");
    // 1000-byte cap, evict down to 500.
    let cache = AttachmentCache::new(dir.path()).with_limits(1000, 500);

    let keys: Vec<String> = (0..5)
        .map(|i| cache_key(1, &format!("m{i}"), PartKind::File, "a"))
        .collect();

    for key in &keys {
        cache.store(key, "blob.bin", &vec![b'x'; 200]).unwrap();
        // mtime has one-second granularity on some filesystems; stagger so the
        // ordering under test is the one being asserted.
        std::thread::sleep(std::time::Duration::from_millis(15));
    }

    // Five 200-byte entries is 1000, which is at the cap and not over it.
    assert_eq!(cache.total_bytes(), 1000);
    assert_eq!(cache.entry_count(), 5);

    // The sixth pushes it over, so eviction runs down to the low-water mark.
    let newest = cache_key(1, "m-newest", PartKind::File, "a");
    cache.store(&newest, "blob.bin", &vec![b'x'; 200]).unwrap();

    assert!(
        cache.total_bytes() <= 500,
        "eviction stopped at {} bytes",
        cache.total_bytes()
    );
    assert!(
        cache.find(&newest).is_some(),
        "the entry that was just written was evicted"
    );
    assert!(
        cache.find(&keys[0]).is_none(),
        "the oldest entry survived while newer ones were dropped"
    );
}

#[test]
fn a_cache_hit_is_what_keeps_an_entry_young() {
    let dir = TempDir::new("lru");
    // 500-byte cap, evict down to 400: one 200-byte entry has to go, and which
    // one is the entire question.
    let cache = AttachmentCache::new(dir.path()).with_limits(500, 400);

    let old = cache_key(1, "old", PartKind::File, "a");
    let middle = cache_key(1, "middle", PartKind::File, "a");
    cache.store(&old, "blob.bin", &vec![b'x'; 200]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(15));
    cache.store(&middle, "blob.bin", &vec![b'x'; 200]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(15));

    // Reading `old` makes it the most recently used, so `middle` is now the
    // eviction candidate. This is the whole difference between LRU and FIFO.
    assert!(cache.find(&old).is_some());
    std::thread::sleep(std::time::Duration::from_millis(15));

    let fresh = cache_key(1, "fresh", PartKind::File, "a");
    cache.store(&fresh, "blob.bin", &vec![b'x'; 200]).unwrap();

    assert!(cache.find(&fresh).is_some());
    assert!(
        cache.find(&old).is_some(),
        "the entry that was read was evicted anyway — this is FIFO, not LRU"
    );
    assert!(cache.find(&middle).is_none(), "the stale entry survived");
}

// ===========================================================================
// The fetch
// ===========================================================================

#[tokio::test]
async fn an_attachment_is_fetched_from_the_documented_endpoint_and_decoded() {
    let bytes = b"%PDF-1.7 a small document".to_vec();
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse::json(
        200,
        attachment_envelope(&bytes),
    ))]);
    let client = gmail(Arc::clone(&transport));

    let fetched = client
        .attachment_get_capped("me", "msg-1", "ANGjdJ_001", MAX_ATTACHMENT_BYTES)
        .await
        .expect("the fetch");

    assert_eq!(fetched, bytes);

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        "https://gmail.test/gmail/v1/users/me/messages/msg-1/attachments/ANGjdJ_001"
    );
    assert_eq!(
        requests[0].header("Authorization"),
        Some("Bearer test-token"),
        "the fetch must go through the same auth as every other call"
    );
}

#[tokio::test]
async fn a_response_larger_than_the_cap_is_refused_before_it_is_parsed() {
    // 4 MiB of base64 against a 1 KiB cap. The point is that the refusal comes
    // from the response *length*, so nothing this large is ever decoded into a
    // second allocation.
    let huge = format!(r#"{{"size":10,"data":"{}"}}"#, "A".repeat(4 * 1024 * 1024));
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse::json(200, huge))]);
    let client = gmail(transport);

    let error = client
        .attachment_get_capped("me", "msg-1", "big", 1024)
        .await
        .expect_err("must refuse");
    assert!(
        matches!(error, GoogleError::InvalidRequest { .. }),
        "got {error:?}"
    );
    assert!(error.to_string().contains("larger than"), "{error}");
}

#[tokio::test]
async fn a_lying_size_field_does_not_get_past_the_cap_either() {
    // Small enough to be parsed, declared small, actually over the cap once
    // decoded — the last of the three checks is what catches this.
    let bytes = vec![b'x'; 4096];
    let envelope = {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        format!(r#"{{"size":1,"data":"{}"}}"#, URL_SAFE_NO_PAD.encode(&bytes))
    };
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse::json(200, envelope))]);
    let client = gmail(transport);

    let error = client
        .attachment_get_capped("me", "msg-1", "liar", 2048)
        .await
        .expect_err("must refuse");
    assert!(matches!(error, GoogleError::InvalidRequest { .. }), "{error:?}");
}

#[tokio::test]
async fn a_declared_size_over_the_cap_is_refused_without_decoding() {
    let envelope = r#"{"size":999999999,"data":"AAAA"}"#;
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse::json(200, envelope))]);
    let client = gmail(transport);

    let error = client
        .attachment_get_capped("me", "msg-1", "claims-to-be-huge", 1024)
        .await
        .expect_err("must refuse");
    assert!(matches!(error, GoogleError::InvalidRequest { .. }), "{error:?}");
}

#[tokio::test]
async fn the_uncapped_helper_still_works_for_callers_that_want_it() {
    let bytes = PNG_MAGIC.to_vec();
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse::json(
        200,
        attachment_envelope(&bytes),
    ))]);
    let client = gmail(transport);

    assert_eq!(
        client.attachment_get("me", "m", "a").await.unwrap(),
        PNG_MAGIC
    );
}

#[tokio::test]
async fn a_gmail_error_keeps_its_own_classification() {
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse::json(
        404,
        r#"{"error":{"message":"Not Found"}}"#,
    ))]);
    let client = gmail(transport);

    let error = client
        .attachment_get_capped("me", "msg-1", "gone", MAX_ATTACHMENT_BYTES)
        .await
        .expect_err("must fail");
    assert!(error.is_not_found(), "got {error:?}");
}

// ===========================================================================
// Display
// ===========================================================================

#[test]
fn identical_filenames_are_disambiguated_for_display_only() {
    let names = vec![
        "image001.png".to_string(),
        "image001.png".to_string(),
        "notes".to_string(),
        "notes".to_string(),
        "IMAGE001.PNG".to_string(),
    ];
    let out = disambiguate(&names);
    assert_eq!(
        out,
        vec![
            "image001.png",
            "image001 (2).png",
            "notes",
            "notes (2)",
            "IMAGE001 (3).PNG"
        ]
    );
}
