//! Quoted-history detection.
//!
//! The job here is to find where the reply ends and the thread's history
//! begins, so the UI can collapse the history behind a "show quoted text"
//! control. Nothing is ever deleted: [`split_html`] and [`split_text`] return
//! both halves and the caller decides what to show.
//!
//! # Why this runs *before* the sanitizer
//!
//! Every real-world quote signal lives in something the sanitizer removes —
//! `class="gmail_quote"`, `id="divRplyFwdMsg"`, an Outlook border-top style.
//! Detection therefore runs on the raw body and the two halves are sanitized
//! independently afterwards. That ordering is also why quote detection is not
//! a security boundary: a mis-split is a display bug, because both halves go
//! through [`super::sanitize::sanitize_fragment`] regardless, and each half is
//! re-parsed and re-balanced from scratch. A body crafted to split in a
//! surprising place buys the sender nothing.

/// A message body cut into what is new and what is history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Split {
    /// Content above the quote. May be empty for a bare forward.
    pub new: String,
    /// Content from the quote marker onward, or `None` when no quote was found.
    pub quoted: Option<String>,
}

impl Split {
    fn all_new(body: &str) -> Self {
        Split {
            new: body.to_string(),
            quoted: None,
        }
    }
}

// ---------------------------------------------------------------------------
// HTML
// ---------------------------------------------------------------------------

/// Literal markers used by the major clients. Matched case-insensitively
/// against the raw body; the cut is made at the start of the enclosing tag so
/// the container element goes with the quote rather than being orphaned above
/// it.
const HTML_MARKERS: &[&str] = &[
    // Gmail: `gmail_quote`, `gmail_quote_container`, `gmail_attr`.
    "gmail_quote",
    // Yahoo.
    "yahoo_quoted",
    // Thunderbird.
    "moz-cite-prefix",
    // Outlook desktop and Outlook web.
    "divrplyfwdmsg",
    "mail-editor-reference-message-container",
    "border-top:solid #e1e1e1",
    "border-top: solid #e1e1e1",
    // Outlook's horizontal rule between reply and history.
    "id=\"stopspelling\"",
    // Apple Mail / generic separators.
    "-----original message-----",
    "----- original message -----",
    "---original message---",
    "begin forwarded message:",
    "forwarded message",
    "original message",
];

pub fn split_html(html: &str) -> Split {
    if html.is_empty() {
        return Split::all_new(html);
    }
    // `to_ascii_lowercase` is byte-length preserving, so indices into `lower`
    // are valid indices into `html`.
    let lower = html.to_ascii_lowercase();

    let mut cut: Option<usize> = None;
    let mut consider = |idx: Option<usize>| {
        if let Some(i) = idx {
            cut = Some(cut.map_or(i, |c: usize| c.min(i)));
        }
    };

    for marker in HTML_MARKERS {
        if let Some(i) = lower.find(marker) {
            // The separator words are generic enough to appear in prose, so
            // require the surrounding dashes that every client actually emits.
            if (*marker == "forwarded message" || *marker == "original message")
                && !dash_delimited(&lower, i, marker.len())
            {
                continue;
            }
            consider(tag_start_before(html, i));
        }
    }

    consider(find_blockquote_cite(&lower));
    consider(find_underscore_rule(&lower, html));
    consider(find_attribution(&lower, html));

    match cut {
        Some(i) if i < html.len() => Split {
            new: html[..i].to_string(),
            quoted: Some(html[i..].to_string()),
        },
        _ => Split::all_new(html),
    }
}

/// Byte index of the `<` that opens the tag containing `idx`, or the start of
/// the text run if `idx` is not inside markup.
fn tag_start_before(html: &str, idx: usize) -> Option<usize> {
    Some(html[..idx].rfind('<').unwrap_or(0))
}

/// True when the marker is wrapped in the dashes clients put around it
/// (`-----Original Message-----`, `---------- Forwarded message ---------`).
fn dash_delimited(lower: &str, at: usize, len: usize) -> bool {
    let before = lower[..at].trim_end().ends_with('-');
    let after = lower[at + len..].trim_start().starts_with('-');
    before && after
}

/// `<blockquote type="cite">` in any attribute order or quoting style. A plain
/// `<blockquote>` is deliberately *not* a quote signal: marketing mail uses it
/// for pull quotes, and collapsing those would hide the message.
fn find_blockquote_cite(lower: &str) -> Option<usize> {
    let mut at = 0usize;
    while let Some(rel) = lower[at..].find("<blockquote") {
        let start = at + rel;
        at = start + 1;
        let end = lower[start..]
            .find('>')
            .map(|e| start + e)
            .unwrap_or(lower.len());
        let tag = &lower[start..end];
        if tag.contains("type=\"cite\"") || tag.contains("type='cite'") || tag.contains("type=cite")
        {
            return Some(start);
        }
    }
    None
}

/// Outlook's `________________________________` divider.
fn find_underscore_rule(lower: &str, html: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    let mut run = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'_' {
            run += 1;
            if run >= 20 {
                return tag_start_before(html, i + 1 - run);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// The largest char boundary at or below `index`.
///
/// `str::floor_char_boundary` is still unstable, and every arithmetic offset in
/// this module is a byte count that can land inside a multi-byte character.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The "On <date>, <person> wrote:" attribution line.
///
/// `wrote:` on its own is far too common in prose ("our founder wrote:"), so a
/// match requires a standalone `on` within a short window before it, outside of
/// any tag, with a digit or an `@` between the two — i.e. an actual date or an
/// address, which is what every client puts there.
fn find_attribution(lower: &str, html: &str) -> Option<usize> {
    const WINDOW: usize = 320;
    let mut at = 0usize;
    while let Some(rel) = lower[at..].find("wrote:") {
        let wrote = at + rel;
        at = wrote + 1;
        // Backing up a fixed number of *bytes* lands wherever it lands, and
        // slicing there panics if that is the middle of a character. Real mail
        // hits this constantly: any confidentiality footer with a curly
        // apostrophe 320 bytes before a "wrote:" was enough to take the whole
        // process down, and it did — eight times, on one thread of payroll
        // PDFs. Nothing about the window needs to be exactly 320 bytes, so it
        // moves to the boundary at or below it.
        let window_start = floor_char_boundary(lower, wrote.saturating_sub(WINDOW));
        let window = &lower[window_start..wrote];
        if !window.contains(|c: char| c.is_ascii_digit()) && !window.contains('@') {
            continue;
        }
        let Some(on) = last_standalone_on(window) else {
            continue;
        };
        let abs = window_start + on;
        if inside_tag(lower, abs) {
            continue;
        }
        // Text between the attribution start and "wrote:" should be a date and
        // a name, not a paragraph.
        if visible_len(&lower[abs..wrote]) > 200 {
            continue;
        }
        return tag_start_before(html, abs);
    }
    None
}

fn last_standalone_on(window: &str) -> Option<usize> {
    let mut found = None;
    let mut at = 0usize;
    while let Some(rel) = window[at..].find("on") {
        let i = at + rel;
        at = i + 1;
        let before_ok = i == 0
            || !window[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
        let after_ok = window[i + 2..]
            .chars()
            .next()
            .is_some_and(|c| c == ' ' || c == '\u{a0}' || c == '\n' || c == '\t' || c == '<');
        if before_ok && after_ok {
            found = Some(i);
        }
    }
    found
}

fn inside_tag(lower: &str, idx: usize) -> bool {
    match (lower[..idx].rfind('<'), lower[..idx].rfind('>')) {
        (Some(open), Some(close)) => open > close,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Length of `s` ignoring markup, so an attribution wrapped in a pile of
/// Outlook `<span>`s is not mistaken for a paragraph.
fn visible_len(s: &str) -> usize {
    let mut n = 0usize;
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => n += 1,
            _ => {}
        }
    }
    n
}

// ---------------------------------------------------------------------------
// Plain text
// ---------------------------------------------------------------------------

pub fn split_text(text: &str) -> Split {
    if text.is_empty() {
        return Split::all_new(text);
    }
    let lines: Vec<&str> = text.split_inclusive('\n').collect();

    let mut cut: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        // A signature block (`-- `) is not history; stop looking at markers
        // that could be confused with one but keep scanning for real quotes.
        let is_quote_start = trimmed.starts_with('>')
            || lower.starts_with("-----original message-----")
            || lower.starts_with("--- original message ---")
            || lower.starts_with("begin forwarded message:")
            || (lower.contains("forwarded message") && trimmed.starts_with('-'))
            || (trimmed.len() >= 20 && trimmed.chars().all(|c| c == '_'))
            || is_text_attribution(&lower);

        if is_quote_start {
            cut = Some(back_up_over_attribution(&lines, i));
            break;
        }
    }

    let Some(cut) = cut else {
        return Split::all_new(text);
    };
    let boundary: usize = lines[..cut].iter().map(|l| l.len()).sum();
    Split {
        new: text[..boundary].to_string(),
        quoted: Some(text[boundary..].to_string()),
    }
}

/// "On Tue, Aug 5, 2026 at 3:04 PM Bob <bob@x.test> wrote:" — possibly with the
/// trailing colon on its own after a wrap.
fn is_text_attribution(lower: &str) -> bool {
    if !lower.starts_with("on ") || !lower.ends_with("wrote:") {
        return false;
    }
    lower.contains(|c: char| c.is_ascii_digit()) || lower.contains('@')
}

/// The attribution line introduces the quote, so it belongs with it. Walk back
/// over an attribution and any blank line between it and the quoted text.
fn back_up_over_attribution(lines: &[&str], start: usize) -> usize {
    let mut i = start;
    // Attribution lines can wrap; allow the "wrote:" line plus one wrapped line.
    for _ in 0..3 {
        if i == 0 {
            break;
        }
        let prev = lines[i - 1].trim().to_ascii_lowercase();
        if prev.is_empty() && i >= 2 {
            let before = lines[i - 2].trim().to_ascii_lowercase();
            if before.ends_with("wrote:") || before.starts_with("on ") {
                i -= 1;
                continue;
            }
            break;
        }
        if prev.ends_with("wrote:") || (prev.starts_with("on ") && prev.ends_with(':')) {
            i -= 1;
            continue;
        }
        break;
    }
    i
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    /// The exact shape that killed the process: a multi-byte character sitting
    /// where the fixed-size lookback lands.
    #[test]
    fn an_attribution_search_survives_a_multibyte_char_at_the_window_edge() {
        // A curly apostrophe placed so that `wrote - 320` falls inside it.
        let mut html = String::from("<p>");
        html.push_str(&"a".repeat(317));
        html.push('\u{2019}'); // three bytes, straddling the 320-byte mark
        html.push_str(" on 2026-01-01 someone@example.com wrote: hello</p>");

        let lower = html.to_lowercase();
        // The assertion is that this returns rather than panicking.
        let _ = find_attribution(&lower, &html);
    }

    #[test]
    fn a_footer_full_of_typography_does_not_panic() {
        let mut html = String::new();
        for _ in 0..40 {
            html.push_str("Confidential — do not forward. It’s privileged. ");
        }
        html.push_str("On 1 Jan 2026, a@b.com wrote: quoted");
        let lower = html.to_lowercase();
        let _ = find_attribution(&lower, &html);
    }

    #[test]
    fn floor_char_boundary_never_lands_inside_a_character() {
        let s = "aé→𝄞";
        for i in 0..=s.len() {
            let floored = floor_char_boundary(s, i);
            assert!(s.is_char_boundary(floored), "{i} floored to {floored}");
            assert!(floored <= i);
        }
    }
}
