//! Sanitized HTML → plain text.
//!
//! Two callers, and they want the same thing for different reasons: the agent
//! reads a message body it is going to put in a prompt, and [`crate::evict`]
//! derives the `body_text` an HTML-only message never had, so the message can
//! be searched by its body and read while its HTML is being re-fetched.
//!
//! # This is not a parser and does not need to be
//!
//! [`from_sanitized`] takes HTML that has already been through
//! [`super::sanitize::sanitize_fragment`], which is what makes a scanner this
//! small correct rather than merely lucky:
//!
//!  * ammonia re-serializes a real parse tree, so every tag is well formed and
//!    every attribute value is entity-escaped. A `>` inside an attribute is
//!    `&gt;`, so "the tag ends at the next `>`" holds.
//!  * `<script>`, `<style>`, `<title>` and the rest of the sanitizer's
//!    content-dropped tags are gone *with their contents*, so a stylesheet
//!    cannot arrive here and become "text". A stripper run on raw mail would
//!    happily index three kilobytes of CSS selectors.
//!  * Comments are gone, so `<!-- -->` is not a case.
//!
//! [`from_html`] is the same thing with the sanitize step included, for a caller
//! holding a raw body. Sanitizing to throw the result away is not free — it is
//! the expensive half — but it is the only version that is safe to point at
//! whatever a stranger sent.
//!
//! # What it keeps
//!
//! Block elements become line breaks and table cells become spaces, because
//! most HTML mail is a table and running the cells together turns "Total $42"
//! into "Total$42" — one token, findable by neither word. `alt` text is kept:
//! in mail that is entirely images it is the only prose there is. A link whose
//! content came to nothing contributes its `href`, so a button that is one
//! image with no `alt` is still a word rather than a hole.
//!
//! Entities are decoded once, at the end, over the whole buffer — including the
//! attribute values, which are pushed still escaped for exactly that reason.
//! Decoding twice would take `&amp;lt;` all the way to `<`.

use std::borrow::Cow;

use super::entities;
use super::sanitize;

/// Elements that end a line.
const BREAKS: &[&str] = &[
    "address",
    "blockquote",
    "br",
    "caption",
    "center",
    "dd",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "li",
    "main",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tr",
    "ul",
];

/// Elements that separate what is either side of them without ending the line.
/// Table cells, which in mail are the layout grid rather than a data table.
const SPACES: &[&str] = &["td", "th"];

/// Sanitize a raw body and take the text out of it.
pub fn from_html(raw: &str) -> String {
    let (clean, _) = sanitize::sanitize_fragment(raw, false);
    from_sanitized(&clean)
}

/// Take the text out of already-sanitized HTML.
///
/// Whitespace *in the markup* is not whitespace on screen: a body that was
/// pretty-printed has a newline and four spaces between every tag, and HTML
/// collapses all of it to one space. So text content is collapsed as it is
/// pushed, and the only real line breaks are the ones the elements themselves
/// put there.
pub fn from_sanitized(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 4);
    // Open anchors: the href, and how long `out` was when the anchor opened.
    // If nothing was added in between, the link would otherwise be nothing.
    let mut anchors: Vec<(String, usize)> = Vec::new();

    let mut rest = html;
    loop {
        let Some(open) = rest.find('<') else {
            push_text(&mut out, rest);
            break;
        };
        push_text(&mut out, &rest[..open]);
        let after = &rest[open + 1..];
        // No closing `>` means the input was truncated mid-tag. Everything
        // after it is markup we cannot read, so it is not text either.
        let Some(close) = after.find('>') else {
            break;
        };
        emit_tag(&after[..close], &mut out, &mut anchors);
        rest = &after[close + 1..];
    }

    tidy(&entities::decode(&out))
}

/// Text content, with every run of whitespace collapsed to one space.
fn push_text(out: &mut String, raw: &str) {
    let mut space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(ch);
    }
    if space {
        out.push(' ');
    }
}

/// End the line, unless it has already ended.
///
/// Block elements break on the way in *and* on the way out, and mail nests them
/// twelve deep, so without this a table is one blank line per row per level.
fn push_break(out: &mut String) {
    if out.trim_end_matches([' ', '\t']).ends_with('\n') || out.trim().is_empty() {
        return;
    }
    out.push('\n');
}

/// What one tag contributes.
fn emit_tag(tag: &str, out: &mut String, anchors: &mut Vec<(String, usize)>) {
    let closing = tag.starts_with('/');
    let name: String = tag
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();

    match name.as_str() {
        "a" if !closing => {
            anchors.push((attribute(tag, "href").unwrap_or_default(), out.len()));
        }
        "a" if closing => {
            if let Some((href, from)) = anchors.pop() {
                if out[from..].trim().is_empty() && !href.is_empty() {
                    out.push(' ');
                    out.push_str(&href);
                    out.push(' ');
                }
            }
        }
        // Void, so only the opening form exists.
        "img" if !closing => {
            if let Some(alt) = attribute(tag, "alt") {
                if !alt.trim().is_empty() {
                    out.push(' ');
                    push_text(out, &alt);
                    out.push(' ');
                }
            }
        }
        other if BREAKS.contains(&other) => push_break(out),
        other if SPACES.contains(&other) => out.push(' '),
        _ => {}
    }
}

/// One attribute out of a tag's interior, still entity-escaped.
///
/// ammonia writes every attribute as `name="value"` with the value escaped, so
/// the value is everything up to the next unescaped `"` — there are no others.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let mut from = 0;
    while let Some(at) = tag[from..].find(&needle) {
        let start = from + at;
        // `href="` must not match `data-href="`. The character before it is
        // whitespace when this is a real attribute name.
        let boundary = start == 0
            || tag[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        let value = start + needle.len();
        if boundary {
            let end = tag[value..].find('"')?;
            return Some(tag[value..value + end].to_string());
        }
        from = value;
    }
    None
}

/// Trim each line and drop the ones that hold nothing.
///
/// Most of the collapsing has already happened at push time; what is left is the
/// whitespace decoding brought in — `&nbsp;` is a space nobody typed, and a
/// spacer row is a hundred of them.
fn tidy(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut collapsed = String::with_capacity(line.len());
        let mut space = false;
        for ch in line.chars() {
            if ch.is_whitespace() {
                space = true;
                continue;
            }
            if space && !collapsed.is_empty() {
                collapsed.push(' ');
            }
            space = false;
            collapsed.push(ch);
        }
        if !collapsed.is_empty() {
            lines.push(collapsed);
        }
    }
    lines.join("\n")
}

/// Cut to at most `max` bytes without splitting a character.
pub fn truncate(text: &str, max: usize) -> Cow<'_, str> {
    sanitize::truncate_on_char_boundary(text, max)
}
