//! What the model is asked, and what comes back.
//!
//! Pure: strings in, strings out, no clock and no database, so the prompt is
//! something a test can read and the parser is something a test can break.
//!
//! # One call, every stance
//!
//! The model is asked for two or three *stances* in a single response, each with
//! a label and its full reply already written. Not one call per stance: the
//! stances only mean anything relative to each other — "say yes", "ask for a
//! raincheck", "push it a week" is a set of alternatives, and a model that
//! cannot see the other two writes three near-identical answers.
//!
//! Writing the bodies up front is what makes picking one instant. The
//! alternative — a label now, the body when he picks — puts a model call on the
//! keystroke, and a suggestion that takes two seconds to become a draft is an
//! interruption.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::voice::VoiceExample;

/// One stance: what it is, and what it says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stance {
    /// Three to five words, imperative, in his register. This is a *button*.
    pub label: String,
    /// The whole reply, plain text, ready to go into the composer.
    pub body: String,
}

/// At most this many stances reach the row. Two buttons is a glance; five is a
/// menu, and a menu is a decision about deciding.
///
/// It was three until the row was measured in the real window. The row also
/// carries reply, reply-all and forward, and at 1440 wide three stances plus
/// that strip does not fit on one line — every label truncated to `Say you…`,
/// which names no stance at all, and a second line would make this a panel.
/// Two is also the shape that was asked for: *"show me two buttons like [Say
/// you can't make it] [Say you'll be there!]"*.
pub const MAX_STANCES: usize = 2;

/// The longest a label may be before it stops being a label.
///
/// Twenty is "three or four words" measured rather than asserted — `Push it to
/// next week` is exactly 20. It is a *rejection*, not a truncation, and that is
/// the point: a label that would not fit is better absent than shown as `Offer
/// to walk them thr…`, which names no stance at all.
///
/// It was 28 for one screenshot. The row also carries reply, reply-all and
/// forward — those were asked for by name and a conversation the model had an
/// opinion about is not one you never want to forward — and at a 1440-wide
/// window three 28-character labels plus that strip does not fit: every stance
/// came out as `Say you…`, which is the failure this constant exists to
/// prevent. Shorter labels are the better half of that trade anyway; the row is
/// scanned, not read. Verified against the real window at the width he uses.
pub const MAX_LABEL_CHARS: usize = 20;

/// A body longer than this is not a reply, it is an essay with his name on it.
pub const MAX_BODY_CHARS: usize = 2_000;

/// Who the model is and what it is being asked for.
///
/// The register instructions are the expensive part and they are not decoration:
/// generic assistant-email has a shape — an opening line that restates the
/// question, a closing line offering further help, an exclamation mark — and a
/// model will produce it by default because most of its email training data is
/// that. Naming the specific tics is more effective than asking for "a natural
/// tone", which every model believes it already has.
pub fn system_prompt(owner_name: &str, owner_email: &str) -> String {
    let who = if owner_name.trim().is_empty() {
        owner_email.to_string()
    } else {
        format!("{} <{}>", owner_name.trim(), owner_email)
    };

    format!(
        "You draft replies for {who}, in his own words, for him to review.

You are given a message he has received and a set of replies he has written
before. The past replies are the point: match how he actually writes — his
length, his greeting or absence of one, his sign-off or absence of one, his
punctuation, how formal he is with this person.

Produce two or three distinct STANCES. A stance is a different decision about
what to do, not a different phrasing of the same decision: accepting and
declining are two stances, `Sure, Tuesday works` and `Tuesday works for me` are
one. When the reply is obvious and low-stakes, one stance is the honest answer.

Each stance has a label and a body.

The LABEL names the stance in three or four words, imperative, lower stakes than
a sentence — it is the text on a button, and it must fit on one:

  Say you'll be there
  Ask for a raincheck
  Push it to next week
  Ask their timeline

Never a sentence, never longer than those, never punctuation at the end, never a
summary of the body. A label that does not fit is dropped, and its reply with it.

The BODY is the whole reply, plain text, ready to send. No subject line, no
greeting unless his past replies have one, no signature — one is added
automatically. Do not invent facts, dates, prices, names or commitments that are
not in the message or in his own past replies; if the reply needs a detail he
has not given you, write around it or ask for it.

Never write: an opening line restating what they said, `I hope this finds you
well`, `Thanks for reaching out`, `Please let me know if you have any
questions`, `I'd be happy to`, or an exclamation mark he would not have used.

Answer with JSON only — an array of objects with `label` and `body`, and nothing
else. No prose before it, no code fence around it."
    )
}

/// The message, the conversation it sits in, and his own past replies.
///
/// His replies come first and are labelled as voice examples rather than as
/// context, because a model handed a wall of email tends to answer the last
/// thing in it.
pub fn user_prompt(
    correspondent: &str,
    subject: &str,
    conversation: &[(String, String)],
    incoming: &str,
    examples: &[VoiceExample],
) -> String {
    let mut out = String::new();

    if !examples.is_empty() {
        out.push_str(
            "Replies he has written before. Match this voice; do not reuse their content.\n\n",
        );
        for (i, example) in examples.iter().enumerate() {
            out.push_str(&format!(
                "--- example {} (subject: {}) ---\n{}\n\n",
                i + 1,
                blank_to_placeholder(&example.subject),
                example.body.trim()
            ));
        }
    }

    out.push_str(&format!(
        "The conversation, oldest first. Subject: {}\n\n",
        blank_to_placeholder(subject)
    ));
    for (sender, body) in conversation {
        out.push_str(&format!("--- {} ---\n{}\n\n", sender, body.trim()));
    }

    out.push_str(&format!(
        "Reply to this message from {}:\n\n{}\n\nAnswer with the JSON array of stances.",
        blank_to_placeholder(correspondent),
        incoming.trim()
    ));
    out
}

fn blank_to_placeholder(value: &str) -> &str {
    match value.trim() {
        "" => "(none)",
        other => other,
    }
}

/// The stances in a model response, or an empty list.
///
/// Tolerant on the way in and strict on the way out. A model asked for JSON will
/// occasionally wrap it in a fence or introduce it with a sentence, and refusing
/// the whole answer over that would throw away three usable replies. So the
/// first `[` through the matching `]` is what gets parsed — and then every
/// stance is checked, because a label that is a paragraph is not something to
/// put on a button and a body that is empty is not a reply.
///
/// Never an error. There is no error state for a suggestion: it either exists or
/// it does not, and nothing is waiting for it.
pub fn parse_stances(text: &str) -> Vec<Stance> {
    let Some(slice) = json_array(text) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(slice) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };

    let mut out: Vec<Stance> = Vec::new();
    for item in items {
        let label = item.get("label").and_then(Value::as_str).unwrap_or_default();
        let body = item.get("body").and_then(Value::as_str).unwrap_or_default();
        let Some(stance) = clean(label, body) else {
            continue;
        };
        // Two buttons that say the same thing is one button and a mistake.
        if out.iter().any(|s| {
            s.label.eq_ignore_ascii_case(&stance.label) || s.body.trim() == stance.body.trim()
        }) {
            continue;
        }
        out.push(stance);
        if out.len() == MAX_STANCES {
            break;
        }
    }
    out
}

/// One stance, tidied, or `None` if there is nothing usable in it.
fn clean(label: &str, body: &str) -> Option<Stance> {
    let label = label.trim().trim_end_matches(['.', '!', ':', ';']).trim();
    let body = normalise_newlines(body.trim());
    if label.is_empty() || body.is_empty() {
        return None;
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return None;
    }
    Some(Stance {
        label: label.to_string(),
        body: truncate_chars(&body, MAX_BODY_CHARS),
    })
}

/// The outermost bracketed array in a blob of text.
///
/// Depth-counted and string-aware rather than "first `[` to last `]`", so a `]`
/// inside one of the bodies does not cut the array in half.
fn json_array(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('[')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (i, &byte) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return text.get(start..=i);
                }
            }
            _ => {}
        }
    }
    None
}

fn normalise_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_array_parses() {
        let text = r#"[{"label":"Say you'll be there","body":"Tuesday works. See you at two."},
                       {"label":"Ask for a raincheck","body":"Can we push this to next week?"}]"#;
        let stances = parse_stances(text);
        assert_eq!(stances.len(), 2);
        assert_eq!(stances[0].label, "Say you'll be there");
        assert_eq!(stances[1].body, "Can we push this to next week?");
    }

    #[test]
    fn a_fenced_array_with_a_preamble_parses() {
        let text = "Here are the stances:\n\n```json\n[{\"label\":\"Say yes\",\"body\":\"Yes.\"}]\n```";
        let stances = parse_stances(text);
        assert_eq!(stances.len(), 1);
        assert_eq!(stances[0].label, "Say yes");
    }

    #[test]
    fn a_bracket_inside_a_body_does_not_end_the_array() {
        let text = r#"[{"label":"Point at the doc","body":"It's in the notes [see section 2] — have a look."}]"#;
        let stances = parse_stances(text);
        assert_eq!(stances.len(), 1);
        assert!(stances[0].body.contains("[see section 2]"));
    }

    #[test]
    fn nothing_parseable_is_no_stances_rather_than_an_error() {
        assert!(parse_stances("I can't help with that.").is_empty());
        assert!(parse_stances("[not json").is_empty());
        assert!(parse_stances("{\"label\":\"x\"}").is_empty());
        assert!(parse_stances("").is_empty());
    }

    #[test]
    fn stances_missing_a_label_or_a_body_are_dropped() {
        let text = r#"[{"label":"","body":"Yes."},
                       {"body":"No label here."},
                       {"label":"Only a label"},
                       {"label":"Say yes","body":"  "},
                       {"label":"Say no","body":"No."}]"#;
        let stances = parse_stances(text);
        assert_eq!(stances.len(), 1);
        assert_eq!(stances[0].label, "Say no");
    }

    #[test]
    fn a_label_that_is_a_sentence_is_not_a_label() {
        let long = "I would suggest replying that you are able to attend the meeting on Tuesday";
        let text = format!(r#"[{{"label":"{long}","body":"Yes."}}]"#);
        assert!(parse_stances(&text).is_empty());
    }

    #[test]
    fn the_labels_the_prompt_asks_for_all_fit_on_a_button() {
        // The four examples the system prompt gives the model, held to the
        // limit the row can actually draw. If one of these grew past it, the
        // prompt would be asking for something the UI truncates.
        for label in [
            "Say you'll be there",
            "Ask for a raincheck",
            "Push it to next week",
            "Ask their timeline",
        ] {
            assert!(system_prompt("B", "b@e.com").contains(label), "{label}");
            assert!(
                label.chars().count() <= MAX_LABEL_CHARS,
                "{label} is {} characters, over the {MAX_LABEL_CHARS} a button holds",
                label.chars().count()
            );
        }
    }

    #[test]
    fn a_label_that_would_not_fit_the_row_is_dropped_rather_than_cut() {
        // Measured against the real window: three of these plus "Write it
        // myself" share one line, and a cut label names no stance at all.
        let text = r#"[{"label":"Offer to walk them through it","body":"Sure."},
                       {"label":"Offer a call","body":"Sure."}]"#;
        let stances = parse_stances(text);
        assert_eq!(stances.len(), 1);
        assert_eq!(stances[0].label, "Offer a call");
    }

    #[test]
    fn trailing_punctuation_comes_off_the_label() {
        let text = r#"[{"label":"Say you'll be there.","body":"Yes."}]"#;
        assert_eq!(parse_stances(text)[0].label, "Say you'll be there");
    }

    #[test]
    fn duplicate_stances_collapse() {
        let text = r#"[{"label":"Say yes","body":"Yes, that works."},
                       {"label":"SAY YES","body":"Different words."},
                       {"label":"Agree","body":"Yes, that works."},
                       {"label":"Say no","body":"No."}]"#;
        let stances = parse_stances(text);
        assert_eq!(stances.len(), 2);
        assert_eq!(stances[1].label, "Say no");
    }

    #[test]
    fn no_more_than_max_stances_reach_the_row() {
        let text = r#"[{"label":"One","body":"a body"},{"label":"Two","body":"b body"},
                       {"label":"Three","body":"c body"},{"label":"Four","body":"d body"}]"#;
        assert_eq!(parse_stances(text).len(), MAX_STANCES);
    }

    #[test]
    fn a_runaway_body_is_cut() {
        let body = "x".repeat(MAX_BODY_CHARS + 500);
        let text = format!(r#"[{{"label":"Say yes","body":"{body}"}}]"#);
        assert_eq!(parse_stances(&text)[0].body.chars().count(), MAX_BODY_CHARS);
    }

    #[test]
    fn the_system_prompt_names_him_and_asks_for_json_only() {
        let prompt = system_prompt("Bruno Bornsztein", "bruno@example.com");
        assert!(prompt.contains("Bruno Bornsztein <bruno@example.com>"));
        assert!(prompt.contains("JSON only"));
        assert!(prompt.contains("STANCES"));
        // The forbidden-phrases list is the part that stops it sounding like
        // every other assistant, so it is pinned here rather than assumed.
        assert!(prompt.contains("Thanks for reaching out"));
    }

    #[test]
    fn the_system_prompt_falls_back_to_the_address_with_no_name() {
        let prompt = system_prompt("  ", "bruno@example.com");
        assert!(prompt.contains("for bruno@example.com"));
        assert!(!prompt.contains("<bruno@example.com>"));
    }

    #[test]
    fn his_own_replies_come_before_the_conversation() {
        let examples = vec![VoiceExample {
            subject: "Lunch".into(),
            body: "Tuesday's fine.".into(),
            same_correspondent: true,
        }];
        let prompt = user_prompt(
            "Kate <kate@example.org>",
            "Coffee?",
            &[("Kate".into(), "Are you free Tuesday?".into())],
            "Are you free Tuesday?",
            &examples,
        );
        let voice_at = prompt.find("Tuesday's fine.").unwrap();
        let conversation_at = prompt.find("The conversation").unwrap();
        assert!(voice_at < conversation_at);
        assert!(prompt.contains("Reply to this message from Kate <kate@example.org>"));
    }

    #[test]
    fn the_prompt_survives_a_message_with_no_subject_and_no_examples() {
        let prompt = user_prompt("kate@example.org", "", &[], "hello?", &[]);
        assert!(prompt.contains("Subject: (none)"));
        assert!(!prompt.contains("Replies he has written before"));
    }
}
