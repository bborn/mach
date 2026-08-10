//! RFC 3676 `format=flowed`, decoded back into paragraphs.
//!
//! A `text/plain` body arrives with the sender's line breaks in it, and the
//! client has no way to tell which of them the sender meant and which the
//! sender's generator inserted to keep the lines under eighty columns. That is
//! the whole problem `format=flowed` exists to solve: a sender who declares it
//! marks every break they did *not* mean by leaving a space in front of it.
//!
//! So a line ending in a space is a **soft** break, to be joined with the line
//! after it, and a line ending in anything else is a **hard** break, to be kept.
//! Decoding that gives back the paragraphs the sender typed, which then wrap to
//! whatever column the reader's window happens to be — which is the point.
//!
//! # This runs only where the sender said so
//!
//! Nothing here is a heuristic and nothing here may be applied to a body that
//! did not declare `format=flowed` in its `Content-Type`. Ordinary plain text
//! uses its line breaks to mean things — a numbered list, an indented block, a
//! table of figures, a copy-paste block — and joining those lines because they
//! happen to end in a space would turn a message that merely *looks* untidy
//! into one that has been scrambled. `ipc::render` decides; this module only
//! knows how.
//!
//! # What is handled, and where it is written down
//!
//! * §4.1 space-stuffing — a leading space the sender added to protect a line
//!   that would otherwise start with a space, a `>`, or `From `, is removed.
//! * §4.2 quoting — the quote prefix is a run of `>` with nothing between them.
//!   Depth is part of a paragraph's identity: a change of depth ends it, so a
//!   reply is never joined onto the quote it answers.
//! * §4.3 the signature separator — `-- ` ends in a space and is *not* a soft
//!   break. Joining it would swallow the separator into the last paragraph and
//!   take the signature with it.
//! * §4.5 `delsp=yes` — the space before a soft break was inserted by the
//!   sender's generator and has to come back out when the lines are rejoined.
//!   Without this a `delsp=yes` message gains a stray space at every join.

/// Rejoin the soft breaks in a `format=flowed` body.
///
/// `delsp` is the `delsp=yes` parameter from the same `Content-Type`. Pass
/// `false` when it was absent, which is the overwhelmingly common case.
pub fn unflow(raw: &str, delsp: bool) -> String {
    // Line count, not byte count: the output is never longer than the input.
    let mut out: Vec<String> = Vec::new();
    // The paragraph currently open, as (quote depth, text so far).
    let mut open: Option<(usize, String)> = None;

    for raw_line in raw.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let (depth, after_quotes) = split_quote_prefix(line);
        // §4.1. Exactly one space, and only the one the sender stuffed.
        let text = after_quotes.strip_prefix(' ').unwrap_or(after_quotes);

        // §4.3. `-- ` ends in a space and is still a hard break.
        let signature = text == SIGNATURE_SEPARATOR;
        let soft = !signature && text.ends_with(' ');
        // §4.5. The space belongs to the encoding, not to the sentence.
        let chunk = if soft && delsp { &text[..text.len() - 1] } else { text };

        // §4.2. A paragraph belongs to one quote depth. A line at a different
        // depth starts a new one however the previous line ended, which is what
        // keeps a reply from being glued onto the quote above it.
        if let Some((open_depth, _)) = &open {
            if *open_depth != depth {
                let (d, text) = open.take().expect("just matched");
                out.push(rebuild(d, &text));
            }
        }

        match open.as_mut() {
            Some((_, paragraph)) => paragraph.push_str(chunk),
            None => open = Some((depth, chunk.to_string())),
        }

        if !soft {
            let (d, text) = open.take().expect("just inserted");
            out.push(rebuild(d, &text));
        }
    }

    // A body whose last line was soft-broken with nothing after it. Malformed,
    // and there is exactly one sensible thing to do with it.
    if let Some((d, text)) = open.take() {
        out.push(rebuild(d, &text));
    }

    out.join("\n")
}

/// RFC 3676 §4.3. The space is part of it.
const SIGNATURE_SEPARATOR: &str = "-- ";

/// The quote prefix is a run of `>` and nothing else — no spaces between them,
/// per §4.2. Returns the depth and what follows it.
fn split_quote_prefix(line: &str) -> (usize, &str) {
    let depth = line.bytes().take_while(|b| *b == b'>').count();
    (depth, &line[depth..])
}

/// One decoded paragraph, with its quote prefix put back in the form the rest
/// of the pipeline reads: `render::quotes::split_text` looks for a leading `>`,
/// and a reader expects to see one.
fn rebuild(depth: usize, text: &str) -> String {
    if depth == 0 {
        return text.to_string();
    }
    let mut line = ">".repeat(depth);
    if !text.is_empty() {
        line.push(' ');
        line.push_str(text);
    }
    line
}

/// `format=flowed` and `delsp=yes`, off a `Content-Type` value.
///
/// Tolerant in the two ways real headers are sloppy: parameter names and values
/// are matched case-insensitively, and a quoted value (`format="flowed"`) is
/// accepted, because both appear in the wild and neither changes the meaning.
/// Anything it cannot read comes back as "not flowed", which is the direction
/// that leaves the body alone.
pub fn flowed_params(content_type: &str) -> (bool, bool) {
    let mut flowed = false;
    let mut delsp = false;
    for param in content_type.split(';').skip(1) {
        let Some((name, value)) = param.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "format" => flowed = value.eq_ignore_ascii_case("flowed"),
            "delsp" => delsp = value.eq_ignore_ascii_case("yes"),
            _ => {}
        }
    }
    (flowed, delsp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_soft_breaks_into_one_paragraph() {
        let body = "The invoice is drafted and waiting for you \nto send. Nothing else needs you.";
        assert_eq!(
            unflow(body, false),
            "The invoice is drafted and waiting for you to send. Nothing else needs you."
        );
    }

    #[test]
    fn keeps_hard_breaks() {
        let body = "One line.\nAnother line.\n\nA new paragraph.";
        assert_eq!(unflow(body, false), body);
    }

    #[test]
    fn delsp_removes_the_space_it_inserted() {
        let body = "one \ntwo";
        assert_eq!(unflow(body, true), "onetwo");
        assert_eq!(unflow(body, false), "one two");
    }

    #[test]
    fn the_signature_separator_is_a_hard_break() {
        let body = "Thanks.\n-- \nAlex Rivera\nMach";
        assert_eq!(unflow(body, false), body);
    }

    #[test]
    fn space_stuffing_is_removed() {
        // A line the sender protected because it would otherwise begin with a
        // `>`, and one that would otherwise begin with `From `.
        let body = " > not a quote\n From accounting";
        assert_eq!(unflow(body, false), "> not a quote\nFrom accounting");
    }

    #[test]
    fn quote_depth_ends_a_paragraph() {
        // The reply is soft-broken; the quote under it must not be joined on.
        let body = "I agree with all of \n> the first point \n> and the second.";
        assert_eq!(
            unflow(body, false),
            "I agree with all of \n> the first point and the second."
        );
    }

    #[test]
    fn nested_quotes_keep_their_depth() {
        let body = ">> deepest \n>> still deepest.\n> shallower.";
        assert_eq!(unflow(body, false), ">> deepest still deepest.\n> shallower.");
    }

    #[test]
    fn a_quoted_blank_line_keeps_its_prefix_without_a_stray_space() {
        assert_eq!(unflow(">\n> after", false), ">\n> after");
    }

    #[test]
    fn a_trailing_soft_break_with_nothing_after_it_is_kept() {
        assert_eq!(unflow("dangling ", false), "dangling ");
    }

    #[test]
    fn an_empty_body_stays_empty() {
        assert_eq!(unflow("", false), "");
    }

    #[test]
    fn a_trailing_newline_survives() {
        assert_eq!(unflow("one\n", false), "one\n");
    }

    #[test]
    fn params_read_format_and_delsp() {
        assert_eq!(
            flowed_params("text/plain; charset=utf-8; format=flowed"),
            (true, false)
        );
        assert_eq!(
            flowed_params("text/plain; format=Flowed; DelSp=Yes"),
            (true, true)
        );
        assert_eq!(
            flowed_params("text/plain; format=\"flowed\""),
            (true, false)
        );
    }

    #[test]
    fn params_default_to_not_flowed() {
        assert_eq!(flowed_params("text/plain"), (false, false));
        assert_eq!(flowed_params("text/plain; charset=utf-8"), (false, false));
        assert_eq!(flowed_params("text/plain; format=fixed"), (false, false));
        // A parameter with no value is not a declaration of anything.
        assert_eq!(flowed_params("text/plain; format"), (false, false));
    }

    /// The shape the reader reported — a hard-wrapped digest, which is *not*
    /// flowed.
    ///
    /// No line in it ends in a space, so nothing is joined and the numbered
    /// list, the hanging indent and the copy-paste block all survive intact.
    /// What does not survive is one space of every indent: §4.1 says a leading
    /// space was stuffed by the sender and comes back out, and this body never
    /// stuffed anything.
    ///
    /// That is the whole argument for never running this on a body that did not
    /// declare `format=flowed`. The damage from guessing is small here and is
    /// not always: a message that *did* end its lines in spaces would have had
    /// its structure joined away.
    #[test]
    fn a_hard_wrapped_digest_keeps_every_break_but_loses_a_space_of_indent() {
        let body = "  1. Invoice #51 is drafted, 64.0 hours\n    at $125/hr = $8,000.00.\n\n       Invoice 51\n       Total due: $8,000.00";
        let out = unflow(body, false);
        assert_eq!(out.lines().count(), body.lines().count());
        assert_eq!(
            out,
            " 1. Invoice #51 is drafted, 64.0 hours\n   at $125/hr = $8,000.00.\n\n      Invoice 51\n      Total due: $8,000.00"
        );
    }
}
