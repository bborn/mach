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

// ---------------------------------------------------------------------------
// text for finding, not for reading
// ---------------------------------------------------------------------------

/// The largest derived body Mach will store.
///
/// Derived text is written into the store beside the markup it came from, so it
/// needs a ceiling that does not depend on the sender's markup being sane. A
/// quarter of a megabyte is far more prose than any mail body — across the
/// 14 349 resident bodies on the owner's store the largest derivation is 123 KB
/// and the average is 6.9 KB — and it bounds the case where stripping the tags
/// off a very large document yields a very large document.
pub const MAX_DERIVED_BYTES: usize = 256 * 1024;

/// The text this HTML would be indexed and read by, or `None` if there is none.
///
/// Sanitizing happens inside [`from_html`], which is what keeps a `<style>`
/// block from arriving in the index as three kilobytes of CSS selectors.
pub fn derive_text(html: &str) -> Option<String> {
    let text = from_html(html);
    let bounded = truncate(&text, MAX_DERIVED_BYTES);
    if bounded.trim().is_empty() {
        None
    } else {
        Some(bounded.into_owned())
    }
}

/// The most distinct words [`word_set`] will hold before it stops collecting.
/// A bound on the set and nothing else; the largest count seen on the owner's
/// store is an order of magnitude below it.
const MAX_COUNTED_WORDS: usize = 4096;

/// The shortest and longest run of letters that can be a word.
///
/// Three at the bottom, because a two-letter token is `to`, `is` or `no`, and
/// those say nothing about whether a body has content in it. Fifteen at the
/// top, because the long tokens in mail are the ones that are not words: a
/// base64 segment of a tracking URL, a hex digest, a message id.
const WORD_LEN: std::ops::RangeInclusive<usize> = 3..=15;

/// Whether a run of letters has the shape of a word rather than of an opaque
/// identifier.
///
/// What this test is for is what it rejects. Splitting on non-letters already
/// breaks `?trkid=NjIxMDk&method=…` apart, but the fragments that survive —
/// `dHJraWQ`, `bGlua`, `tZXRob` — are pure letters of a plausible length, and
/// counting them would make a page of tracking URLs look like it says
/// something.
///
/// Case is what separates them. Base64 alternates case inside a token because
/// it is encoding bytes rather than spelling anything, and English does not,
/// outside `McDonald` and `iPhone`. Those two are missed, which is a cost worth
/// paying. So an ASCII token counts only in one of the three shapes a word is
/// written in: `lowercase`, `Capitalised`, `ACRONYM`.
///
/// A token holding any non-ASCII letter counts whatever its shape or length.
/// Case carries no signal in Japanese, Hebrew or Arabic, and neither does the
/// length bound, because those scripts do not put spaces between words and a
/// whole clause arrives here as one run. Being generous is the safe direction:
/// over-counting can only make Mach conclude that a derivation adds nothing new
/// and skip storing it, and skipping is the cheap mistake.
fn is_word_shaped(word: &str) -> bool {
    if !word.is_ascii() {
        return true;
    }
    if !WORD_LEN.contains(&word.len()) {
        return false;
    }
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let mut rest_lower = true;
    let mut rest_upper = true;
    for ch in chars {
        rest_lower &= ch.is_ascii_lowercase();
        rest_upper &= ch.is_ascii_uppercase();
    }
    match first {
        c if c.is_ascii_lowercase() => rest_lower,
        c if c.is_ascii_uppercase() => rest_lower || rest_upper,
        _ => false,
    }
}

/// The distinct dictionary-shaped words in a text, lowercased.
///
/// Distinct, because the question every caller asks is what a person could find
/// this text by, and a footer repeating six words forty times is findable by
/// six. It is also what makes the answer indifferent to the shape of the
/// padding: 500 tracking URLs built from the same handful of parameters
/// contribute those parameters once.
pub fn word_set(text: &str) -> std::collections::HashSet<String> {
    let mut seen = std::collections::HashSet::new();
    let mut word = String::new();
    let take = |word: &str, seen: &mut std::collections::HashSet<String>| {
        if seen.len() < MAX_COUNTED_WORDS && is_word_shaped(word) {
            seen.insert(word.to_lowercase());
        }
    };
    for ch in text.chars() {
        if ch.is_alphabetic() {
            word.push(ch);
            continue;
        }
        take(&word, &mut seen);
        word.clear();
    }
    // The run the input ended on, which no separator closed.
    take(&word, &mut seen);
    seen
}

/// How many distinct words this text carries. See [`word_set`].
pub fn distinct_words(text: &str) -> usize {
    word_set(text).len()
}

/// The text a message should be *findable* by that it is not already findable
/// by, or `None` when the markup says nothing new.
///
/// # What this is for
///
/// `messages_fts` indexes `subject`, `body_text` and `search_text`. The first
/// two are what the sender sent. The third is this: the readable text of the
/// sender's HTML, stored so a search can match on it, and never rendered.
///
/// It exists because a `text/plain` part is not reliably an alternative
/// rendering of the message. The case that produced the column is message
/// 69568 in the owner's store, a hotel booking confirmation whose generator
/// replaces every anchor's *text* with the tracking URL behind it. Its plain
/// part runs to 19 KB and carries 278 distinct English words — the terms and
/// conditions, in full — so by any measure of density it is a real body rather
/// than a stub. What is missing from it is the booking: the hotel, the address,
/// the dates. `Mandalay Bay and W Las Vegas` arrives as
/// `…&token=dHJraWQ9…TWFuZGFsYXkgQmF5…`, and searching `mandalay` found
/// nothing.
///
/// No test of the plain part's quality catches that, because there is nothing
/// wrong with the plain part. Indexing both texts is the only fix, and it is
/// what this column is.
///
/// # Why it is not simply always stored
///
/// Because most of the time it would be a second copy of what is already
/// indexed. A sender who ships a faithful `text/plain` alternative would have
/// every word of it stored and tokenised twice, for a search nobody could not
/// already do.
///
/// So the test is the one that cannot cost a search: **does this derivation
/// carry a word the message is not already findable by?** Wrong in the
/// direction of storing too much, this costs disk. Wrong in the other direction
/// it cannot be — if the answer is no, every word of the derivation is already
/// in the index, and dropping it removes nothing anyone could have searched
/// for.
///
/// Measured across the 14 349 messages on the owner's store with resident HTML
/// over 2 KB: 12 689 (88 %) carry at least one word their plain part does not,
/// and the median adds 12. The 1 660 that add nothing are left NULL.
pub fn searchable_text(sender_text: Option<&str>, html: &str) -> Option<String> {
    let derived = derive_text(html)?;
    let already = word_set(sender_text.unwrap_or(""));
    let adds_something = word_set(&derived).iter().any(|w| !already.contains(w));
    adds_something.then_some(derived)
}
