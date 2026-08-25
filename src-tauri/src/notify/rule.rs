//! Which mail earns a banner, and what the banner says.
//!
//! Pure functions over facts the store already holds. Nothing here opens a
//! database or touches the platform, so the decision that matters most in this
//! feature is the one thing that is trivial to test.
//!
//! # The rule
//!
//! A message earns a banner when **all** of these are true:
//!
//!  1. it arrived through an incremental history replay on an account that was
//!     already synced — never during a backfill (that gate lives in
//!     [`crate::sync::mail`], because only the sync loop knows which pass it is
//!     in);
//!  2. it is `UNREAD` and carries `INBOX`;
//!  3. it is not `SENT`, `DRAFT`, `SPAM` or `TRASH`, and it is not from the
//!     account's own address;
//!  4. and it is either in Gmail's **Personal** category — which includes
//!     having no category at all — or it lands in a thread this account has
//!     already written to.
//!
//! Everything else is silent.
//!
//! # Why those four and not others
//!
//! The first three are not really judgement; they are the difference between
//! "mail arrived" and "the store changed". Gmail reports a message you sent
//! yourself as an addition, and a filter that files something straight into an
//! archive reports it too. Neither is news.
//!
//! The fourth one is the judgement, and it is the whole feature. A mailbox with
//! 61,000 messages in it is mostly Promotions, Social, Updates and Forums —
//! Gmail has already sorted them, at Google's expense, using signals this app
//! will never have. Refusing to notify for those four categories is not a
//! heuristic Mach invented; it is deferring to a classification the user
//! already trusts enough to let it hide things behind tabs.
//!
//! The escape hatch matters as much as the rule. Receipts, shipping notices and
//! calendar invitations land in Updates, and one of them can be a reply to
//! something you asked for. So a message in a de-prioritised category still
//! notifies when the thread contains something *you* sent: you started that
//! conversation, and its continuation is not bulk mail whatever the category
//! says.
//!
//! # What was deliberately not used
//!
//! Gmail's `IMPORTANT` label is the obvious candidate and it is the wrong one.
//! Google applies it generously — a large share of a busy mailbox's promotional
//! mail carries it — so notifying on importance would put most of the firehose
//! back. The same goes for "has attachments" and "addressed directly to me":
//! both are true of an enormous amount of automated mail.

/// The Gmail labels the rule reads. Named here so a typo is a compile error in
/// one place rather than a silent behaviour change in four.
pub const INBOX: &str = "INBOX";
pub const UNREAD: &str = "UNREAD";
pub const SENT: &str = "SENT";
pub const DRAFT: &str = "DRAFT";
pub const SPAM: &str = "SPAM";
pub const TRASH: &str = "TRASH";

/// The four tabs Gmail sorts bulk mail into. A message in any of them is
/// silent unless it continues a conversation this account has written to.
///
/// `CATEGORY_PERSONAL` is deliberately absent: it is the one category that
/// means "this is not bulk", and a message with no category at all — which is
/// what most mail in an account with the tabs turned off looks like — is
/// treated the same way.
pub const BULK_CATEGORIES: [&str; 4] = [
    "CATEGORY_PROMOTIONS",
    "CATEGORY_SOCIAL",
    "CATEGORY_UPDATES",
    "CATEGORY_FORUMS",
];

/// Tabs Gmail splits out of Primary. The badge and the Inbox list use this
/// cut; banners still use [`BULK_CATEGORIES`] so an Updates digest does not
/// interrupt just because it now appears in the list.
pub const PRIMARY_EXCLUDED: [&str; 2] = ["CATEGORY_PROMOTIONS", "CATEGORY_SOCIAL"];

/// One newly stored message, with everything the rule needs about it.
///
/// Built by [`super::hydrate`] from rows that were written moments earlier, so
/// every field here is what the store believes rather than what the wire said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    pub gmail_message_id: String,
    /// The local `threads.id`, which is what the UI selects a conversation by.
    pub thread_id: i64,
    pub gmail_thread_id: String,
    pub from_name: Option<String>,
    pub from_email: String,
    pub subject: String,
    /// Gmail's own one-line preview of the body, as stored. Already
    /// entity-decoded — see [`crate::db::backfill::decode_snippets`].
    pub snippet: String,
    /// The message's full Gmail label set.
    pub labels: Vec<String>,
    /// Whether some *other* message in the same thread was sent from this
    /// account's own address — "I am part of this conversation".
    pub thread_has_own_message: bool,
}

impl Arrival {
    fn has(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label)
    }

    /// The name to put in front of a person. Falls back to the address, and
    /// then to something rather than an empty banner.
    pub fn sender(&self) -> &str {
        match self.from_name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => name,
            _ if !self.from_email.is_empty() => &self.from_email,
            _ => "Someone",
        }
    }

    /// The subject, or a stand-in. Mail with no subject is rare and real, and a
    /// blank middle line reads as a rendering bug rather than as an empty
    /// subject.
    pub fn subject_line(&self) -> &str {
        match self.subject.trim() {
            "" => "(no subject)",
            subject => subject,
        }
    }
}

/// Whether this arrival is worth interrupting the owner for.
///
/// `own_address` is the account's own email; comparison is case-insensitive
/// because addresses are, and Gmail echoes back whatever case the sender used.
pub fn earns_a_banner(arrival: &Arrival, own_address: &str) -> bool {
    if !arrival.has(UNREAD) || !arrival.has(INBOX) {
        return false;
    }
    if arrival.has(SENT) || arrival.has(DRAFT) || arrival.has(SPAM) || arrival.has(TRASH) {
        return false;
    }
    // A message from yourself with no SENT label happens: mail to a list you
    // are on comes back around, and Gmail does not always label the copy.
    if arrival.from_email.eq_ignore_ascii_case(own_address) {
        return false;
    }

    let bulk = arrival
        .labels
        .iter()
        .any(|l| BULK_CATEGORIES.contains(&l.as_str()));

    !bulk || arrival.thread_has_own_message
}

/// What a notification actually says.
///
/// Three fields because macOS draws three lines: title and subtitle in bold,
/// then the body. Mail has exactly that shape — who it is from, what it is
/// about, what it says — and squeezing it into two lines was throwing one of
/// them away. `subtitle` is `Option` because the platform treats an empty
/// subtitle as "no second line" rather than as a blank one, and because a
/// notifier that cannot draw three lines should collapse rather than invent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub title: String,
    pub subtitle: Option<String>,
    pub body: String,
}

/// How many senders a coalesced banner names before it starts counting.
const NAMED_SENDERS: usize = 3;

/// Turn everything that qualified in one sweep into a single notification.
///
/// Five messages arriving together is one banner that says so, not five
/// banners. The single-message case is the one that carries real information —
/// who it is from and what it is about — and the plural case answers the only
/// question a stack of banners could have answered anyway: how many, and from
/// whom.
///
/// `label_account` is the caller's answer to "does this person have more than
/// one mailbox". With one account the address is noise on every banner; with
/// several it is the first thing you want to know.
///
/// # The two shapes
///
/// ```text
///   One message                    Several
///   ┌──────────────────────────┐   ┌──────────────────────────┐
///   │ Anna Lee                 │   │ 3 new messages           │
///   │ Lunch?                   │   │ Anna Lee, Bob and Carol  │
///   │ Are you free Thursday…   │   │ Lunch?                   │
///   └──────────────────────────┘   └──────────────────────────┘
///     sender / subject / preview     count / who / newest subject
/// ```
///
/// The coalesced form keeps the wording it already had — the count as the
/// title, the same "A, B and 2 others" list — and spends the line it gained on
/// the newest subject, which is the one thing a stack of banners could have
/// told you that a count cannot.
pub fn digest(arrivals: &[Arrival], account_email: &str, label_account: bool) -> Option<Banner> {
    let (first, rest) = arrivals.split_first()?;
    let newest = arrivals.last().unwrap_or(first);

    let mut banner = if rest.is_empty() {
        Banner {
            title: first.sender().to_string(),
            subtitle: Some(first.subject_line().to_string()),
            body: first.snippet.trim().to_string(),
        }
    } else {
        Banner {
            title: format!("{} new messages", arrivals.len()),
            subtitle: Some(sender_list(arrivals)),
            body: newest.subject_line().to_string(),
        }
    };

    if label_account && !account_email.is_empty() {
        // Last, and on its own line: it is the answer to "which mailbox", which
        // only matters once you have read the rest.
        if !banner.body.is_empty() {
            banner.body.push('\n');
        }
        banner.body.push_str(account_email);
    }
    Some(banner)
}

/// "Anna Lee, Bob Chen and 2 others" — distinct senders, in arrival order.
fn sender_list(arrivals: &[Arrival]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for arrival in arrivals {
        let name = arrival.sender();
        if !names.contains(&name) {
            names.push(name);
        }
    }

    match names.len() {
        0 => String::new(),
        1 => names[0].to_string(),
        n if n <= NAMED_SENDERS => {
            let last = names.pop().unwrap_or_default();
            format!("{} and {}", names.join(", "), last)
        }
        n => {
            let extra = n - NAMED_SENDERS;
            let shown = names[..NAMED_SENDERS].join(", ");
            if extra == 1 {
                format!("{shown} and 1 other")
            } else {
                format!("{shown} and {extra} others")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arrival(labels: &[&str]) -> Arrival {
        Arrival {
            gmail_message_id: "m1".into(),
            thread_id: 1,
            gmail_thread_id: "t1".into(),
            from_name: Some("Anna Lee".into()),
            from_email: "anna@example.com".into(),
            subject: "Lunch?".into(),
            snippet: "Are you free Thursday around one?".into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            thread_has_own_message: false,
        }
    }

    const ME: &str = "alex@example.com";

    #[test]
    fn plain_unread_inbox_mail_notifies() {
        assert!(earns_a_banner(&arrival(&[INBOX, UNREAD]), ME));
    }

    #[test]
    fn read_or_archived_mail_does_not() {
        assert!(!earns_a_banner(&arrival(&[INBOX]), ME), "already read");
        assert!(!earns_a_banner(&arrival(&[UNREAD]), ME), "filtered past the inbox");
    }

    #[test]
    fn your_own_mail_never_notifies() {
        assert!(!earns_a_banner(&arrival(&[INBOX, UNREAD, SENT]), ME));

        let mut mine = arrival(&[INBOX, UNREAD]);
        mine.from_email = "ALEX@example.com".into();
        assert!(!earns_a_banner(&mine, ME), "case-insensitive, like every address");
    }

    #[test]
    fn spam_drafts_and_trash_never_notify() {
        for label in [SPAM, TRASH, DRAFT] {
            assert!(
                !earns_a_banner(&arrival(&[INBOX, UNREAD, label]), ME),
                "{label} should be silent"
            );
        }
    }

    #[test]
    fn bulk_categories_are_silent_until_you_join_the_thread() {
        for category in BULK_CATEGORIES {
            let promo = arrival(&[INBOX, UNREAD, category]);
            assert!(!earns_a_banner(&promo, ME), "{category} should be silent");

            let mut reply = promo.clone();
            reply.thread_has_own_message = true;
            assert!(
                earns_a_banner(&reply, ME),
                "{category} should speak once it continues a conversation of mine"
            );
        }
    }

    #[test]
    fn personal_and_uncategorised_mail_both_notify() {
        assert!(earns_a_banner(&arrival(&[INBOX, UNREAD, "CATEGORY_PERSONAL"]), ME));
        assert!(earns_a_banner(&arrival(&[INBOX, UNREAD]), ME));
    }

    #[test]
    fn one_message_is_sender_then_subject_then_preview() {
        let banner = digest(&[arrival(&[INBOX, UNREAD])], "alex@example.com", false).unwrap();
        assert_eq!(banner.title, "Anna Lee");
        assert_eq!(banner.subtitle.as_deref(), Some("Lunch?"));
        assert_eq!(banner.body, "Are you free Thursday around one?");
    }

    #[test]
    fn a_subjectless_message_still_says_something() {
        let mut bare = arrival(&[INBOX, UNREAD]);
        bare.subject = "   ".into();
        let banner = digest(&[bare], "alex@example.com", false).unwrap();
        assert_eq!(banner.subtitle.as_deref(), Some("(no subject)"));
    }

    #[test]
    fn a_message_with_no_preview_leaves_the_line_empty_rather_than_padding_it() {
        let mut terse = arrival(&[INBOX, UNREAD]);
        terse.snippet = "   ".into();
        let banner = digest(&[terse], "alex@example.com", false).unwrap();
        assert_eq!(banner.title, "Anna Lee");
        assert_eq!(banner.subtitle.as_deref(), Some("Lunch?"));
        assert_eq!(banner.body, "", "no filler, and no stray whitespace");
    }

    #[test]
    fn several_messages_coalesce_into_one_banner() {
        let mut second = arrival(&[INBOX, UNREAD]);
        second.from_name = Some("Bob Chen".into());
        let mut third = arrival(&[INBOX, UNREAD]);
        third.from_name = Some("Carol Diaz".into());
        third.subject = "Thursday works".into();

        let banner = digest(
            &[arrival(&[INBOX, UNREAD]), second, third],
            "alex@example.com",
            false,
        )
        .unwrap();
        assert_eq!(banner.title, "3 new messages");
        assert_eq!(
            banner.subtitle.as_deref(),
            Some("Anna Lee, Bob Chen and Carol Diaz")
        );
        assert_eq!(
            banner.body, "Thursday works",
            "the line the count cannot give you: what the newest one is about"
        );
    }

    #[test]
    fn a_repeated_sender_is_named_once() {
        let banner = digest(
            &[arrival(&[INBOX, UNREAD]), arrival(&[INBOX, UNREAD])],
            "alex@example.com",
            false,
        )
        .unwrap();
        assert_eq!(banner.title, "2 new messages");
        assert_eq!(banner.subtitle.as_deref(), Some("Anna Lee"));
    }

    #[test]
    fn a_long_list_of_senders_starts_counting() {
        let many: Vec<Arrival> = ["A", "B", "C", "D", "E"]
            .iter()
            .map(|name| {
                let mut a = arrival(&[INBOX, UNREAD]);
                a.from_name = Some((*name).to_string());
                a.from_email = format!("{name}@example.com");
                a
            })
            .collect();
        let banner = digest(&many, "alex@example.com", false).unwrap();
        assert_eq!(banner.title, "5 new messages");
        assert_eq!(banner.subtitle.as_deref(), Some("A, B, C and 2 others"));
    }

    #[test]
    fn the_account_is_named_only_when_there_is_more_than_one() {
        let one = digest(&[arrival(&[INBOX, UNREAD])], "alex@example.com", false).unwrap();
        assert!(!one.body.contains("alex@example.com"));

        let several = digest(&[arrival(&[INBOX, UNREAD])], "alex@example.com", true).unwrap();
        assert!(several.body.ends_with("\nalex@example.com"));

        // …and it does not open with a blank line when there is no preview to
        // sit above it.
        let mut terse = arrival(&[INBOX, UNREAD]);
        terse.snippet = String::new();
        let alone = digest(&[terse], "alex@example.com", true).unwrap();
        assert_eq!(alone.body, "alex@example.com");
    }

    #[test]
    fn nothing_arrived_is_not_a_banner() {
        assert!(digest(&[], "alex@example.com", true).is_none());
    }

    #[test]
    fn a_sender_with_no_name_falls_back_to_the_address() {
        let mut anonymous = arrival(&[INBOX, UNREAD]);
        anonymous.from_name = None;
        assert_eq!(anonymous.sender(), "anna@example.com");

        anonymous.from_email = String::new();
        assert_eq!(anonymous.sender(), "Someone");
    }
}
