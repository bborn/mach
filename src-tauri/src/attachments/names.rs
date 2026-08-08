//! Turning a sender-supplied filename into a name that is safe to put on disk.
//!
//! # Threat model
//!
//! Every string that reaches this module was chosen by whoever sent the mail,
//! which is to say by anybody at all: the owner's address is public, and a
//! `Content-Disposition: attachment; filename="…"` header is free-form bytes.
//! By the time a name gets here it is about to become part of a path we
//! `create_dir_all` into, and later part of a path handed to LaunchServices.
//! Two properties therefore have to hold, and they are different properties:
//!
//! 1. **Containment.** The result must be a single path *component*. Not a
//!    path, not a component that resolves to a parent, not a component that an
//!    OS API would reinterpret. `../../../.ssh/authorized_keys` must not be
//!    able to name a file outside the cache directory, and neither must
//!    `/etc/passwd`, `C:\Windows\System32\x.dll`, `..`, or a name containing a
//!    NUL that truncates in a C API somewhere below us.
//!
//! 2. **Honesty.** The result must not *look* like something it is not. This is
//!    the right-to-left override attack: `report\u{202E}gnp.exe` renders in
//!    every list view on earth as `reportexe.png`, and it is an executable. The
//!    fix is not to detect the trick and warn — it is to remove the characters
//!    that make the display differ from the bytes, so that what
//!    [`is_dangerous`] inspects and what the reader sees are the same string.
//!
//! Containment is enforced twice: once by construction here, and once by
//! [`is_safe_component`], which the cache asserts on immediately before it
//! writes. A single function that is correct is better than two that agree, but
//! this is the boundary where the cost of being wrong is arbitrary file write,
//! so it gets the redundant check.
//!
//! # What this deliberately does not do
//!
//! It does not try to make the name *pretty*, and it does not preserve the
//! sender's intent where that conflicts with either property above. A name that
//! survives unchanged is the common case; a name that is mangled was hostile or
//! broken, and a mangled name is a strictly better outcome than a clever one.

use std::collections::HashSet;

/// What an unusable name becomes. Deliberately boring, deliberately extensionless
/// — a fallback that carried an extension would be a guess about content, and
/// LaunchServices acts on guesses.
pub const FALLBACK_NAME: &str = "attachment";

/// Longest name we will write.
///
/// APFS and HFS+ both cap a component at 255 *bytes*, and so does every Linux
/// filesystem worth naming. The margin below that is for the ` (2)` a save
/// panel may append and for the temporary `.part-…` prefix the cache writes
/// through, neither of which should be able to push a legal name over the
/// limit and turn a write into an `ENAMETOOLONG` the user cannot act on.
pub const MAX_FILENAME_BYTES: usize = 200;

/// Past this, a trailing dot-separated run is not an extension — it is the rest
/// of the name. Only matters for where [`truncate_preserving_extension`] cuts.
const MAX_EXTENSION_BYTES: usize = 24;

/// Characters that are invisible, or that reorder what follows them.
///
/// Stripped rather than replaced. Replacing with `_` would leave a visible scar
/// on a name that legitimately contained a soft hyphen, and the scar is worth
/// less than the name.
///
/// The bidi controls (`U+202A`–`U+202E`, `U+2066`–`U+2069`, `U+200E`, `U+200F`,
/// `U+061C`) are the whole reason this list exists: they are what turns
/// `evil\u{202E}gnp.exe` into something that reads as `evilexe.png`. The
/// zero-width and format characters (`U+200B`–`U+200D`, `U+FEFF`, `U+00AD`,
/// `U+180E`) are here for the weaker version of the same trick — a name that
/// compares unequal to the one the reader thinks they are looking at.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}'              // soft hyphen
        | '\u{061C}'            // arabic letter mark
        | '\u{180E}'            // mongolian vowel separator
        | '\u{200B}'..='\u{200F}' // zero-width space/joiners, LRM, RLM
        | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
        | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
        | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
        | '\u{FEFF}'            // BOM / zero-width no-break space
        | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation
    )
}

/// Characters that are legal in a filename on the local filesystem but are
/// reserved somewhere the file will plausibly travel — a Windows share, a zip,
/// a sync client. Replaced with `_` rather than dropped so that removing them
/// cannot silently join two parts of a name into a third meaning.
const REPLACED: &[char] = &['<', '>', ':', '"', '|', '?', '*'];

/// Names DOS reserved in 1981 and Windows has honoured ever since. Opening
/// `CON.txt` there talks to the console rather than to a file.
const DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The one entry point: a sender's filename, as something safe to write.
///
/// Always returns a non-empty string containing no path separator, no NUL, no
/// control character and no bidi control, which is neither `.` nor `..`, does
/// not begin with `.` or `-`, does not end in a dot or a space, and is at most
/// [`MAX_FILENAME_BYTES`] long.
pub fn safe_filename(raw: &str) -> String {
    // 1. Keep only the last path component. This is what defeats traversal, and
    //    it defeats every spelling of it at once — `../`, `..\`, an absolute
    //    path, a Windows drive path, a UNC path — because none of them can name
    //    anything without a separator, and everything before the final
    //    separator is discarded rather than inspected.
    let last = raw
        .rsplit(['/', '\\'])
        .find(|segment| !segment.trim().is_empty())
        .unwrap_or("");

    // 2. Remove what must not be there and neutralise what must not be trusted.
    //
    //    Order matters. Whitespace is tested *before* control-ness, because a
    //    tab and a newline are both: dropping them outright would silently join
    //    two words of the name into a third one, and a name that reads
    //    differently from what the sender wrote is the thing this module exists
    //    to prevent. So every flavour of whitespace — tab, newline, NBSP, NEL —
    //    becomes one ordinary space, and the runs are collapsed afterwards.
    //    Everything else in `char::is_control` (NUL, the rest of C0, DEL, C1) is
    //    removed, as are the invisible and bidi controls and the two Unicode
    //    line separators, which are in neither category.
    let cleaned: String = last
        .chars()
        .filter_map(|c| {
            if is_invisible(c) || c == '\u{2028}' || c == '\u{2029}' {
                None
            } else if c.is_whitespace() {
                Some(' ')
            } else if c.is_control() {
                None
            } else if REPLACED.contains(&c) {
                Some('_')
            } else {
                Some(c)
            }
        })
        .collect();

    // Runs of whitespace collapse, so `a\n\n\nb` is `a b` rather than a name
    // padded out to look like something else in a fixed-width listing.
    let cleaned = collapse_spaces(&cleaned);

    // 3. Trailing dots and spaces are stripped by the Windows path
    //    normaliser, which means `evil.exe. ` and `evil.exe` are the same file
    //    there while comparing unequal here. Leading whitespace is cosmetic.
    let trimmed = cleaned
        .trim_start()
        .trim_end_matches(|c: char| c == '.' || c == ' ');

    // 4. Nothing left, or nothing but dots: there is no name to preserve.
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '.') {
        return FALLBACK_NAME.to_string();
    }

    let mut name = trimmed.to_string();

    // 5. A leading dot makes a hidden file, which is how an attachment named
    //    `.bash_profile` gets saved somewhere it will be read but never seen. A
    //    leading dash makes a name that a shell — or a careless `Command` —
    //    reads as a flag. Both are prefixed rather than stripped, so the name
    //    stays recognisable.
    if name.starts_with('.') || name.starts_with('-') {
        name.insert(0, '_');
    }

    // 6. Device names, with or without an extension.
    if is_device_name(&name) {
        name.insert(0, '_');
    }

    // 7. Length. Truncating the extension away would change what the file
    //    *is*, so the stem gives up the bytes.
    name = truncate_preserving_extension(&name, MAX_FILENAME_BYTES);

    // 8. The redundant check. Everything above should make this unreachable;
    //    if it is ever reached, a name we cannot vouch for must not become a
    //    path, so it becomes the fallback instead.
    if !is_safe_component(&name) {
        return FALLBACK_NAME.to_string();
    }

    name
}

/// Is this string usable as exactly one path component?
///
/// The cache calls this immediately before it joins the name onto its root. It
/// is not a substitute for [`safe_filename`] — it rejects rather than repairs —
/// but it is what makes "a hostile filename cannot escape the cache directory"
/// a property of the code that does the writing rather than a property of a
/// function somebody remembered to call.
pub fn is_safe_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !name.chars().any(char::is_control)
        && !name.chars().any(is_invisible)
        && std::path::Path::new(name).components().count() == 1
}

fn is_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim();
    DEVICE_NAMES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
}

/// Cut a name down to `limit` bytes without losing its extension or splitting a
/// character in half.
pub fn truncate_preserving_extension(name: &str, limit: usize) -> String {
    if name.len() <= limit {
        return name.to_string();
    }

    // An "extension" that is longer than a few characters is not one, and
    // preserving it would spend the entire budget on the wrong half.
    let split = name
        .rfind('.')
        .filter(|i| *i > 0 && name.len() - i <= MAX_EXTENSION_BYTES);

    match split {
        Some(i) => {
            let (stem, ext) = name.split_at(i);
            let budget = limit.saturating_sub(ext.len());
            if budget == 0 {
                // The extension alone will not fit; there is nothing worth
                // preserving, so cut the whole thing.
                floor_char_boundary(name, limit).to_string()
            } else {
                format!("{}{}", floor_char_boundary(stem, budget), ext)
            }
        }
        None => floor_char_boundary(name, limit).to_string(),
    }
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut previous_was_space = false;
    for c in s.chars() {
        if c == ' ' {
            if !previous_was_space {
                out.push(' ');
            }
            previous_was_space = true;
        } else {
            out.push(c);
            previous_was_space = false;
        }
    }
    out
}

/// `&s[..limit]`, moved back to the nearest character boundary. Slicing a
/// `String` mid-codepoint panics, and a filename is exactly the kind of value
/// that arrives as four-byte emoji when it arrives at all.
fn floor_char_boundary(s: &str, limit: usize) -> &str {
    if s.len() <= limit {
        return s;
    }
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The final extension of a sanitized name, lowercased. `None` when there is no
/// extension, or when the "extension" is the whole name (`.bashrc` has already
/// become `_.bashrc` by this point, so this only sees real ones).
pub fn extension_of(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// What we refuse to hand to the operating system
// ---------------------------------------------------------------------------

/// Extensions that mean "run me" on some platform the file could reach.
///
/// The list is macOS-first because that is what Mach ships on, but it is not
/// macOS-only: a saved attachment gets forwarded, synced and opened elsewhere,
/// and an entry that is inert here costs nothing.
///
/// Scripting formats (`sh`, `py`, `rb`, `js`) are included even though double
/// clicking them usually opens an editor rather than an interpreter, because
/// "usually" depends on what the user has installed and on a LaunchServices
/// database we do not control.
const DANGEROUS_EXTENSIONS: &[&str] = &[
    // macOS
    "app", "command", "workflow", "action", "scpt", "scptd", "applescript", "osascript",
    "terminal", "inetloc", "webloc", "fileloc", "dmg", "pkg", "mpkg", "prefpane", "kext",
    "dylib", "so", "bundle", "saver", "qlgenerator", "component", "plugin",
    // Windows
    "exe", "com", "scr", "bat", "cmd", "pif", "msi", "msp", "msc", "cpl", "dll", "sys",
    "ps1", "ps1xml", "psc1", "psm1", "vb", "vbs", "vbe", "js", "jse", "ws", "wsf", "wsc",
    "wsh", "hta", "reg", "lnk", "url", "chm", "jar", "gadget", "inf", "scf", "shs",
    // Unix-ish
    "sh", "bash", "zsh", "csh", "ksh", "fish", "run", "out", "elf", "bin", "deb", "rpm",
    "appimage", "desktop", "service", "py", "pyc", "pl", "rb", "php", "lua",
    // Archives that some unarchivers auto-execute, and the classic double-click
    // trap: an "extension" that is really a second one.
    "iso", "img", "vhd", "vhdx",
    // Documents that execute. An HTML or SVG attachment opened with the system
    // handler becomes a page in the default browser, with a `file:` origin,
    // running the sender's script — which is the whole reason
    // `render::sanitize` will not let either of them near the reading pane
    // either. A message that wants to show you a web page has a link for that.
    "html", "htm", "xhtml", "shtml", "mhtml", "mht", "svg", "svgz", "xht",
];

/// MIME types that describe a program, whatever the filename says.
///
/// Belt to the extension list's braces: a sender who names the file `notes` and
/// declares it `application/x-mach-binary` is making the same request by
/// another route.
const DANGEROUS_MIME_TYPES: &[&str] = &[
    "application/x-msdownload",
    "application/x-msdos-program",
    "application/x-ms-dos-executable",
    "application/vnd.microsoft.portable-executable",
    "application/x-executable",
    "application/x-mach-binary",
    "application/x-sharedlib",
    "application/x-dosexec",
    "application/x-sh",
    "application/x-shellscript",
    "application/x-csh",
    "application/x-perl",
    "application/x-python-code",
    "application/x-apple-diskimage",
    "application/vnd.apple.installer+xml",
    "application/x-ms-shortcut",
    "application/x-msi",
    "application/javascript",
    "text/javascript",
];

/// Would opening this file with the system handler be handing over execution?
///
/// # The decision
///
/// Mach refuses. It does not warn, it does not offer "open anyway", and it does
/// not quarantine and hope.
///
/// The reasoning is that "open" here means `LaunchServices`, and LaunchServices
/// decides what to do from the extension — the same extension the sender chose.
/// A confirmation dialog on top of that is a dialog the user has already
/// learned to dismiss, and the one time it matters is the one time it will be
/// dismissed fastest, because a malicious attachment is delivered inside a
/// message engineered to make opening it feel routine.
///
/// Refusing costs the user the ability to double-click an installer that
/// arrived by mail, which is a workflow worth breaking. **Saving still works**:
/// the file lands where the user chose it to, in Finder, with the operating
/// system's own quarantine and Gatekeeper checks in front of it. That is a
/// deliberate split — saving is a decision the user makes about a file, opening
/// is a decision they make about *this mail client*, and only one of those
/// should be able to start a process.
pub fn is_dangerous(filename: &str, mime_type: &str) -> bool {
    if let Some(ext) = extension_of(filename) {
        if DANGEROUS_EXTENSIONS.contains(&ext.as_str()) {
            return true;
        }
    }
    let mime = mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    DANGEROUS_MIME_TYPES.contains(&mime.as_str())
}

/// The raster image types an inline `cid:` part is allowed to be.
///
/// Identical to the list `render::sanitize` accepts for `data:` images, and for
/// the same reason: `image/svg+xml` is a document format with script and
/// external-reference capability, and "it is only an `<img>`" is a
/// browser-version-dependent argument. Since the inline path ends with the
/// bytes being spliced into a message frame as a `data:` URL, letting SVG
/// through here would reopen a hole the sanitizer closed.
///
/// The type is decided by **sniffing the bytes**, never by the sender's
/// `Content-Type`, so a part declared `image/png` that is really something else
/// is refused rather than relabelled.
pub fn sniff_raster_image(bytes: &[u8]) -> Option<&'static str> {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const GIF87: &[u8] = b"GIF87a";
    const GIF89: &[u8] = b"GIF89a";
    const BMP: &[u8] = b"BM";
    const ICO: &[u8] = b"\x00\x00\x01\x00";
    const CUR: &[u8] = b"\x00\x00\x02\x00";

    if bytes.starts_with(PNG) {
        return Some("image/png");
    }
    // JPEG: SOI marker, then any of the APPn/COM markers.
    if bytes.len() >= 3 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(GIF87) || bytes.starts_with(GIF89) {
        return Some("image/gif");
    }
    // RIFF container whose form type is WEBP.
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(BMP) {
        return Some("image/bmp");
    }
    if bytes.starts_with(ICO) || bytes.starts_with(CUR) {
        return Some("image/x-icon");
    }
    None
}

/// The conventional extension for a sniffed image type, so the cached bytes can
/// be read back later without a sidecar recording what they are.
pub fn raster_extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/x-icon" => "ico",
        _ => "bin",
    }
}

/// The reverse: what a cached inline image's extension says it is.
pub fn raster_mime(extension: &str) -> Option<&'static str> {
    match extension {
        "png" => Some("image/png"),
        "jpg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        _ => None,
    }
}

/// A `Content-ID` we are willing to look up.
///
/// The same shape `render::sanitize` allows through into `data-mach-cid`, held
/// here independently because this side is what turns the value into a cache
/// key and must not depend on the other side having been careful.
pub fn is_valid_content_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 512
        && raw.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+' | b'%' | b'@')
        })
}

/// Deduplicate a set of display names, so a message with three parts all called
/// `image001.png` does not render three identical chips.
///
/// Cosmetic only — the cache keys off ids, never off this.
pub fn disambiguate(names: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let mut candidate = name.clone();
        let mut n = 2;
        while !seen.insert(candidate.to_lowercase()) {
            candidate = match name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
                _ => format!("{name} ({n})"),
            };
            n += 1;
        }
        out.push(candidate);
    }
    out
}
