//! The editor's output: HTML in, email-safe HTML and a plain-text twin out.
//!
//! # Why this replaced a markdown renderer
//!
//! The composer used to be a `<textarea>` holding markdown-ish source. That made
//! the two parts of a `multipart/alternative` trivially derivable — the source
//! *was* the `text/plain` part, and [`markdown::to_html`](super::markdown) made
//! the `text/html` one. It also made `**bold**` the thing you looked at while
//! writing, which is not what anybody means by a mail composer.
//!
//! The editor now emits HTML, so the derivation runs the other way: this module
//! cleans what the editor produced into something every mail client renders the
//! same, and [`to_text`] reads that back into prose.
//!
//! # What "email-safe" means here, concretely
//!
//! A recipient's client is not a browser. It may be Outlook, which drops most
//! of a stylesheet; it may be a webmail that rewrites the document; it is never
//! a page that has loaded Mach's CSS. So:
//!
//!  * **No classes and no ids.** They name rules that do not exist on the other
//!    side. The one exception is added downstream, not here: `gmail_quote`, in
//!    [`draft::quote_html`](super::draft::quote_html), which every client keys
//!    its "show trimmed content" affordance off.
//!  * **Inline styles only, from a fixed list of properties.** [`STYLE_PROPERTIES`]
//!    is what survives; everything else is dropped, value and all.
//!  * **No modern CSS anywhere in a value.** `var()`, `calc()`, `clamp()`,
//!    `oklch()`, `color-mix()`, `flex`, `grid`, `position` — see
//!    [`is_safe_style_value`]. Outlook's rendering engine is Word's, and a value
//!    it cannot parse does not degrade, it takes the declaration with it.
//!  * **No `<style>`, `<script>`, `<meta>`, `<link>`, no comments.** Word and
//!    Google Docs paste all four. Comments matter more than they look: MSO
//!    conditional comments are how a pasted Word document smuggles a stylesheet
//!    past a naive tag filter.
//!
//! # Paste
//!
//! Squire cleans on paste in the webview, and this cleans again at send. Both,
//! deliberately: the webview's copy is what the user sees while writing, and
//! this one is what actually leaves. A draft can also be written by the agent,
//! or arrive from another client, and neither goes through the editor at all.

use std::collections::HashSet;

/// Tags that may appear in an outgoing message.
///
/// Small on purpose. Everything here renders in Outlook 2016, Apple Mail and
/// Gmail without a stylesheet; `<figure>`, `<section>`, `<details>` and friends
/// are not on the list because their default rendering is a browser convention
/// rather than a mail one.
const ALLOWED_TAGS: &[&str] = &[
    "a", "b", "blockquote", "br", "code", "div", "em", "h1", "h2", "h3", "h4", "h5", "h6", "hr",
    "i", "img", "li", "ol", "p", "pre", "s", "span", "strike", "strong", "sub", "sup", "table",
    "tbody", "td", "tfoot", "th", "thead", "tr", "u", "ul",
];

/// CSS properties an outgoing message may carry inline.
///
/// The test is "does Outlook honour this in 2026", not "is this valid CSS".
/// Layout properties are absent for that reason: a `display` or a `position`
/// that the recipient's client ignores does not fall back to the unstyled
/// document, it falls back to something nobody designed.
const STYLE_PROPERTIES: &[&str] = &[
    "background-color",
    "border-left",
    "color",
    // `font-family` and `font-size` are deliberately absent — see
    // `STYLE_PROPERTIES` in `src/lib/email-html.ts` for the argument. Pasted
    // Calibri at 14pt in the middle of a reply is the "somebody else's fonts"
    // problem that was the original case against a rich-text composer.
    "font-style",
    "font-weight",
    "margin",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "text-align",
    "text-decoration",
    "white-space",
];

/// Substrings that disqualify a declaration outright.
///
/// A blocklist rather than a value grammar because the failure mode is
/// asymmetric: a dropped declaration costs a little styling, and a value the
/// recipient's engine chokes on can cost the paragraph.
const UNSAFE_VALUE_MARKERS: &[&str] = &[
    "var(",
    "calc(",
    "clamp(",
    "min(",
    "max(",
    "env(",
    "oklch(",
    "oklab(",
    "lab(",
    "lch(",
    "color-mix(",
    "color(",
    "url(",
    "expression(",
    "javascript:",
    "!important",
    "@",
];

/// Clean HTML for sending.
///
/// Idempotent: running it over its own output changes nothing, which matters
/// because a draft is rebuilt and re-cleaned on every autosave and every push.
pub fn sanitize(html: &str) -> String {
    let tags: HashSet<&str> = ALLOWED_TAGS.iter().copied().collect();
    let generic: HashSet<&str> = ["style", "dir", "align"].into_iter().collect();
    let schemes: HashSet<&str> = ["http", "https", "mailto", "tel", "cid"].into_iter().collect();

    let mut builder = ammonia::Builder::new();
    builder
        .tags(tags)
        .generic_attributes(generic)
        .url_schemes(schemes)
        // `rel="noopener noreferrer"` is a browser concern. In a message it is
        // one more attribute for a mail client to mangle.
        .link_rel(None)
        // Relative URLs cannot resolve in somebody else's inbox, and an
        // unresolvable `src` is a broken-image icon in the middle of a message.
        .url_relative(ammonia::UrlRelative::Deny)
        .attribute_filter(|_element, attribute, value| match attribute {
            "style" => {
                let cleaned = clean_style(value);
                (!cleaned.is_empty()).then(|| cleaned.into())
            }
            _ => Some(value.into()),
        });
    builder
        .add_tag_attributes("a", ["href", "title"])
        .add_tag_attributes("img", ["src", "alt", "width", "height"])
        .add_tag_attributes("td", ["colspan", "rowspan"])
        .add_tag_attributes("th", ["colspan", "rowspan"])
        .add_tag_attributes("ol", ["start"]);

    let cleaned = builder.clean(html).to_string();
    if cleaned.trim().is_empty() {
        String::new()
    } else {
        cleaned
    }
}

/// Keep only the declarations on [`STYLE_PROPERTIES`] whose values survive
/// [`is_safe_style_value`].
fn clean_style(value: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    for declaration in value.split(';') {
        let Some((property, val)) = declaration.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        let val = val.trim();
        if val.is_empty() || !STYLE_PROPERTIES.contains(&property.as_str()) {
            continue;
        }
        if !is_safe_style_value(val) {
            continue;
        }
        kept.push(format!("{property}: {val}"));
    }
    kept.join("; ")
}

/// Whether a declaration's value is something a mail client from any decade
/// will parse.
pub fn is_safe_style_value(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    if lowered.len() > 200 {
        return false;
    }
    !UNSAFE_VALUE_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

// ---------------------------------------------------------------------------
// the plain-text twin
// ---------------------------------------------------------------------------

/// The `text/plain` alternative, derived from the HTML.
///
/// # Why this is not "strip the tags"
///
/// The plain part is what a screen reader, a watch, a terminal client and every
/// spam filter reads. Tags-removed HTML gives all of them one run-on paragraph
/// with the list items welded together. So this reproduces the *structure*:
/// blocks are separated by a blank line, `<br>` is a newline, list items keep
/// their bullet or number, a blockquote keeps its `>` prefix, and a link whose
/// text is not already its URL carries the URL after it in angle brackets —
/// which is the one piece of information that is otherwise simply lost.
///
/// Emphasis is *not* reproduced. `**bold**` in a plain-text part is a markdown
/// convention, and this editor no longer has one; a reader who sees asterisks
/// cannot tell them from asterisks the writer typed.
pub fn to_text(html: &str) -> String {
    let mut out = Text::default();
    let mut scanner = Scanner::new(html);
    while let Some(token) = scanner.next_token() {
        match token {
            Token::Text(raw) => out.push_text(&decode_entities(&raw)),
            Token::Open { name, attributes } => out.open(&name, &attributes),
            Token::Close(name) => out.close(&name),
        }
    }
    out.finish()
}

/// The accumulating plain-text document, and the block context it is in.
#[derive(Default)]
struct Text {
    out: String,
    /// One entry per open `<ol>`/`<ul>`; `Some(n)` counts an ordered list.
    lists: Vec<Option<usize>>,
    quote_depth: usize,
    /// Whitespace is significant inside `<pre>` and collapsed everywhere else.
    pre_depth: usize,
    /// The href of the `<a>` being read, and the text seen since it opened.
    link: Option<(String, usize)>,
    /// Set when a block has just closed: the newline is written lazily, so a
    /// document does not end in a run of them.
    pending_breaks: usize,
    /// Whether anything at all has been written to the current line.
    line_started: bool,
    /// How deep inside a list item we are.
    ///
    /// Google Docs wraps every list item's text in a `<p>`, and a paragraph
    /// break between the bullet and its own words puts "- " on a line of its
    /// own. Inside an item, a block break is a line break and nothing more.
    item_depth: usize,
    /// Swallow the next break: the line so far is a marker awaiting its text.
    swallow_break: bool,
}

impl Text {
    fn push_text(&mut self, raw: &str) {
        if self.pre_depth > 0 {
            for (index, line) in raw.split('\n').enumerate() {
                if index > 0 {
                    self.newline(1);
                }
                if !line.is_empty() {
                    self.write(line);
                }
            }
            return;
        }
        let collapsed = collapse_whitespace(raw);
        if collapsed.is_empty() {
            return;
        }
        // Whitespace between two blocks is layout, not a word gap.
        if collapsed.trim().is_empty() && (self.pending_breaks > 0 || !self.line_started) {
            return;
        }
        // A space at a block boundary is layout, not content.
        if !self.line_started && collapsed.starts_with(' ') {
            let trimmed = collapsed.trim_start();
            if trimmed.is_empty() {
                return;
            }
            self.write(trimmed);
            return;
        }
        self.write(&collapsed);
    }

    fn write(&mut self, text: &str) {
        self.flush_breaks();
        if !self.line_started {
            let prefix = "> ".repeat(self.quote_depth);
            self.out.push_str(&prefix);
            self.line_started = true;
        }
        self.out.push_str(text);
        self.swallow_break = false;
    }

    /// Ask for `count` line breaks before the next thing written. Requests
    /// coalesce, so `</p><p>` and `</div></div><div>` both come out as one
    /// blank line rather than three.
    fn newline(&mut self, count: usize) {
        if (self.out.is_empty() && !self.line_started) || self.swallow_break {
            return;
        }
        self.pending_breaks = self.pending_breaks.max(count);
    }

    fn flush_breaks(&mut self) {
        for _ in 0..self.pending_breaks {
            self.out.push('\n');
            self.line_started = false;
        }
        // A blank line inside a quote is still inside the quote, and a client
        // that re-quotes this reply keys off the marker being on every line.
        if self.pending_breaks > 1 && self.quote_depth > 0 {
            let marker = "> ".repeat(self.quote_depth);
            let trimmed = marker.trim_end();
            self.out.truncate(self.out.len() - 1);
            self.out.push_str(trimmed);
            self.out.push('\n');
        }
        self.pending_breaks = 0;
    }

    fn open(&mut self, name: &str, attributes: &str) {
        match name {
            "br" => self.newline(1),
            "hr" => {
                self.newline(2);
                self.write("---");
                self.newline(2);
            }
            // `<div>` is not on the spaced list: it has no default margin, so
            // two in a row are two adjacent lines — and the editor writes one
            // `<div>` per line. Treating it as a paragraph would double-space
            // the plain-text part of every message. `<p>` draws its own blank
            // line and gets one here.
            "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "table" | "tfoot" | "thead"
            | "tbody" => self.newline(if self.item_depth > 0 { 1 } else { 2 }),
            "div" | "tr" => self.newline(1),
            "td" | "th" => {
                if self.line_started {
                    self.write("\t");
                }
            }
            "pre" => {
                self.newline(2);
                self.pre_depth += 1;
            }
            "blockquote" => {
                self.newline(2);
                self.quote_depth += 1;
            }
            "ul" => {
                self.newline(2);
                self.lists.push(None);
            }
            "ol" => {
                self.newline(2);
                let start = attribute(attributes, "start")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1);
                self.lists.push(Some(start));
            }
            "li" => {
                self.newline(1);
                let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let marker = format!("{n}. ");
                        *n += 1;
                        marker
                    }
                    _ => "- ".to_string(),
                };
                self.write(&format!("{indent}{marker}"));
                self.item_depth += 1;
                self.swallow_break = true;
            }
            "a" => {
                if let Some(href) = attribute(attributes, "href") {
                    let href = decode_entities(&href);
                    self.link = Some((href, self.out.len()));
                }
            }
            _ => {}
        }
    }

    fn close(&mut self, name: &str) {
        match name {
            "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "table" | "tfoot" | "thead"
            | "tbody" => self.newline(if self.item_depth > 0 { 1 } else { 2 }),
            "li" => {
                self.item_depth = self.item_depth.saturating_sub(1);
                self.newline(1);
            }
            "div" | "tr" => self.newline(1),
            "pre" => {
                self.pre_depth = self.pre_depth.saturating_sub(1);
                self.newline(2);
            }
            "blockquote" => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.newline(2);
            }
            "ul" | "ol" => {
                self.lists.pop();
                self.newline(2);
            }
            "a" => {
                // The URL is worth writing only when the text does not already
                // say it — `<a href="https://x">https://x</a>` is the common
                // case and doubling it reads as a mistake.
                if let Some((href, from)) = self.link.take() {
                    let text: String = self.out.chars().skip(from).collect();
                    let text = text.trim();
                    if !href.is_empty()
                        && !text.is_empty()
                        && text != href
                        && !href.starts_with("mailto:")
                        && !href.starts_with("cid:")
                    {
                        self.write(&format!(" <{href}>"));
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> String {
        self.pending_breaks = 0;
        while self.out.ends_with('\n') || self.out.ends_with(' ') {
            self.out.pop();
        }
        self.out
    }
}

fn collapse_whitespace(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    if out.trim().is_empty() {
        // A run of whitespace between two blocks is layout. Between two inline
        // runs it is a word gap, and the caller cannot tell the difference —
        // but `write` refuses a leading space at the start of a line, which
        // covers the case that matters.
        return if out.is_empty() {
            String::new()
        } else {
            " ".to_string()
        };
    }
    out
}

// ---------------------------------------------------------------------------
// the scanner
// ---------------------------------------------------------------------------

enum Token {
    Text(String),
    Open { name: String, attributes: String },
    Close(String),
}

/// A tag-level scanner, not a parser.
///
/// It is enough because the input has already been through [`sanitize`], which
/// is html5ever underneath: the markup reaching here is well-formed, the tags
/// are from a known list, and comments, `<script>` and `<style>` are gone. A
/// second DOM here would buy nothing and cost a dependency.
struct Scanner<'a> {
    rest: &'a str,
}

impl<'a> Scanner<'a> {
    fn new(html: &'a str) -> Self {
        Scanner { rest: html }
    }

    fn next_token(&mut self) -> Option<Token> {
        if self.rest.is_empty() {
            return None;
        }
        if let Some(after) = self.rest.strip_prefix('<') {
            // A `<` that does not open a tag is text — sanitized output escapes
            // it, but this function is also handed hand-written HTML in tests.
            if let Some(end) = after.find('>') {
                let inner = &after[..end];
                self.rest = &after[end + 1..];
                let inner = inner.trim();
                if let Some(name) = inner.strip_prefix('/') {
                    return Some(Token::Close(tag_name(name)));
                }
                let inner = inner.strip_suffix('/').unwrap_or(inner);
                let name = tag_name(inner);
                let attributes = inner
                    .split_once(|c: char| c.is_ascii_whitespace())
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_default();
                return Some(Token::Open { name, attributes });
            }
        }
        let end = self.rest[1..]
            .find('<')
            .map(|i| i + 1)
            .unwrap_or(self.rest.len());
        let text = &self.rest[..end];
        self.rest = &self.rest[end..];
        Some(Token::Text(text.to_string()))
    }
}

fn tag_name(inner: &str) -> String {
    inner
        .split(|c: char| c.is_ascii_whitespace() || c == '/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// One attribute out of a tag's attribute text. Quoted values only, which is
/// what [`sanitize`] emits.
fn attribute(attributes: &str, name: &str) -> Option<String> {
    let lowered = attributes.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(found) = lowered[from..].find(name) {
        let at = from + found;
        let before_ok = at == 0
            || lowered.as_bytes()[at - 1].is_ascii_whitespace()
            || lowered.as_bytes()[at - 1] == b';';
        let after = &attributes[at + name.len()..];
        let trimmed = after.trim_start();
        if before_ok {
            if let Some(value) = trimmed.strip_prefix('=') {
                let value = value.trim_start();
                let quote = value.chars().next()?;
                if quote == '"' || quote == '\'' {
                    let value = &value[1..];
                    let end = value.find(quote)?;
                    return Some(value[..end].to_string());
                }
                let end = value
                    .find(|c: char| c.is_ascii_whitespace())
                    .unwrap_or(value.len());
                return Some(value[..end].to_string());
            }
        }
        from = at + name.len();
    }
    None
}

/// The five entities [`sanitize`] emits, plus `&nbsp;` — which is what Word and
/// Google Docs paste by the hundred, and which must become an ordinary space
/// rather than a U+00A0 that a plain-text reader shows as a box.
fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let Some(end) = after[..after.len().min(12)].find(';') else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..end];
        let replacement = match entity {
            "amp" => Some("&".to_string()),
            "lt" => Some("<".to_string()),
            "gt" => Some(">".to_string()),
            "quot" => Some("\"".to_string()),
            "apos" | "#39" => Some("'".to_string()),
            "nbsp" | "#160" => Some(" ".to_string()),
            other => other
                .strip_prefix('#')
                .and_then(|digits| digits.parse::<u32>().ok())
                .and_then(char::from_u32)
                .map(|c| c.to_string()),
        };
        match replacement {
            Some(text) => {
                out.push_str(&text);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Plain text as HTML the editor can hold: one block per line, nothing else.
///
/// Used where text arrives from somewhere that never had HTML — a draft written
/// on a phone whose only body is `text/plain`, and the signature preference,
/// which is a text field. The alternative is to put the text in the editor
/// unescaped and watch a `<` in somebody's signature eat the rest of it.
pub fn from_plain_text(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len() + 16);
    for line in normalized.split('\n') {
        out.push_str("<div>");
        if line.trim().is_empty() {
            // An empty `<div>` collapses to nothing in most clients, which
            // would silently drop the blank line the writer left.
            out.push_str("<br>");
        } else {
            escape_into(&mut out, line);
        }
        out.push_str("</div>");
    }
    out
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out
}

fn escape_into(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
}

// ---------------------------------------------------------------------------
// signatures
// ---------------------------------------------------------------------------

/// The RFC 3676 delimiter, as HTML. The line is exactly `-- ` — trailing space
/// and all — because that is what every client looks for when it greys a
/// signature out or trims it from a quote.
pub const SIGNATURE_MARKER: &str = "<div>-- </div>";

/// Whether a body already carries a signature block, so appending one twice is
/// impossible.
pub fn has_signature(html: &str) -> bool {
    html.contains(SIGNATURE_MARKER)
}
