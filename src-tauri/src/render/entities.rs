//! Turning `&#39;` back into `'`.
//!
//! Gmail hands back `snippet` with HTML entities still encoded, so the list row
//! for a message reading "Sure I'm free Thursday" arrives as
//! `Sure I&#39;m free Thursday` — and that is exactly what the inbox showed.
//! Snippets are plain text by the time they reach a row, so there is nothing
//! downstream that would ever have decoded them.
//!
//! # Why this is one pass and not a chain of replacements
//!
//! The obvious implementation is a run of `.replace()` calls, and there was one
//! in `agent::tools`. It has a bug that only shows up on text about HTML:
//!
//! ```text
//!   "&amp;lt;"  --replace &amp;-->  "&lt;"  --replace &lt;-->  "<"
//! ```
//!
//! The correct answer is the literal text `&lt;`, because the author escaped an
//! ampersand. Decoding `&amp;` last fixes that particular pair and still breaks
//! `&amp;amp;`. There is no ordering that works, because the flaw is re-reading
//! output as input.
//!
//! Scanning once cannot make that mistake: every character is consumed exactly
//! once, and a decoded `&` is output, never re-examined.
//!
//! Only the entities that actually turn up in mail are named here. Anything
//! unrecognised is left exactly as written, which is the right answer for a
//! bare `&` in prose ("Ben & Jerry's") and for the long tail of entities nobody
//! sends.

/// The named entities worth carrying, decoded to their character.
const NAMED: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", '\u{a0}'),
    ("hellip", '…'),
    ("mdash", '—'),
    ("ndash", '–'),
    ("lsquo", '‘'),
    ("rsquo", '’'),
    ("ldquo", '“'),
    ("rdquo", '”'),
    ("middot", '·'),
    ("bull", '•'),
    ("trade", '™'),
    ("copy", '©'),
    ("reg", '®'),
    ("deg", '°'),
    ("euro", '€'),
    ("pound", '£'),
    ("times", '×'),
];

/// The longest thing that can sit between `&` and `;` before we stop looking.
///
/// Bounded so a stray ampersand in a long line costs a glance, not a scan to
/// the end of the message.
const MAX_ENTITY_LEN: usize = 10;

/// Decodes HTML entities in text that is already plain — snippets, extracted
/// bodies, subjects. Not a sanitiser: it must never run on markup, because
/// turning `&lt;script&gt;` into `<script>` is precisely the thing escaping was
/// protecting against.
pub fn decode(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }

    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];

        match after
            .char_indices()
            .take(MAX_ENTITY_LEN + 1)
            .find(|(_, c)| *c == ';')
            .and_then(|(end, _)| resolve(&after[..end]).map(|c| (c, end)))
        {
            Some((decoded, end)) => {
                out.push(decoded);
                // Past the ';'. The decoded character is never re-read, which
                // is what makes `&amp;lt;` come out as the text `&lt;`.
                rest = &after[end + 1..];
            }
            None => {
                // Not an entity — a bare ampersand. Keep it and move on.
                out.push('&');
                rest = after;
            }
        }
    }

    out.push_str(rest);
    out
}

/// The body of an entity — `amp`, `#39`, `#x27` — to its character.
fn resolve(body: &str) -> Option<char> {
    if body.is_empty() {
        return None;
    }

    if let Some(digits) = body.strip_prefix('#') {
        let code = match digits.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => digits.parse::<u32>().ok()?,
        };
        // Rejects surrogates and out-of-range code points, so a malformed
        // numeric entity is left as written rather than becoming U+FFFD.
        return char::from_u32(code);
    }

    NAMED
        .iter()
        .find(|(name, _)| *name == body)
        .map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_apostrophe_that_started_this() {
        assert_eq!(decode("Sure I&#39;m free Thursday"), "Sure I'm free Thursday");
    }

    #[test]
    fn decodes_named_numeric_and_hex_forms() {
        assert_eq!(decode("a &amp; b"), "a & b");
        assert_eq!(decode("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode("&#8230;"), "…");
        assert_eq!(decode("&#x27;"), "'");
        assert_eq!(decode("&#X27;"), "'");
    }

    #[test]
    fn does_not_re_read_its_own_output() {
        // The bug in the chained-replace version: `&amp;lt;` is an escaped
        // ampersand followed by "lt;", so the answer is the literal `&lt;`.
        assert_eq!(decode("&amp;lt;"), "&lt;");
        assert_eq!(decode("&amp;amp;"), "&amp;");
        assert_eq!(decode("&amp;#39;"), "&#39;");
    }

    #[test]
    fn leaves_bare_ampersands_alone() {
        assert_eq!(decode("Ben & Jerry's"), "Ben & Jerry's");
        assert_eq!(decode("R&D"), "R&D");
        assert_eq!(decode("a & b & c"), "a & b & c");
        assert_eq!(decode("trailing &"), "trailing &");
    }

    #[test]
    fn leaves_unknown_and_malformed_entities_as_written() {
        assert_eq!(decode("&notanentity;"), "&notanentity;");
        assert_eq!(decode("&;"), "&;");
        assert_eq!(decode("&#;"), "&#;");
        assert_eq!(decode("&#99999999;"), "&#99999999;");
        // A lone surrogate is not a character; leaving it beats a replacement
        // glyph appearing in someone's inbox.
        assert_eq!(decode("&#xD800;"), "&#xD800;");
    }

    #[test]
    fn does_not_scan_forever_for_a_missing_semicolon() {
        let long = format!("&{}", "x".repeat(500));
        assert_eq!(decode(&long), long);
    }

    #[test]
    fn text_without_ampersands_is_returned_unchanged() {
        assert_eq!(decode("nothing to do here"), "nothing to do here");
        assert_eq!(decode(""), "");
    }

    #[test]
    fn handles_multibyte_text_around_entities() {
        // `find('&')` returns a byte offset; slicing on it must stay on a char
        // boundary or this panics.
        assert_eq!(decode("café &amp; crème"), "café & crème");
        assert_eq!(decode("→&#39;→"), "→'→");
    }
}
