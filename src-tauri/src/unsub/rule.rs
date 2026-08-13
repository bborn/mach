//! Whether it is safe to unsubscribe from a message, or whether the honest
//! offer is Gmail's spam report instead.
//!
//! One pure function. It opens no database, makes no request and reads no
//! clock, so the decision that governs every outbound byte this feature sends
//! is the one thing that is trivial to test and cheap to argue with.
//!
//! # Why this needs a rule at all
//!
//! An unsubscribe is a message to the sender saying *this address is live and
//! somebody reads it*. For a newsletter he asked for, that is exactly the point
//! and the sender will honour it. For a spammer it is the most valuable thing
//! he could possibly send them: a confirmed, monitored address, worth more sold
//! on than the original list was. The industry advice has been "never
//! unsubscribe from spam" for twenty years and it is still right.
//!
//! So a feature that offered "unsubscribe" on anything carrying a
//! `List-Unsubscribe` header would be a machine for confirming his address to
//! whoever asked. The header is free to write and a spammer has more reason to
//! write it than a legitimate sender does.
//!
//! Gmail already has the answer for that mail, and Mach already has the
//! gesture: **report spam**. So the rule below has three outcomes rather than
//! two, and the third one is the interesting one.
//!
//! # The rule
//!
//! Offer to unsubscribe when **all** of these hold:
//!
//! 1. the message is not `SENT`, `DRAFT` or `TRASH` — nothing to act on;
//! 2. `List-Unsubscribe` parses to a target Mach is willing to touch (see
//!    [`super::target`], which is where `javascript:` and `http:` die);
//! 3. it does not carry `SPAM` — Gmail has already been told, and telling the
//!    sender as well is the one thing that could still make it worse;
//! 4. it is **recognisably bulk mail**: `List-Id`, or `Precedence: bulk|list|
//!    junk`, or one of Gmail's four bulk categories, in addition to the
//!    unsubscribe header;
//! 5. and it is from an **established sender**: he has written to them, or the
//!    store already holds [`ESTABLISHED_MESSAGE_COUNT`] messages from that
//!    address.
//!
//! Fail 4 or 5 and the message has an unsubscribe link but nothing vouching
//! for the sender, which is the shape of a blast rather than a subscription.
//! That is [`Verdict::ReportSpam`], and the UI says so in those words.
//!
//! # Arguing with each check
//!
//! **3 — the `SPAM` label.** Google's filter is the best signal available here
//! by a wide margin, computed over signals this app will never have, and it is
//! already the one he trusts enough to let it hide mail. Deferring to it costs
//! nothing.
//!
//! **4 — a second bulk marker.** A message Gmail filed as Personal, with no
//! `List-Id` and no `Precedence`, carrying a lone `List-Unsubscribe`, is
//! unusual for a real newsletter and is exactly the shape of a header forged
//! onto a targeted message to get a `POST` out of the client. Real senders
//! large enough to matter set at least one of the three. The cost of this
//! check is a false negative on a small, sloppy sender — and the fallback for
//! a false negative is the link in the message body, which is where he is
//! today.
//!
//! **5 — an established sender.** The number is small on purpose. His store
//! holds a twelve-month backfill, so a newsletter he actually subscribes to has
//! sent three; a blast has sent one. Counted on the exact `From` address rather
//! than the domain, because a domain is a weaker claim — a spammer controls
//! their whole domain and can send three messages from three addresses under
//! it.
//!
//! # What this gets wrong
//!
//! * **A first issue.** He signs up on Tuesday, the welcome mail arrives, he
//!   changes his mind. One message from the sender, never written to them:
//!   declined as `UnknownSender`, and offered spam report — which is not what
//!   he means. The link in the body still works.
//! * **A sender who rotates their `From`.** `newsletter-8842@sender.example`
//!   never repeats, so the count never reaches three and every issue looks
//!   like a first issue. Counting by `List-Id` would fix this and is the
//!   obvious next move.
//! * **A patient spammer.** Three messages over three months from one address,
//!   through Gmail's filter, with a `List-Id`, gets offered. This rule buys
//!   time and evidence, not certainty. It cannot be made certain.
//! * **Mail he was subscribed to without asking.** A vendor who bought his
//!   address, mails compliantly, and is not spam by Gmail's reckoning is
//!   indistinguishable here from one he opted into. Unsubscribing is probably
//!   still the right move, but the rule is not making that judgement — it is
//!   making a narrower one about whether the sender is real.
//! * **Everything before the header was stored.** `list_unsubscribe` is a
//!   column as of migration 18; a message synced before that has `NULL` and
//!   reads as [`Decline::NoHeader`] whatever it actually carried. See
//!   [`super`].

use crate::unsub::target::{self, Target, Unusable};

/// Gmail labels this rule reads.
pub use crate::suggest::rule::{BULK_CATEGORIES, DRAFT, SENT, SPAM, TRASH};

/// How many messages from one address make it a sender rather than a stranger.
///
/// Three, against a twelve-month backfill. See the module doc for why this is
/// counted on the address and not the domain.
pub const ESTABLISHED_MESSAGE_COUNT: i64 = 3;

/// Everything the rule needs about one message. Assembled by
/// [`super::store`] from rows; nothing here comes off the wire at decision
/// time, so the verdict is a function of what the store believes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Candidate {
    pub from_email: String,
    /// The message's full Gmail label set.
    pub labels: Vec<String>,
    pub list_unsubscribe: Option<String>,
    pub list_unsubscribe_post: Option<String>,
    pub list_id: Option<String>,
    pub precedence: Option<String>,
    /// How many messages the store holds from this exact `From` address.
    pub messages_from_sender: i64,
    /// Whether anything he sent has this sender on `To` or `Cc`.
    pub has_written_to_sender: bool,
}

/// Why the message was not offered an unsubscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decline {
    /// No `List-Unsubscribe`, or one that was never stored.
    NoHeader,
    /// A header that named nothing Mach is willing to touch.
    Unusable(Unusable),
    /// Carries `SENT` or `DRAFT` — his own mail.
    NotAnArrival,
    /// In the trash.
    InTrash,
    /// Already in Spam. Gmail knows; the sender does not need to.
    AlreadySpam,
    /// An unsubscribe header with no other sign that this is bulk mail.
    NotBulkMail,
    /// Nothing in the store vouches for this sender.
    UnknownSender,
}

impl Decline {
    /// A short, stable name — for a log line, and for a test to assert on.
    pub fn as_str(self) -> &'static str {
        match self {
            Decline::NoHeader => "noHeader",
            Decline::Unusable(_) => "unusable",
            Decline::NotAnArrival => "notAnArrival",
            Decline::InTrash => "inTrash",
            Decline::AlreadySpam => "alreadySpam",
            Decline::NotBulkMail => "notBulkMail",
            Decline::UnknownSender => "unknownSender",
        }
    }
}

/// What to offer on a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Unsubscribe, this way.
    Unsubscribe(Target),
    /// It has an unsubscribe, and using it would confirm his address to a
    /// sender nothing vouches for. Gmail's spam report is the honest offer.
    ReportSpam(Decline),
    /// Offer nothing.
    Nothing(Decline),
}

impl Verdict {
    pub fn target(&self) -> Option<&Target> {
        match self {
            Verdict::Unsubscribe(target) => Some(target),
            _ => None,
        }
    }

    /// The decline behind a non-offer, for a log line and for the UI's copy.
    pub fn decline(&self) -> Option<Decline> {
        match self {
            Verdict::Unsubscribe(_) => None,
            Verdict::ReportSpam(reason) | Verdict::Nothing(reason) => Some(*reason),
        }
    }
}

/// The whole decision. See the module doc for the rule and its failure modes.
pub fn verdict(candidate: &Candidate) -> Verdict {
    let labels: Vec<String> = candidate
        .labels
        .iter()
        .map(|l| l.trim().to_ascii_uppercase())
        .collect();
    let has = |label: &str| labels.iter().any(|l| l == label);

    if has(SENT) || has(DRAFT) {
        return Verdict::Nothing(Decline::NotAnArrival);
    }
    if has(TRASH) {
        return Verdict::Nothing(Decline::InTrash);
    }

    // Before anything about the sender, because a message with no unsubscribe
    // in it is an ordinary message and must not be accused of anything.
    let target = match target::parse(
        candidate.list_unsubscribe.as_deref(),
        candidate.list_unsubscribe_post.as_deref(),
    ) {
        Ok(target) => target,
        Err(Unusable::Empty) => return Verdict::Nothing(Decline::NoHeader),
        Err(reason) => return Verdict::Nothing(Decline::Unusable(reason)),
    };

    if has(SPAM) {
        return Verdict::Nothing(Decline::AlreadySpam);
    }

    if !is_bulk(candidate, &labels) {
        return Verdict::ReportSpam(Decline::NotBulkMail);
    }
    if !is_established(candidate) {
        return Verdict::ReportSpam(Decline::UnknownSender);
    }

    Verdict::Unsubscribe(target)
}

/// Whether anything besides `List-Unsubscribe` says this is bulk mail.
fn is_bulk(candidate: &Candidate, labels: &[String]) -> bool {
    if present(&candidate.list_id) {
        return true;
    }
    if let Some(precedence) = candidate.precedence.as_deref() {
        let value = precedence.trim().to_ascii_lowercase();
        if value == "bulk" || value == "list" || value == "junk" {
            return true;
        }
    }
    labels
        .iter()
        .any(|l| BULK_CATEGORIES.iter().any(|category| l == category))
}

/// Whether the store has any reason to believe in this sender.
fn is_established(candidate: &Candidate) -> bool {
    candidate.has_written_to_sender
        || candidate.messages_from_sender >= ESTABLISHED_MESSAGE_COUNT
}

fn present(header: &Option<String>) -> bool {
    header.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_CLICK: &str = "List-Unsubscribe=One-Click";

    /// A newsletter of the shape most of his actually are: Gmail filed it under
    /// Promotions, the sender implements RFC 8058, and it has been arriving for
    /// months.
    fn newsletter() -> Candidate {
        Candidate {
            from_email: "hello@stratechery.com".into(),
            labels: vec!["INBOX".into(), "CATEGORY_PROMOTIONS".into(), "UNREAD".into()],
            list_unsubscribe: Some("<https://stratechery.com/u/9f2a>".into()),
            list_unsubscribe_post: Some(ONE_CLICK.into()),
            list_id: Some("<daily.stratechery.com>".into()),
            precedence: None,
            messages_from_sender: 84,
            has_written_to_sender: false,
        }
    }

    // ---------------------------------------------------------- the yes case

    #[test]
    fn a_newsletter_he_has_been_getting_for_months_is_offered_one_click() {
        assert_eq!(
            verdict(&newsletter()),
            Verdict::Unsubscribe(Target::OneClick {
                url: "https://stratechery.com/u/9f2a".into()
            })
        );
    }

    #[test]
    fn a_mailing_list_with_only_a_mailto_is_offered_the_mail() {
        let mut c = newsletter();
        c.from_email = "announce@lists.example.org".into();
        c.list_unsubscribe = Some("<mailto:announce-leave@lists.example.org?subject=leave>".into());
        c.list_unsubscribe_post = None;
        assert_eq!(
            verdict(&c),
            Verdict::Unsubscribe(Target::Mail {
                to: vec!["announce-leave@lists.example.org".into()],
                subject: "leave".into(),
                body: None,
            })
        );
    }

    #[test]
    fn a_sender_he_has_written_to_is_established_on_one_message() {
        let mut c = newsletter();
        c.messages_from_sender = 1;
        c.has_written_to_sender = true;
        assert!(matches!(verdict(&c), Verdict::Unsubscribe(_)));
    }

    #[test]
    fn precedence_alone_is_enough_to_be_bulk_mail() {
        let mut c = newsletter();
        c.list_id = None;
        c.labels = vec!["INBOX".into()];
        c.precedence = Some("bulk".into());
        assert!(matches!(verdict(&c), Verdict::Unsubscribe(_)));
    }

    #[test]
    fn a_gmail_bulk_category_alone_is_enough_to_be_bulk_mail() {
        for category in BULK_CATEGORIES {
            let mut c = newsletter();
            c.list_id = None;
            c.precedence = None;
            c.labels = vec!["INBOX".into(), category.into()];
            assert!(
                matches!(verdict(&c), Verdict::Unsubscribe(_)),
                "{category} should count as bulk"
            );
        }
    }

    #[test]
    fn an_archived_newsletter_is_still_offered() {
        let mut c = newsletter();
        c.labels = vec!["CATEGORY_PROMOTIONS".into()];
        assert!(matches!(verdict(&c), Verdict::Unsubscribe(_)));
    }

    // ------------------------------------------------- nothing to act on

    #[test]
    fn a_message_with_no_unsubscribe_header_is_offered_nothing() {
        let mut c = newsletter();
        c.list_unsubscribe = None;
        assert_eq!(verdict(&c), Verdict::Nothing(Decline::NoHeader));

        c.list_unsubscribe = Some("   ".into());
        assert_eq!(verdict(&c), Verdict::Nothing(Decline::NoHeader));
    }

    #[test]
    fn a_header_naming_a_scheme_we_refuse_is_offered_nothing() {
        let mut c = newsletter();
        c.list_unsubscribe = Some("<javascript:alert(1)>".into());
        assert_eq!(
            verdict(&c),
            Verdict::Nothing(Decline::Unusable(Unusable::BadScheme))
        );

        c.list_unsubscribe = Some("<http://stratechery.com/u/9f2a>".into());
        assert_eq!(
            verdict(&c),
            Verdict::Nothing(Decline::Unusable(Unusable::BadScheme))
        );

        c.list_unsubscribe = Some("<https://127.0.0.1:1420/u>".into());
        assert_eq!(
            verdict(&c),
            Verdict::Nothing(Decline::Unusable(Unusable::UnsafeHost))
        );
    }

    #[test]
    fn his_own_sent_mail_and_drafts_are_offered_nothing() {
        for label in [SENT, DRAFT] {
            let mut c = newsletter();
            c.labels.push(label.into());
            assert_eq!(verdict(&c), Verdict::Nothing(Decline::NotAnArrival), "{label}");
        }
    }

    #[test]
    fn trashed_mail_is_offered_nothing() {
        let mut c = newsletter();
        c.labels.push(TRASH.into());
        assert_eq!(verdict(&c), Verdict::Nothing(Decline::InTrash));
    }

    // ----------------------------------------------- the spam-report cases

    #[test]
    fn mail_already_in_spam_is_never_unsubscribed_from() {
        // The whole point: Gmail has been told. Telling the sender as well is
        // the only move left that makes it worse.
        let mut c = newsletter();
        c.labels = vec!["SPAM".into()];
        c.messages_from_sender = 400;
        c.has_written_to_sender = true;
        assert_eq!(verdict(&c), Verdict::Nothing(Decline::AlreadySpam));
    }

    #[test]
    fn a_stranger_with_a_perfect_unsubscribe_header_gets_the_spam_report() {
        let mut c = newsletter();
        c.from_email = "offers@bargain-deals.example".into();
        c.messages_from_sender = 1;
        c.has_written_to_sender = false;
        assert_eq!(verdict(&c), Verdict::ReportSpam(Decline::UnknownSender));
    }

    #[test]
    fn a_lone_unsubscribe_header_on_personal_mail_gets_the_spam_report() {
        // No List-Id, no Precedence, no Gmail category: the shape of a header
        // forged onto a targeted message.
        let mut c = newsletter();
        c.list_id = None;
        c.precedence = None;
        c.labels = vec!["INBOX".into(), "CATEGORY_PERSONAL".into()];
        assert_eq!(verdict(&c), Verdict::ReportSpam(Decline::NotBulkMail));
    }

    #[test]
    fn one_click_headers_do_not_by_themselves_vouch_for_a_sender() {
        // A spammer can write `List-Unsubscribe-Post` as easily as anyone.
        let mut c = newsletter();
        c.messages_from_sender = 2;
        c.has_written_to_sender = false;
        assert_eq!(verdict(&c), Verdict::ReportSpam(Decline::UnknownSender));
    }

    #[test]
    fn the_established_threshold_is_where_it_says_it_is() {
        let mut c = newsletter();
        c.has_written_to_sender = false;

        c.messages_from_sender = ESTABLISHED_MESSAGE_COUNT - 1;
        assert_eq!(verdict(&c), Verdict::ReportSpam(Decline::UnknownSender));

        c.messages_from_sender = ESTABLISHED_MESSAGE_COUNT;
        assert!(matches!(verdict(&c), Verdict::Unsubscribe(_)));
    }

    // -------------------------------------------------------- housekeeping

    #[test]
    fn a_verdict_reports_its_reason() {
        assert_eq!(
            verdict(&Candidate::default()).decline(),
            Some(Decline::NoHeader)
        );
        assert!(verdict(&newsletter()).target().is_some());
        assert!(verdict(&newsletter()).decline().is_none());
    }

    #[test]
    fn every_decline_has_a_name() {
        use std::collections::BTreeSet;
        let all = [
            Decline::NoHeader,
            Decline::Unusable(Unusable::Empty),
            Decline::NotAnArrival,
            Decline::InTrash,
            Decline::AlreadySpam,
            Decline::NotBulkMail,
            Decline::UnknownSender,
        ];
        let names: BTreeSet<&str> = all.iter().map(|d| d.as_str()).collect();
        assert_eq!(names.len(), all.len(), "two declines share a name");
    }
}
