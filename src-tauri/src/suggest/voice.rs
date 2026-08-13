//! His own Sent mail, as the examples the model writes from.
//!
//! This is the whole differentiator, and it is the reason the feature is worth
//! building inside a mail client rather than buying. Sixty-one thousand of his
//! messages are on this disk with FTS5 over them, so "how does he actually
//! answer this person" is a local read measured in milliseconds. Generic
//! LLM-email is recognisable on sight — the throat-clearing, the summarising
//! first paragraph, the offer to help further — and he would not send it. A
//! suggestion that does not sound like him is worse than no suggestion, because
//! it costs him a read to reject.
//!
//! # Two passes, in this order
//!
//! **Same correspondent first.** How he writes to *this person* dominates every
//! other signal: the greeting, whether there is one, the sign-off, the length,
//! whether he uses their first name. Found by taking his own messages out of
//! threads that person has written in — which is a stronger relation than "he
//! once emailed that address", because it is the back-and-forth rather than a
//! broadcast.
//!
//! **Then topically similar.** For a first message from somebody new there is no
//! correspondent history, and the fallback is his own Sent mail about the same
//! subject. FTS5 over the incoming subject and snippet, restricted to messages
//! he sent.
//!
//! Both are capped and both are trimmed hard: see [`for_voice`]. A prompt full
//! of quoted history teaches the model to quote history.

use rusqlite::{params, Connection};

use crate::db::{queries, Result as DbResult};

/// One past message of his, ready to go in the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceExample {
    pub subject: String,
    pub body: String,
    /// Whether this came from the same-correspondent pass. Ordering only — the
    /// prompt does not tell the model which is which, because "this one matters
    /// more" is a thing to express by putting it first.
    pub same_correspondent: bool,
}

/// How many of each pass to fetch. Six is about two thousand tokens of his
/// prose after trimming, which is enough to carry a voice and short enough that
/// the model still answers the message rather than the examples.
pub const CORRESPONDENT_LIMIT: usize = 4;
pub const TOPICAL_LIMIT: usize = 3;

/// The longest a single example is allowed to be. A three-page message of his
/// is still his voice in its first paragraphs, and the rest is detail about
/// something else.
pub const MAX_EXAMPLE_CHARS: usize = 900;

/// The shortest that is worth including. "Sounds good" is his voice in the sense
/// that he wrote it and useless in the sense that it teaches nothing.
pub const MIN_EXAMPLE_CHARS: usize = 40;

/// Everything he has written that is worth showing the model, best first.
pub fn examples(
    conn: &Connection,
    account_id: i64,
    own_address: &str,
    correspondent: &str,
    topic: &str,
) -> DbResult<Vec<VoiceExample>> {
    let mut out = from_correspondent(conn, account_id, own_address, correspondent)?;
    let mut topical = on_topic(conn, account_id, own_address, topic)?;

    // A message that turned up in both passes is one message. Deduped on the
    // trimmed body rather than on a row id, because the same sentence sent
    // twice teaches the model the same thing twice.
    topical.retain(|t| !out.iter().any(|e| e.body == t.body));
    out.append(&mut topical);
    Ok(out)
}

/// His own messages in threads this person has written in, newest first.
pub fn from_correspondent(
    conn: &Connection,
    account_id: i64,
    own_address: &str,
    correspondent: &str,
) -> DbResult<Vec<VoiceExample>> {
    if own_address.trim().is_empty() || correspondent.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT mine.subject, COALESCE(mine.body_text, '')
           FROM messages mine
          WHERE mine.account_id = ?1
            AND lower(mine.from_email) = lower(?2)
            AND mine.is_draft = 0
            AND mine.thread_id IN (
                SELECT theirs.thread_id
                  FROM messages theirs
                 WHERE theirs.account_id = ?1
                   AND lower(theirs.from_email) = lower(?3)
            )
          ORDER BY mine.internal_date DESC
          LIMIT ?4",
    )?;

    // Over-fetch: trimming drops quoted-only and one-line replies, and a limit
    // applied before the trim would hand back four blanks.
    let rows = stmt.query_map(
        params![
            account_id,
            own_address,
            correspondent,
            (CORRESPONDENT_LIMIT * 4) as i64
        ],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;

    let mut out = Vec::new();
    for row in rows {
        let (subject, body) = row?;
        if let Some(body) = for_voice(&body) {
            out.push(VoiceExample {
                subject,
                body,
                same_correspondent: true,
            });
            if out.len() == CORRESPONDENT_LIMIT {
                break;
            }
        }
    }
    Ok(out)
}

/// His own messages about the same thing, by full-text rank.
pub fn on_topic(
    conn: &Connection,
    account_id: i64,
    own_address: &str,
    topic: &str,
) -> DbResult<Vec<VoiceExample>> {
    let Some(expr) = topic_expression(topic) else {
        return Ok(Vec::new());
    };
    if own_address.trim().is_empty() {
        return Ok(Vec::new());
    }

    // The FTS hit set is collapsed first and joined back to `messages`, the same
    // shape `search_threads` uses: bm25() is an auxiliary function and wants to
    // be evaluated once per matching row rather than once per join output.
    let mut stmt = conn.prepare(
        "WITH hits AS (
             SELECT rowid AS rid, bm25(messages_fts, 4.0, 1.0) AS score
               FROM messages_fts
              WHERE messages_fts MATCH ?1
              ORDER BY score
              LIMIT 400
         )
         SELECT m.subject, COALESCE(m.body_text, '')
           FROM hits
           JOIN messages m ON m.id = hits.rid
          WHERE m.account_id = ?2
            AND lower(m.from_email) = lower(?3)
            AND m.is_draft = 0
          ORDER BY hits.score
          LIMIT ?4",
    )?;

    let rows = stmt.query_map(
        params![expr, account_id, own_address, (TOPICAL_LIMIT * 6) as i64],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;

    let mut out = Vec::new();
    for row in rows {
        let (subject, body) = row?;
        if let Some(body) = for_voice(&body) {
            out.push(VoiceExample {
                subject,
                body,
                same_correspondent: false,
            });
            if out.len() == TOPICAL_LIMIT {
                break;
            }
        }
    }
    Ok(out)
}

/// An OR of the topic's distinctive words, or `None` when there are none.
///
/// `fts_match_expression` joins terms with a space, which FTS5 reads as AND —
/// right for a search box, wrong here, where a subject line's exact word set
/// will not appear together in anything he has ever written. So this ORs, drops
/// the stopwords that would match everything, and takes the first handful.
pub fn topic_expression(topic: &str) -> Option<String> {
    const MAX_TERMS: usize = 8;
    const STOPWORDS: [&str; 30] = [
        "re", "fwd", "fw", "the", "a", "an", "and", "or", "of", "to", "for", "in", "on", "at",
        "is", "it", "this", "that", "you", "your", "we", "i", "me", "my", "with", "from", "was",
        "be", "are", "hi",
    ];

    let mut terms: Vec<String> = Vec::new();
    for word in topic.split(|c: char| !c.is_alphanumeric()) {
        if terms.len() == MAX_TERMS {
            break;
        }
        let lower = word.to_ascii_lowercase();
        if lower.len() < 3 || STOPWORDS.contains(&lower.as_str()) {
            continue;
        }
        let Some(escaped) = queries::fts_escape(&lower, false) else {
            continue;
        };
        if !terms.contains(&escaped) {
            terms.push(escaped);
        }
    }
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

/// The part of a past message that is his voice, or `None` if there is none.
///
/// Drops three things, in order, and each one is a thing the model would copy:
///
///  * **The quoted reply.** A body is usually his four sentences on top of six
///    of somebody else's; leaving them in triples the prompt and teaches the
///    model to quote.
///  * **The signature.** Held to the RFC 3676 `-- ` delimiter, which every
///    client writes and Mach writes itself. The composer adds his signature
///    back on the way out, so an example carrying one would double it.
///  * **Forwarded and attribution lines.** "On Tuesday, X wrote:" is boilerplate
///    the model is very willing to imitate.
pub fn for_voice(body: &str) -> Option<String> {
    let normalised = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut kept: Vec<&str> = Vec::new();
    for line in normalised.lines() {
        let trimmed = line.trim_start();

        // The signature delimiter ends the message. `-- ` exactly, or `--` on
        // its own line, which is what clients that strip the trailing space
        // leave behind.
        if trimmed == "-- " || trimmed.trim_end() == "--" {
            break;
        }
        if trimmed.starts_with('>') {
            continue;
        }
        if is_attribution(trimmed) {
            break;
        }
        kept.push(line);
    }

    // Own the string only once the borrows are done.
    let joined = kept.join("\n");
    let text = collapse_blank_lines(joined.trim());
    if text.chars().count() < MIN_EXAMPLE_CHARS {
        return None;
    }
    Some(truncate_chars(&text, MAX_EXAMPLE_CHARS))
}

/// "On Tue, 3 Jun 2025 at 14:02, Kate <kate@…> wrote:" and its cousins, plus
/// the block separators Outlook and Gmail draw above a quoted message.
fn is_attribution(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    if lower.starts_with("on ") && lower.ends_with("wrote:") {
        return true;
    }
    if lower.starts_with("-----original message") || lower.starts_with("________________") {
        return true;
    }
    if lower.starts_with("begin forwarded message") {
        return true;
    }
    // "From: kate@example.org" as the first line of a quoted header block.
    lower.starts_with("from: ") && lower.contains('@')
}

fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    out.trim().to_string()
}

/// Truncate on a character boundary, at a word if one is near the end.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    match cut.rfind(char::is_whitespace) {
        Some(at) if at > max * 3 / 4 => format!("{}…", cut[..at].trim_end()),
        _ => format!("{}…", cut.trim_end()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quoted_reply_is_dropped() {
        let body = "Tuesday works for me — let's say two o'clock at the usual place.\n\
                    I'll bring the numbers you asked about last week.\n\
                    \n\
                    > Are you free Tuesday?\n\
                    > Kate";
        let kept = for_voice(body).unwrap();
        assert!(kept.contains("Tuesday works for me"));
        assert!(!kept.contains("Are you free"));
    }

    #[test]
    fn the_signature_is_dropped() {
        let body = "That all sounds right to me. I'll pick it up on Thursday and let you know.\n\
                    \n-- \nBruno\nMach\nhttps://machmail.dev";
        let kept = for_voice(body).unwrap();
        assert!(!kept.contains("machmail.dev"));
        assert!(kept.starts_with("That all sounds right"));
    }

    #[test]
    fn the_attribution_line_ends_the_message() {
        let body = "No need — I already did it, and the numbers came out fine in the end.\n\
                    \nOn Tue, 3 Jun 2025 at 14:02, Kate <kate@example.org> wrote:\n\
                    Some quoted thing that is quite long indeed.";
        let kept = for_voice(body).unwrap();
        assert!(!kept.contains("Kate"));
        assert!(!kept.contains("quoted thing"));
    }

    #[test]
    fn outlook_and_forward_separators_end_it_too() {
        for separator in [
            "-----Original Message-----",
            "________________________________",
            "Begin forwarded message:",
            "From: kate@example.org",
        ] {
            let body = format!(
                "Sure, I can take a look at that this afternoon and get back to you.\n\n{separator}\nquoted"
            );
            let kept = for_voice(&body).unwrap();
            assert!(!kept.contains("quoted"), "{separator} should end the message");
        }
    }

    #[test]
    fn a_message_with_nothing_left_is_no_example() {
        assert_eq!(for_voice("> only a quote\n> and more of it"), None);
        assert_eq!(for_voice("Sounds good."), None);
        assert_eq!(for_voice(""), None);
    }

    #[test]
    fn long_messages_are_cut_at_a_word() {
        let body = "word ".repeat(600);
        let kept = for_voice(&body).unwrap();
        assert!(kept.chars().count() <= MAX_EXAMPLE_CHARS + 1);
        assert!(kept.ends_with('…'));
        assert!(!kept.contains("wor…"));
    }

    #[test]
    fn runs_of_blank_lines_collapse() {
        let body = "First paragraph of a reply that is comfortably long enough.\n\n\n\n\nSecond one.";
        let kept = for_voice(body).unwrap();
        assert!(!kept.contains("\n\n\n"));
    }

    #[test]
    fn the_topic_expression_ors_its_distinctive_words() {
        let expr = topic_expression("Re: the quarterly invoice for Acme").unwrap();
        assert!(expr.contains(" OR "), "{expr}");
        assert!(expr.contains("\"quarterly\""), "{expr}");
        assert!(expr.contains("\"invoice\""), "{expr}");
        assert!(expr.contains("\"acme\""), "{expr}");
        // Stopwords and the reply prefix carry no signal and match everything.
        assert!(!expr.contains("\"re\""), "{expr}");
        assert!(!expr.contains("\"the\""), "{expr}");
        assert!(!expr.contains("\"for\""), "{expr}");
    }

    #[test]
    fn a_topic_with_no_distinctive_words_has_no_expression() {
        assert_eq!(topic_expression("Re: the it"), None);
        assert_eq!(topic_expression("   "), None);
        assert_eq!(topic_expression("!!!"), None);
    }

    #[test]
    fn the_topic_expression_escapes_quotes() {
        let expr = topic_expression("the \"budget\" meeting").unwrap();
        assert!(!expr.contains("\"budget\"\""), "unescaped: {expr}");
        assert!(expr.contains("meeting"));
    }
}
