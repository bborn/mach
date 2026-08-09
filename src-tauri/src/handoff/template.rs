//! The `run` template: a command line he wrote, turned into argv, then filled in.
//!
//! # Why the order of those two steps is the whole security model
//!
//! `claude "{{prompt}}"` looks like shell, and it has to: it is a command line,
//! he types it into a text field, and quoting is how a human says "this is one
//! argument". The temptation is to substitute first and hand the result to a
//! shell, which is exactly the bug — the prompt contains an email body, an email
//! body is written by a stranger, and `"; rm -rf ~; echo "` in the middle of one
//! is a working command line.
//!
//! So the parse happens *first*, on the template alone, which is text he wrote.
//! [`tokenize`] applies the quoting rules once, to trusted input, and produces a
//! fixed list of argv elements. [`substitute`] then replaces `{{placeholders}}`
//! *inside* those elements. After that point there is no parser left: the values
//! are already on the far side of every rule that could have given a character a
//! meaning, and [`super::plan`] execs them as argv. A semicolon is a semicolon.
//!
//! This is also why the tokenizer is deliberately *not* a shell. It knows
//! whitespace, `'…'`, `"…"` and backslash — the four things a person actually
//! uses when writing a command line — and nothing else. `$HOME`, `` `date` ``,
//! `$(…)`, `|`, `&&`, `*` are ordinary characters here. A template that needs a
//! pipeline should name a script that has one; expanding the vocabulary would be
//! adding a shell back in through the front door.
//!
//! The one convenience kept is `~/`, expanded at the start of a token only, and
//! only in the template — never in a substituted value.

use std::collections::BTreeMap;

use super::HandoffError;

/// Every `{{name}}` the substitution set understands.
///
/// Listed once, here, because three places need to agree about it: the
/// substitution itself, the environment [`super::plan`] exports, and the hint
/// the editor dialog shows under the `run` field.
pub const PLACEHOLDERS: &[&str] = &[
    "prompt",
    "note",
    "subject",
    "from",
    "date",
    "body",
    "permalink",
    "attachments",
    "context_file",
];

/// The values `{{name}}` resolves to, keyed by name.
pub type Values = BTreeMap<String, String>;

/// Split a command line into argv.
///
/// Errors on an unterminated quote and on an empty template, because both mean
/// the same thing to a user — "that is not a command" — and both are much better
/// caught while he is typing than at three in the morning when a handoff does
/// nothing.
pub fn tokenize(template: &str) -> Result<Vec<String>, HandoffError> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = template.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        // Single quotes are literal, as everywhere else: there
                        // is no escape inside them, not even for a backslash.
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err(unterminated('\'')),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        // Only `\"` and `\\` mean anything; `\n` is a literal
                        // backslash and an n, which is what a shell does too.
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Err(unterminated('"')),
                        },
                        Some(c) => current.push(c),
                        None => return Err(unterminated('"')),
                    }
                }
            }
            '\\' => {
                started = true;
                match chars.next() {
                    Some(escaped) => current.push(escaped),
                    None => current.push('\\'),
                }
            }
            c => {
                started = true;
                current.push(c);
            }
        }
    }

    if started {
        tokens.push(current);
    }

    if tokens.is_empty() {
        return Err(HandoffError::BadTemplate(
            "there is no command to run — the run field is empty".to_string(),
        ));
    }

    Ok(tokens.into_iter().map(expand_home).collect())
}

fn unterminated(quote: char) -> HandoffError {
    HandoffError::BadTemplate(format!(
        "the run template has an unterminated {quote} — close the quote and try again"
    ))
}

/// `~/x` → `$HOME/x`, at the start of a token only.
///
/// A bare `~` in the middle of a word is not a home directory in any shell
/// either, and expanding one would be a surprise rather than a convenience.
fn expand_home(token: String) -> String {
    let Some(rest) = token.strip_prefix("~/") else {
        return token;
    };
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => format!("{}/{rest}", home.trim_end_matches('/')),
        _ => token,
    }
}

/// Fill `{{name}}` inside already-split argv elements.
///
/// Three properties are load-bearing, and all three are consequences of doing
/// this *after* [`tokenize`]:
///
/// * a value cannot create an argument — whitespace inside it is just bytes;
/// * a value cannot end an argument — a quote inside it is just a quote;
/// * a value cannot be re-scanned — substitution is one pass, so a body
///   containing the literal text `{{prompt}}` is copied through, never resolved.
///
/// An unknown placeholder is left exactly as written. A typo should be visible
/// in the argument list rather than silently becoming an empty string.
pub fn substitute(tokens: &[String], values: &Values) -> Vec<String> {
    tokens.iter().map(|token| fill(token, values)).collect()
}

fn fill(token: &str, values: &Values) -> String {
    let mut out = String::with_capacity(token.len());
    let mut rest = token;

    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            // No closing braces anywhere: the rest is literal text.
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        match values.get(name.trim()) {
            // NULs cannot exist in argv — the exec would fail with an opaque
            // error — so they are dropped at the boundary rather than allowed
            // to turn a working handoff into "invalid argument". Nothing legible
            // is lost: a NUL in a mail body is either a bug or an attempt.
            Some(value) => out.extend(value.chars().filter(|c| *c != '\0')),
            None => {
                out.push_str("{{");
                out.push_str(name);
                out.push_str("}}");
            }
        }
        rest = &after[close + 2..];
    }

    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> Values {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// The single most important test in this module: a body written to break
    /// out of a shell command stays one argument.
    #[test]
    fn a_hostile_body_cannot_become_a_second_argument() {
        let tokens = tokenize(r#"claude "{{prompt}}""#).expect("tokenize");
        let filled = substitute(
            &tokens,
            &values(&[("prompt", "\"; rm -rf ~; echo \"$(id)`whoami`\nmore")]),
        );
        assert_eq!(filled.len(), 2, "argv must stay two elements: {filled:?}");
        assert_eq!(filled[0], "claude");
        assert_eq!(filled[1], "\"; rm -rf ~; echo \"$(id)`whoami`\nmore");
    }

    #[test]
    fn substitution_is_one_pass() {
        let tokens = tokenize("run {{note}}").expect("tokenize");
        let filled = substitute(&tokens, &values(&[("note", "{{prompt}}")]));
        assert_eq!(filled[1], "{{prompt}}", "a value must never be rescanned");
    }

    #[test]
    fn an_unknown_placeholder_stays_visible() {
        let tokens = tokenize("run {{nope}}").expect("tokenize");
        assert_eq!(substitute(&tokens, &values(&[]))[1], "{{nope}}");
    }
}
