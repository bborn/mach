//! HTML sanitizing for untrusted message bodies.
//!
//! # Threat model
//!
//! Everything that reaches this module was written by a stranger who wants to
//! read the user's mail. The output is rendered in a WebView inside a desktop
//! app holding five Google accounts, so a single script injection or a single
//! exfiltration channel is a total compromise. The configuration below is
//! therefore written as an explicit allowlist: [`ammonia::Builder::empty`] is
//! the starting point, never [`ammonia::Builder::new`], so a future ammonia
//! release that widens its defaults cannot widen ours.
//!
//! # Documented decisions
//!
//! * **`data:` URLs.** Denied everywhere except `<img src>`, and there only for
//!   a closed list of raster image MIME types (`png`, `jpeg`, `gif`, `webp`,
//!   `bmp`, `ico`) delivered as `;base64` with a strict base64 payload charset
//!   and a size cap. `image/svg+xml` is *not* on that list: SVG is a document
//!   format with script and external-reference capability, and "it is only an
//!   `<img>`" is a browser-version-dependent argument we refuse to depend on.
//!   `data:` in `href` is denied outright — a `data:text/html` document is a
//!   navigation target, not an image. Raster data images are kept because they
//!   are common in real mail (embedded logos), cost no network request and
//!   therefore leak nothing, and blocking them would make the "remote images
//!   blocked" indicator lie.
//!
//! * **CID / inline images.** `cid:` references point at a MIME part of the
//!   same message. They are not a privacy leak, so they are *not* counted as
//!   blocked remote images and the user should never have to click "load
//!   images" to see them. The WebView cannot resolve `cid:` on its own, so the
//!   reference is moved to `data-mach-cid` and the `src` is replaced with a
//!   transparent placeholder pixel; the UI substitutes the real attachment URL.
//!
//! * **Links.** `href` is preserved (the user needs to see and copy where a
//!   link goes, and the app needs the URL to hand to the system browser) but
//!   only for `http`, `https`, `mailto` and `tel`. Every `<a>` is forced to
//!   `target="_blank"` and `rel="noopener noreferrer nofollow"`, overwriting
//!   whatever the sender asked for. Links are *not* stripped or rewritten into
//!   a custom scheme: the app intercepts navigation and opens the system
//!   browser instead.
//!
//!   `target="_blank"` is not belt-and-braces, whatever this comment used to
//!   claim. It is what makes a click a *new-window* navigation, and inside a
//!   sandboxed message frame that is the only kind the app can see: a
//!   same-frame navigation is refused by the app's own `frame-src` policy
//!   before `ipc::render::link_guard` is consulted. Dropping this would make
//!   every link in every message dead again.
//!
//! * **Remote images.** Rewritten to `data-mach-blocked-src` with a
//!   transparent placeholder `src`, so nothing is loadable until the user opts
//!   in, and re-rendering with [`RenderOptions::allow_remote_images`] restores
//!   them. Every other remote-fetch vector (`srcset`, `background`, `poster`,
//!   `ping`, `<link>`, CSS `url()`) is removed outright rather than deferred,
//!   because there is no UI affordance for them.
//!
//! * **Tracking pixels.** When remote images *are* allowed — which is now the
//!   default, because a mail client that hides the pictures is not showing you
//!   your mail — the images that are not pictures are removed anyway. See
//!   [`block_trackers`] for the heuristics, and for what they deliberately do
//!   not claim.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ammonia::{Builder, UrlRelative};
use url::Url;

/// 1x1 fully transparent GIF. Used so a blocked or deferred image keeps its
/// box (and its `width`/`height`) instead of rendering a broken-image icon.
pub const PLACEHOLDER_PIXEL: &str =
    "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

/// Bodies larger than this are truncated before parsing. Real mail does not
/// reach it; a body designed to exhaust memory does.
pub const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// Largest `data:` image payload we are willing to inline.
pub const MAX_DATA_URI_BYTES: usize = 4 * 1024 * 1024;

/// How many images of each kind the sanitizer saw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImageCounts {
    /// Remote (`http`/`https`) images that were deferred behind "load images"
    /// and can still be loaded on demand. Only non-zero when the caller asked
    /// for every remote image to be blocked.
    pub blocked_remote: usize,
    /// Images judged to be tracking pixels and dropped outright. These are not
    /// loadable afterwards and there is no affordance to load them: the whole
    /// point is that they were never anything a reader wanted to see.
    pub blocked_trackers: usize,
    /// `cid:` references to parts of this same message.
    pub inline_cid: usize,
    /// Accepted `data:` raster images.
    pub inline_data: usize,
}

impl ImageCounts {
    pub fn add(&mut self, other: ImageCounts) {
        self.blocked_remote += other.blocked_remote;
        self.blocked_trackers += other.blocked_trackers;
        self.inline_cid += other.inline_cid;
        self.inline_data += other.inline_data;
    }
}

// ---------------------------------------------------------------------------
// Allowlists
// ---------------------------------------------------------------------------

/// Elements that may appear in the output. Everything absent is dropped; for
/// most of them the children are kept (so a stray `<font>` does not eat a
/// paragraph), for the ones in [`CONTENT_DROPPED_TAGS`] the subtree goes too.
const ALLOWED_TAGS: &[&str] = &[
    "a", "abbr", "acronym", "address", "area", "b", "bdi", "bdo", "big", "blockquote", "br",
    "caption", "center", "cite", "code", "col", "colgroup", "dd", "del", "dfn", "dir", "div", "dl",
    "dt", "em", "figcaption", "figure", "font", "h1", "h2", "h3", "h4", "h5", "h6", "hr", "i",
    "img", "ins", "kbd", "label", "legend", "li", "main", "mark", "nobr", "ol", "p", "pre", "q",
    "rp", "rt", "ruby", "s", "samp", "section", "small", "span", "strike", "strong", "sub", "sup",
    "table", "tbody", "td", "tfoot", "th", "thead", "time", "tr", "tt", "u", "ul", "var", "wbr",
];

/// Elements whose *contents* are dropped along with the element. These are the
/// ones whose children are never display text, or whose parsing mode (RAWTEXT,
/// RCDATA, foreign content) makes leaving the children behind a mutation-XSS
/// risk once the tree is re-serialized into the HTML namespace.
const CONTENT_DROPPED_TAGS: &[&str] = &[
    "applet", "audio", "base", "basefont", "body", "button", "canvas", "datalist", "embed",
    "frame", "frameset", "head", "html", "iframe", "input", "keygen", "link", "map", "marquee",
    "math", "menu", "meta", "meter", "noembed", "noframes", "noscript", "object", "optgroup",
    "option", "output", "param", "plaintext", "progress", "script", "select", "slot", "source",
    "style", "svg", "template", "textarea", "title", "track", "video", "xmp",
];

/// Attributes allowed on any element.
const GENERIC_ATTRS: &[&str] = &["align", "dir", "lang", "style", "title", "valign"];

/// URL-bearing attribute names. Any attribute with one of these names that is
/// not explicitly handled by the attribute filter is dropped, so adding a tag
/// to [`ALLOWED_TAGS`] can never accidentally open a fetch channel.
const URL_ATTRS: &[&str] = &[
    "action",
    "archive",
    "background",
    "cite",
    "classid",
    "codebase",
    "data",
    "dynsrc",
    "formaction",
    "href",
    "icon",
    "longdesc",
    "lowsrc",
    "manifest",
    "poster",
    "profile",
    "ping",
    "src",
    "srcdoc",
    "srcset",
    "usemap",
    "xlink:href",
];

/// Schemes a link may point at. Note this is checked with a real URL parser,
/// not string matching: `java\tscript:` and `&#106;avascript:` both parse to
/// scheme `javascript` and are rejected here, whereas a substring check for
/// `"javascript:"` would miss them.
const LINK_SCHEMES: &[&str] = &["http", "https", "mailto", "tel"];

/// MIME types accepted in a `data:` image. Deliberately no `image/svg+xml`.
const DATA_IMAGE_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/jpg",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/x-icon",
    "image/vnd.microsoft.icon",
];

/// CSS properties allowed in a `style` attribute. Presentational only: nothing
/// that can fetch a resource (`background-image`, `list-style-image`,
/// `border-image`, `content`, `cursor`, `filter`), nothing that can move
/// content out of its container or hide it from the reader (`position`, `top`,
/// `left`, `z-index`, `transform`, `opacity`, `visibility`, `clip-path`,
/// `pointer-events`, `mix-blend-mode`), and nothing that can run
/// (`behavior`, `-moz-binding`, `expression()` lives in a value but the value
/// scrubber catches it).
const CSS_PROPERTIES: &[&str] = &[
    "background",
    "background-color",
    "border",
    "border-bottom",
    "border-bottom-color",
    "border-bottom-left-radius",
    "border-bottom-right-radius",
    "border-bottom-style",
    "border-bottom-width",
    "border-collapse",
    "border-color",
    "border-left",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-radius",
    "border-right",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-spacing",
    "border-style",
    "border-top",
    "border-top-color",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-top-style",
    "border-top-width",
    "border-width",
    "caption-side",
    "clear",
    "color",
    "direction",
    "display",
    "empty-cells",
    "float",
    "font",
    "font-family",
    "font-size",
    "font-style",
    "font-variant",
    "font-weight",
    "height",
    "letter-spacing",
    "line-height",
    "list-style-type",
    "margin",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "overflow",
    "overflow-wrap",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "table-layout",
    "text-align",
    "text-decoration",
    "text-indent",
    "text-transform",
    "vertical-align",
    "white-space",
    "width",
    "word-break",
    "word-spacing",
    "word-wrap",
];

/// Substrings that disqualify a whole CSS declaration.
///
/// This runs *before* ammonia's cssparser-based property filter, and catches
/// the things a property allowlist alone does not: a URL smuggled into an
/// allowed property (`background: url(...)`), a CSS escape used to rebuild a
/// banned token (`\75 rl(...)`), a comment used to split one (`ur/**/l(...)`),
/// and legacy IE script vectors. Any declaration containing one of these is
/// dropped whole rather than repaired, because repairing attacker CSS is a
/// game we would lose.
const CSS_FORBIDDEN: &[&str] = &[
    "\\", "/*", "*/", "@", "&#", "<", ">", "url", "expression", "javascript", "vbscript",
    "behavior", "binding", "image-set", "element(", "attr(", "var(", "--", "progid", "\u{0}",
];

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Sanitize one HTML fragment.
///
/// Returns the safe HTML and what was done to the images inside it. When
/// `allow_remote_images` is false (the default for an unopened message) every
/// `http`/`https` image is moved to `data-mach-blocked-src`; when it is true
/// they are passed through untouched. Nothing else changes between the two
/// modes — opting into images must never opt into anything else.
pub fn sanitize_fragment(html: &str, allow_remote_images: bool) -> (String, ImageCounts) {
    let html = truncate_on_char_boundary(html, MAX_INPUT_BYTES);

    let blocked = Arc::new(AtomicUsize::new(0));
    let cid = Arc::new(AtomicUsize::new(0));
    let data = Arc::new(AtomicUsize::new(0));

    // Marker prefixes carry a per-call nonce so that no amount of attacker text
    // in the body can forge the post-pass that turns them into data attributes.
    let nonce = nonce();
    let blocked_marker = format!("mach-blk-{nonce}:");
    let cid_marker = format!("mach-cid-{nonce}:");

    let out = {
        let (blocked, cid, data) = (blocked.clone(), cid.clone(), data.clone());
        let (blk, cidm) = (blocked_marker.clone(), cid_marker.clone());

        let mut builder = Builder::empty();
        builder
            .tags(ALLOWED_TAGS.iter().copied().collect())
            .clean_content_tags(CONTENT_DROPPED_TAGS.iter().copied().collect())
            .generic_attributes(GENERIC_ATTRS.iter().copied().collect())
            .generic_attribute_prefixes(HashSet::new())
            .tag_attributes(tag_attributes())
            .tag_attribute_values(HashMap::new())
            .set_tag_attribute_values(forced_link_attributes())
            .url_schemes(
                // `data` and `cid` are here only so the attribute filter below
                // gets a chance to inspect them; ammonia's scheme check runs
                // first and would otherwise drop them before we see them.
                ["http", "https", "mailto", "tel", "cid", "data"]
                    .into_iter()
                    .collect(),
            )
            // Relative URLs in mail have no meaningful base. Resolved against
            // the WebView they would point at the app's own origin, so deny.
            .url_relative(UrlRelative::Deny)
            // A sender-supplied `rel` is already gone by this point (it is not
            // in the allowlist), so this appends exactly one. It is kept
            // separate from `target` above because ammonia takes the forced
            // attributes as a `HashMap`, whose iteration order would otherwise
            // shuffle `target` and `rel` between renders of the same body.
            .link_rel(Some("noopener noreferrer nofollow"))
            .allowed_classes(HashMap::new())
            .strip_comments(true)
            .id_prefix(None)
            .filter_style_properties(CSS_PROPERTIES.iter().copied().collect())
            .attribute_filter(move |element, attribute, value| {
                filter_attribute(
                    element,
                    attribute,
                    value,
                    allow_remote_images,
                    &blk,
                    &cidm,
                    &blocked,
                    &cid,
                    &data,
                )
            });

        builder.clean(&html).to_string()
    };

    // Tracking pixels, before the markers are promoted: the pass needs to see
    // one `src` per image whether or not it is carrying a marker, and a tracker
    // it rewrites must not then be promoted into a loadable blocked image.
    //
    // It runs only when remote images are allowed, which is the only mode where
    // there is anything left to block. Under "block everything" the answer is
    // already no, and re-labelling half of those images as trackers would move
    // them out of the count the "load images" button is about.
    let (out, trackers, reclaimed) = if allow_remote_images {
        block_trackers(&out, &blocked_marker)
    } else {
        (out, 0, 0)
    };

    // The attribute filter cannot rename or add attributes, so it encodes its
    // decision in the value behind an unguessable marker and we finish the job
    // here, on ammonia's own well-formed, attribute-escaped output.
    let out = promote_marker(&out, &blocked_marker, "data-mach-blocked-src");
    let out = promote_marker(&out, &cid_marker, "data-mach-cid");

    let counts = ImageCounts {
        blocked_remote: blocked.load(Ordering::Relaxed).saturating_sub(reclaimed),
        blocked_trackers: trackers,
        inline_cid: cid.load(Ordering::Relaxed),
        inline_data: data.load(Ordering::Relaxed),
    };
    (out, counts)
}

fn tag_attributes() -> HashMap<&'static str, HashSet<&'static str>> {
    let mut m: HashMap<&'static str, HashSet<&'static str>> = HashMap::new();
    let mut set = |tag: &'static str, attrs: &[&'static str]| {
        m.insert(tag, attrs.iter().copied().collect());
    };
    // No `name`: a legacy `<a name>` anchor buys nothing in a mail body and
    // named elements are a DOM-clobbering primitive against the UI's own
    // script. Same reasoning applies to `id` and `class`, which are allowed
    // nowhere.
    set("a", &["href"]);
    set("blockquote", &["type"]);
    set("col", &["span", "width"]);
    set("colgroup", &["span", "width"]);
    set("del", &["datetime"]);
    set("font", &["color", "face", "size"]);
    set("hr", &["noshade", "size", "width"]);
    set("img", &["alt", "border", "height", "hspace", "src", "vspace", "width"]);
    set("ins", &["datetime"]);
    set("li", &["type", "value"]);
    set("ol", &["reversed", "start", "type"]);
    set("table", &["bgcolor", "border", "cellpadding", "cellspacing", "height", "summary", "width"]);
    set("tbody", &["bgcolor"]);
    set("td", &["bgcolor", "colspan", "height", "nowrap", "rowspan", "width"]);
    set("tfoot", &["bgcolor"]);
    set("th", &["bgcolor", "colspan", "height", "nowrap", "rowspan", "scope", "width"]);
    set("thead", &["bgcolor"]);
    set("time", &["datetime"]);
    set("tr", &["bgcolor", "height"]);
    set("ul", &["type"]);
    m
}

/// `target` is *set*, never allowlisted, so a sender-supplied value is dropped
/// by the attribute pass and replaced by ours. `rel` is added by `link_rel`.
fn forced_link_attributes() -> HashMap<&'static str, HashMap<&'static str, &'static str>> {
    let mut a: HashMap<&'static str, &'static str> = HashMap::new();
    a.insert("target", "_blank");
    let mut m = HashMap::new();
    m.insert("a", a);
    m
}

// ---------------------------------------------------------------------------
// Attribute filtering
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn filter_attribute<'u>(
    element: &str,
    attribute: &str,
    value: &'u str,
    allow_remote_images: bool,
    blocked_marker: &str,
    cid_marker: &str,
    blocked: &AtomicUsize,
    cid: &AtomicUsize,
    data: &AtomicUsize,
) -> Option<Cow<'u, str>> {
    let attr = attribute.to_ascii_lowercase();

    // Never let a sender pre-arm our own machinery: an attacker-supplied
    // `data-mach-blocked-src` would be honoured by the "load images" button,
    // and an attacker-supplied `data-mach-cid` would aim the attachment
    // resolver at a part of a different message.
    if attr.starts_with("data-mach") {
        return None;
    }

    if attr == "style" {
        let mut cleaned = sanitize_style(value);
        // `visibility` and `opacity` are not allowed properties, so an image
        // the sender hid with either would arrive at the tracker pass looking
        // perfectly visible — and, worse, would *become* visible in the reader's
        // pane. Fold both into the one property that survives the allowlist.
        // Restricted to `img` because for ordinary elements the existing
        // behaviour (show it) is the one the reader wants.
        if element == "img" && style_hides_element(value) && !style_declares_hidden(&cleaned) {
            if !cleaned.is_empty() {
                cleaned.push(';');
            }
            cleaned.push_str("display:none");
        }
        return if cleaned.is_empty() {
            None
        } else {
            Some(Cow::Owned(cleaned))
        };
    }

    if element == "img" && attr == "src" {
        return filter_image_src(
            value,
            allow_remote_images,
            blocked_marker,
            cid_marker,
            blocked,
            cid,
            data,
        );
    }

    if element == "a" && attr == "href" {
        return match Url::parse(value.trim()) {
            // Emit the parser's own normalized serialization rather than the
            // sender's bytes. It percent-encodes `"`, `'`, `<`, `>` and spaces,
            // so the URL stays inert even if a later layer does something
            // careless with it (interpolating it into markup, logging it into
            // an HTML view).
            Ok(url) if LINK_SCHEMES.contains(&url.scheme()) => Some(Cow::Owned(url.into())),
            _ => None,
        };
    }

    // Default-deny for anything else that could name a resource.
    if URL_ATTRS.contains(&attr.as_str()) {
        return None;
    }

    Some(Cow::Borrowed(value))
}

fn filter_image_src<'u>(
    value: &'u str,
    allow_remote_images: bool,
    blocked_marker: &str,
    cid_marker: &str,
    blocked: &AtomicUsize,
    cid: &AtomicUsize,
    data: &AtomicUsize,
) -> Option<Cow<'u, str>> {
    let trimmed = value.trim();
    let url = Url::parse(trimmed).ok()?;
    match url.scheme() {
        "http" | "https" => {
            // Normalized form, for the same reason as `href`: whatever ends up
            // in `data-mach-blocked-src` is handed back to the UI when the user
            // clicks "load images", so it must not carry quotes or angle
            // brackets no matter how the UI uses it.
            let normalized = url.as_str();
            if allow_remote_images {
                Some(Cow::Owned(normalized.to_string()))
            } else {
                blocked.fetch_add(1, Ordering::Relaxed);
                Some(Cow::Owned(format!("{blocked_marker}{normalized}")))
            }
        }
        "cid" => {
            // `Url` percent-encodes an opaque path, which would break the
            // Content-ID lookup, so take the raw text after the scheme — and
            // therefore constrain it by hand. A Content-ID is an addr-spec;
            // anything outside that shape is a smuggling attempt, not a
            // reference we could resolve anyway.
            let raw = trimmed
                .split_once(':')
                .map(|(_, rest)| rest.trim())
                .unwrap_or_default();
            let raw = raw.strip_prefix('<').unwrap_or(raw);
            let raw = raw.strip_suffix('>').unwrap_or(raw);
            if raw.is_empty()
                || raw.len() > 512
                || !raw
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+' | b'%' | b'@'))
            {
                return None;
            }
            cid.fetch_add(1, Ordering::Relaxed);
            Some(Cow::Owned(format!("{cid_marker}{raw}")))
        }
        "data" if is_safe_data_image(trimmed) => {
            data.fetch_add(1, Ordering::Relaxed);
            Some(Cow::Borrowed(trimmed))
        }
        _ => None,
    }
}

/// A `data:` URL is acceptable only as `data:<raster mime>;base64,<base64>`.
///
/// Everything is checked positively: the MIME type against a closed list, the
/// payload against the base64 alphabet, and the length against a cap. Anything
/// that does not match exactly — `image/svg+xml`, `text/html`, a non-base64
/// payload, a stray parameter — is rejected.
fn is_safe_data_image(value: &str) -> bool {
    if value.len() > MAX_DATA_URI_BYTES {
        return false;
    }
    let Some(rest) = strip_prefix_ci(value, "data:") else {
        return false;
    };
    let Some((meta, payload)) = rest.split_once(',') else {
        return false;
    };
    let meta = meta.trim().to_ascii_lowercase();
    let Some(mime) = meta.strip_suffix(";base64") else {
        return false;
    };
    if !DATA_IMAGE_MIMES.contains(&mime) {
        return false;
    }
    !payload.is_empty()
        && payload
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'\n' | b'\r'))
}

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

/// First of two CSS layers. Splits the declaration list crudely, drops any
/// declaration whose text contains a forbidden token, lowercases surviving
/// property names, and drops properties outside the allowlist. Ammonia's
/// cssparser pass then re-parses and re-filters what is left, so a declaration
/// has to survive a substring scrubber *and* a real CSS parser to be emitted.
///
/// The split on `;` is naive on purpose. A `;` inside a quoted value produces
/// two nonsense fragments rather than one valid declaration, which fails
/// closed: the fragments are dropped by the property allowlist, and any
/// dangerous token still lands in one of them.
fn sanitize_style(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for decl in value.split(';') {
        let decl = decl.trim();
        if decl.is_empty() {
            continue;
        }
        let lower = decl.to_ascii_lowercase();
        if CSS_FORBIDDEN.iter().any(|bad| lower.contains(bad)) {
            continue;
        }
        let Some((name, val)) = decl.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let val = val.trim();
        if val.is_empty() || !CSS_PROPERTIES.contains(&name.as_str()) {
            continue;
        }
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(&name);
        out.push(':');
        out.push_str(val);
    }
    out
}

// ---------------------------------------------------------------------------
// Tracking pixels
// ---------------------------------------------------------------------------

/// At or below this, in either dimension, an image is not a picture.
///
/// 1×1 is the classic, but senders pad: 2×2 and 3×3 turn up, and so does 1×3
/// used as a "spacer". Nothing a reader is meant to look at is three pixels
/// wide, so the line costs no false positives worth the name.
const TINY_PX: f64 = 3.0;

/// Path segments that mean "this endpoint records that you opened the mail".
/// Matched as *whole segments*, never as substrings, so `/images/opengraph.png`
/// is not caught by `open`.
const TRACKER_SEGMENTS: &[&str] = &[
    "open",
    "opens",
    "opened",
    "openpixel",
    "pixel",
    "pixels",
    "px",
    "track",
    "tracks",
    "tracked",
    "tracking",
    "trk",
    "beacon",
    "beacons",
    "impression",
    "impressions",
    "imp",
    "wf",
    "collect",
    "spacer",
    "1x1",
];

/// Filename stems (the last segment with its extension removed) that mean the
/// same thing. Kept separate from [`TRACKER_SEGMENTS`] because a *file* called
/// `blank.gif` is a tracker whereas a *directory* called `blank` is nothing.
const TRACKER_STEMS: &[&str] = &[
    "open",
    "pixel",
    "px",
    "track",
    "beacon",
    "spacer",
    "blank",
    "clear",
    "transparent",
    "trans",
    "1x1",
    "dot",
];

/// Remove the images that are not pictures.
///
/// # Why this exists
///
/// Blocking every remote image blocks every tracker, and also blocks the
/// product photo, the logo and the chart — so the reader clicks "load images"
/// on everything and the protection evaporates along with the readability.
/// Almost all of the read-receipt value in mail is carried by images that were
/// never meant to be seen, and those are identifiable by shape.
///
/// # The heuristics
///
/// An `<img>` is a tracker when any of these hold:
///
/// 1. **It is tiny.** A declared `width` or `height` — attribute or `style` —
///    of at most [`TINY_PX`].
/// 2. **It is hidden.** `display:none`. `visibility:hidden` and `opacity:0`
///    reach this point as `display:none` too: the CSS allowlist drops both
///    properties, so [`sanitize_style`] folds them into the one property that
///    survives, for `img` only.
/// 3. **It is shapeless and the URL is an open-tracking shape.** No declared
///    dimensions at all, plus a path segment or filename stem from the lists
///    above.
///
/// # What this is not
///
/// It is not a security boundary and it is not exhaustive. A tracker served
/// from `/logo.png` at 600×400 and cropped by CSS is not caught, and cannot be:
/// distinguishing it from a logo requires loading it, which is the thing we are
/// trying not to do. `BLOCK_ALL_REMOTE_IMAGES` on the frontend remains the
/// answer for a reader who wants a guarantee rather than a heuristic.
///
/// Nor is it a parser. It runs over ammonia's *output*, which is a
/// re-serialization of a parsed DOM: every attribute value is quoted and `"`
/// inside one is escaped, so reading to the next `"` is unambiguous. The same
/// assumption [`promote_marker`] already makes.
fn block_trackers(html: &str, blocked_marker: &str) -> (String, usize, usize) {
    if !html.contains("<img") {
        return (html.to_string(), 0, 0);
    }

    let mut out = String::with_capacity(html.len());
    let mut trackers = 0usize;
    let mut reclaimed = 0usize;
    let mut cursor = 0usize;

    while let Some(rel) = html[cursor..].find("<img") {
        let start = cursor + rel;
        let after_name = start + "<img".len();
        // `<image>` and `<imgfoo>` are not `<img>`.
        if !html[after_name..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_whitespace() || c == '>' || c == '/')
        {
            out.push_str(&html[cursor..after_name]);
            cursor = after_name;
            continue;
        }

        let Some(tag) = scan_tag(html, after_name) else {
            // Unterminated: emit the rest verbatim rather than guess.
            break;
        };

        let src = tag.value(html, "src").unwrap_or("");
        let url = src.strip_prefix(blocked_marker).unwrap_or(src);
        let was_deferred = src.len() != url.len();

        out.push_str(&html[cursor..after_name]);
        if is_tracker(&tag, html, url) {
            trackers += 1;
            if was_deferred {
                reclaimed += 1;
            }
            out.push_str(" data-mach-tracker=\"\"");
            // Everything but the `src` is kept: `alt` still describes it, and
            // the frame stylesheet is what makes the box disappear.
            match tag.span("src") {
                Some((from, to)) => {
                    out.push_str(&html[after_name..from]);
                    out.push_str(PLACEHOLDER_PIXEL);
                    out.push_str(&html[to..tag.end]);
                }
                None => out.push_str(&html[after_name..tag.end]),
            }
        } else {
            out.push_str(&html[after_name..tag.end]);
        }
        cursor = tag.end;
    }

    out.push_str(&html[cursor..]);
    (out, trackers, reclaimed)
}

/// One parsed start tag: the byte ranges of each attribute value, and where the
/// tag ends. Values are borrowed from the source by range rather than copied so
/// the rewrite above can splice around them.
struct Tag {
    /// Name, plus the range of the quoted value (exclusive of the quotes).
    attrs: Vec<(String, usize, usize)>,
    /// One past the tag's closing `>`.
    end: usize,
}

impl Tag {
    fn span(&self, name: &str) -> Option<(usize, usize)> {
        self.attrs
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, a, b)| (*a, *b))
    }

    fn value<'h>(&self, html: &'h str, name: &str) -> Option<&'h str> {
        self.span(name).map(|(a, b)| &html[a..b])
    }
}

/// Parse a start tag's attributes, beginning just after the tag name.
///
/// Deliberately narrow: it accepts exactly what html5ever's serializer emits
/// (whitespace-separated `name="value"`, always quoted, `"` escaped inside).
/// Anything else returns `None` and the tag is left untouched, which fails in
/// the direction of showing an image rather than eating one.
fn scan_tag(html: &str, from: usize) -> Option<Tag> {
    let bytes = html.as_bytes();
    let mut i = from;
    let mut attrs = Vec::new();

    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b'>' {
            return Some(Tag { attrs, end: i + 1 });
        }
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'>') {
            return Some(Tag { attrs, end: i + 2 });
        }

        let name_start = i;
        while i < bytes.len() && !matches!(bytes[i], b'=' | b'>' | b'/') && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == name_start || i >= bytes.len() {
            return None;
        }
        let name = html[name_start..i].to_ascii_lowercase();

        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            // A bare attribute. Record it with an empty value and carry on.
            attrs.push((name, i, i));
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'"') {
            return None;
        }
        i += 1;
        let value_start = i;
        let rel = html[i..].find('"')?;
        let value_end = i + rel;
        attrs.push((name, value_start, value_end));
        i = value_end + 1;
    }
}

fn is_tracker(tag: &Tag, html: &str, url: &str) -> bool {
    let style = tag.value(html, "style").unwrap_or("");
    let width = declared_length(tag.value(html, "width"), style, "width");
    let height = declared_length(tag.value(html, "height"), style, "height");

    if width.is_some_and(|w| w <= TINY_PX) || height.is_some_and(|h| h <= TINY_PX) {
        return true;
    }
    if style_declares_hidden(style) {
        return true;
    }
    width.is_none() && height.is_none() && looks_like_tracker_url(url)
}

/// The image's declared size on one axis.
///
/// The `style` is consulted first because that is the one that wins on screen:
/// `<img width="600" style="width:1px">` renders as a pixel, whatever the
/// attribute claims, and it is the rendered size that decides whether this is
/// something the reader was meant to see.
///
/// A percentage says nothing about pixels, so it yields `None` and a
/// `width="100%"` image is judged on its URL like any other shapeless one.
fn declared_length(attribute: Option<&str>, style: &str, property: &str) -> Option<f64> {
    for decl in style.split(';') {
        // Not `?`: a declaration without a colon is a malformed fragment to skip,
        // not a reason to abandon the ones after it.
        let Some((name, value)) = decl.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(property) {
            if let Some(px) = parse_px(value) {
                return Some(px);
            }
        }
    }
    attribute.and_then(parse_px)
}

/// `"1"`, `"1px"`, `" 1 "` → `1.0`. `"100%"`, `"auto"`, `""` → `None`.
fn parse_px(value: &str) -> Option<f64> {
    let v = value.trim().to_ascii_lowercase();
    let v = v.strip_suffix("px").unwrap_or(&v).trim();
    if v.is_empty() {
        return None;
    }
    v.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0)
}

fn style_declares_hidden(style: &str) -> bool {
    style.split(';').any(|decl| {
        decl.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("display") && value.trim().eq_ignore_ascii_case("none")
        })
    })
}

/// Does the sender's *raw* style — before the property allowlist eats most of
/// it — say "do not show this to the reader"?
///
/// `visibility` and `opacity` are not allowed properties, so by the time
/// anything downstream looks at the style they are gone. This runs on the value
/// as written.
fn style_hides_element(raw: &str) -> bool {
    raw.split(';').any(|decl| {
        let Some((name, value)) = decl.split_once(':') else {
            return false;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_ascii_lowercase();
        let value = value.split('!').next().unwrap_or("").trim();
        match name.as_str() {
            "display" => value == "none",
            "visibility" => value == "hidden" || value == "collapse",
            "opacity" => parse_px(value).is_some_and(|n| n <= 0.0),
            "width" | "height" | "max-width" | "max-height" => {
                parse_px(value).is_some_and(|n| n <= TINY_PX)
            }
            _ => false,
        }
    })
}

fn looks_like_tracker_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let path = parsed.path().to_ascii_lowercase();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.iter().any(|s| TRACKER_SEGMENTS.contains(s)) {
        return true;
    }
    let Some(last) = segments.last() else {
        return false;
    };
    let stem = last.rsplit_once('.').map_or(*last, |(stem, _)| stem);
    // Deliberately not "a one-letter filename": `a.png` and `l.png` are real
    // images on real CDNs, and hiding somebody's picture to catch a pixel is
    // the wrong trade for a mail client whose whole complaint was that it
    // hid too much.
    TRACKER_STEMS.contains(&stem)
}

// ---------------------------------------------------------------------------
// Marker promotion
// ---------------------------------------------------------------------------

/// Turn `src="<marker><value>"` into `<new_attr>="<value>" src="<placeholder>"`.
///
/// This runs on ammonia's output, which is a re-serialization of a parsed DOM:
/// attribute values have `&`, `"` and NBSP escaped, so the first `"` after the
/// marker is unambiguously the end of the value. The marker itself carries a
/// per-call nonce, so attacker-controlled text cannot produce a match — and
/// even a match in a text node would only rewrite text into other text.
fn promote_marker(html: &str, marker: &str, new_attr: &str) -> String {
    let needle = format!("src=\"{marker}");
    if !html.contains(&needle) {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html;
    while let Some(i) = rest.find(&needle) {
        out.push_str(&rest[..i]);
        let after = &rest[i + needle.len()..];
        let Some(end) = after.find('"') else {
            out.push_str(&rest[i..]);
            return out;
        };
        out.push_str(new_attr);
        out.push_str("=\"");
        out.push_str(&after[..end]);
        out.push_str("\" src=\"");
        out.push_str(PLACEHOLDER_PIXEL);
        out.push('"');
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Plain text
// ---------------------------------------------------------------------------

/// Render a `text/plain` body as safe HTML.
///
/// Escape first, link second, and never the other way around: the autolinker
/// works on the *raw* text, emits HTML for each span itself, and validates
/// every candidate through [`Url::parse`] before writing an `href`. A URL
/// candidate also terminates at any of `" ' < > \``, so an attacker cannot walk
/// the scanner past the closing quote of the attribute it is about to write.
pub fn text_to_html(text: &str) -> String {
    let text = truncate_on_char_boundary(text, MAX_INPUT_BYTES);
    let mut out = String::with_capacity(text.len() + 64);
    // `white-space: pre-wrap` preserves runs of spaces and tabs; the newlines
    // themselves become <br> so the two do not double up.
    out.push_str("<div style=\"white-space:pre-wrap\">");
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push_str("<br>");
        }
        push_linked_line(&mut out, line.strip_suffix('\r').unwrap_or(line));
    }
    out.push_str("</div>");
    out
}

fn push_linked_line(out: &mut String, line: &str) {
    let mut cursor = 0usize;
    while let Some(found) = find_link(line, cursor) {
        push_escaped(out, &line[cursor..found.start]);
        out.push_str("<a href=\"");
        push_escaped(out, &found.href);
        out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
        push_escaped(out, &line[found.start..found.end]);
        out.push_str("</a>");
        cursor = found.end;
    }
    push_escaped(out, &line[cursor..]);
}

struct Link {
    start: usize,
    end: usize,
    href: String,
}

/// Characters that end a URL candidate. The quote characters are the important
/// ones: they are what an attacker would need to close the `href` attribute we
/// are about to open.
const URL_TERMINATORS: &[char] = &[
    '"', '\'', '<', '>', '`', ' ', '\t', '\r', '\n', '\u{0}', '\u{a0}', '\u{2028}', '\u{2029}',
];

fn find_link(line: &str, from: usize) -> Option<Link> {
    let hay = &line[from..];
    let lower = hay.to_ascii_lowercase();

    let mut best: Option<Link> = None;
    let mut consider = |link: Link| {
        if best.as_ref().is_none_or(|b| link.start < b.start) {
            best = Some(link);
        }
    };

    for (prefix, needs_scheme) in [("http://", false), ("https://", false), ("www.", true)] {
        let mut at = 0usize;
        while let Some(rel) = lower[at..].find(prefix) {
            let start = at + rel;
            at = start + 1;
            if !starts_token(hay, start) {
                continue;
            }
            let Some((end, text)) = url_extent(hay, start) else {
                continue;
            };
            let candidate = if needs_scheme {
                format!("https://{text}")
            } else {
                text.to_string()
            };
            let Ok(url) = Url::parse(&candidate) else {
                continue;
            };
            if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
                continue;
            }
            consider(Link {
                start: from + start,
                end: from + end,
                href: url.as_str().to_string(),
            });
            break;
        }
    }

    if let Some(link) = find_email(hay, from) {
        consider(link);
    }

    best
}

/// A candidate must start at a token boundary, so `xhttp://x` and
/// `mailto:http://x` do not become links.
fn starts_token(hay: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let prev = hay[..start].chars().next_back().unwrap();
    !prev.is_alphanumeric() && !matches!(prev, '.' | '-' | '_' | '+' | '/' | ':' | '@' | '%')
}

/// Extent of a URL candidate, with trailing sentence punctuation and unbalanced
/// closing brackets trimmed off.
fn url_extent(hay: &str, start: usize) -> Option<(usize, &str)> {
    let mut end = hay.len();
    for (i, c) in hay[start..].char_indices() {
        if URL_TERMINATORS.contains(&c) || c.is_control() {
            end = start + i;
            break;
        }
    }
    let mut text = &hay[start..end];
    loop {
        let Some(last) = text.chars().next_back() else {
            return None;
        };
        let trim = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' => true,
            ')' => text.matches('(').count() < text.matches(')').count(),
            ']' => text.matches('[').count() < text.matches(']').count(),
            '}' => text.matches('{').count() < text.matches('}').count(),
            _ => false,
        };
        if !trim {
            break;
        }
        text = &text[..text.len() - last.len_utf8()];
    }
    if text.is_empty() {
        return None;
    }
    Some((start + text.len(), text))
}

fn find_email(hay: &str, offset: usize) -> Option<Link> {
    let at = hay.find('@')?;
    let is_local = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-');
    let is_domain = |c: char| c.is_alphanumeric() || matches!(c, '.' | '-');

    let mut start = at;
    for (i, c) in hay[..at].char_indices().rev() {
        if is_local(c) {
            start = i;
        } else {
            break;
        }
    }
    let mut end = at + 1;
    for (i, c) in hay[at + 1..].char_indices() {
        if is_domain(c) {
            end = at + 1 + i + c.len_utf8();
        } else {
            break;
        }
    }
    let local = &hay[start..at];
    let mut domain = &hay[at + 1..end];
    while domain.ends_with('.') || domain.ends_with('-') {
        domain = &domain[..domain.len() - 1];
    }
    if local.is_empty() || !domain.contains('.') || domain.starts_with('.') {
        return None;
    }
    if !starts_token(hay, start) {
        return None;
    }
    let addr = format!("{local}@{domain}");
    let url = Url::parse(&format!("mailto:{addr}")).ok()?;
    if url.scheme() != "mailto" {
        return None;
    }
    Some(Link {
        start: offset + start,
        end: offset + at + 1 + domain.len(),
        href: format!("mailto:{addr}"),
    })
}

/// Escape for both text and attribute contexts. `'` and `"` are escaped even in
/// text position so a single routine is safe everywhere it is called.
pub fn push_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\u{0}' => out.push_str("\u{fffd}"),
            _ => out.push(c),
        }
    }
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    push_escaped(&mut out, s);
    out
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

pub(crate) fn truncate_on_char_boundary(s: &str, max: usize) -> Cow<'_, str> {
    if s.len() <= max {
        return Cow::Borrowed(s);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Borrowed(&s[..end])
}

/// Per-call unguessable marker suffix. `RandomState` is seeded from the OS at
/// process start and its keys advance per instance, which is enough to keep the
/// marker out of an attacker's reach; nothing here is a secret beyond the
/// lifetime of one `clean` call.
fn nonce() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    let a = h.finish();
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u64(a);
    format!("{a:016x}{:016x}", h2.finish())
}
