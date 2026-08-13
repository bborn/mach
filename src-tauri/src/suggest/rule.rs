//! Which mail deserves a reply written for it.
//!
//! One pure function over facts the sync loop already has in hand, so the
//! decision that governs every model call this feature makes is the one thing
//! that is trivial to test and cheap to tune.
//!
//! # The trigger
//!
//! **Any human message addressed to him.** That is broader than a category
//! filter or an importance heuristic, and it is the owner's choice — the whole
//! bet of the feature is that a reply he might actually send is worth writing
//! even for mail he would not have been notified about.
//!
//! Which puts all the weight on the exclusions. Everything below is either
//! *this is not addressed to me* or *this was not written by a person*, and
//! nothing below is a guess about whether the message is interesting.
//!
//! # The exclusions, and what each one is for
//!
//! | check | catches |
//! |---|---|
//! | `INBOX`, not `SENT`/`DRAFT`/`SPAM`/`TRASH` | store changes that are not arrivals |
//! | not from the account's own address | mail he sent to himself, and Gmail reporting his own send as an addition |
//! | his address in `To` or `Cc` | announcements, lists, and anything bcc-fanned to a thousand people |
//! | `List-Unsubscribe`, `List-Id` | every newsletter, every product mail, every notification digest |
//! | `Precedence: bulk\|list\|junk` | the older convention for the same thing |
//! | `Auto-Submitted` other than `no` | vacation responders, ticket systems, RFC 3834 senders generally |
//! | `X-Auto-Response-Suppress` | Exchange-side automation |
//! | a no-reply-shaped sender | the address that tells you not to bother |
//! | Gmail's four bulk categories | Linear, CI, billing, statements — unless he is already in the thread |
//!
//! The last one is the one doing the most work and the one most worth arguing
//! with. His inbox is 61,000 messages and mostly automated; Gmail has already
//! sorted that at Google's expense using signals this app will never have, and
//! `CATEGORY_UPDATES` is where notification mail lands even when it is addressed
//! to him by name. Deferring to that classification is not a heuristic Mach
//! invented — it is the same one he trusts enough to let it hide things behind
//! tabs, and it is the same call [`crate::notify::rule`] makes for banners.
//!
//! The escape hatch matters as much: a message in a de-prioritised category
//! still earns a suggestion when the thread already contains something *he*
//! sent. He started that conversation; its continuation is not bulk mail
//! whatever the category says.
//!
//! # What is deliberately not checked
//!
//! Whether the message asks a question. Whether it is short. Whether it is
//! important. Every one of those is a judgement about content, the model is
//! better at it than a predicate is, and getting it wrong here means the
//! suggestion never exists to be judged.

use std::collections::BTreeSet;

/// The Gmail labels the rule reads. Named so a typo is a compile error in one
/// place rather than a silent behaviour change in four.
pub const INBOX: &str = "INBOX";
pub const SENT: &str = "SENT";
pub const DRAFT: &str = "DRAFT";
pub const SPAM: &str = "SPAM";
pub const TRASH: &str = "TRASH";

/// The four tabs Gmail sorts bulk mail into. `CATEGORY_PERSONAL` is absent on
/// purpose: it is the category that means "not bulk", and a message with no
/// category at all — which is most mail in an account with the tabs off — is
/// treated the same way.
pub const BULK_CATEGORIES: [&str; 4] = [
    "CATEGORY_PROMOTIONS",
    "CATEGORY_SOCIAL",
    "CATEGORY_UPDATES",
    "CATEGORY_FORUMS",
];

/// Local parts that mean "do not answer this". Matched as whole words against
/// the part before the `@`, with `.`, `-`, `_` and `+` treated as separators, so
/// `no-reply`, `no_reply`, `noreply+123` and `donotreply` all land and
/// `norepli.co` does not.
/// `notifier` and `notify` are here on evidence rather than on principle: the
/// single highest-volume sender in the owner's mailbox is
/// `notifier@mail.rollbar.com` at 9,748 messages, up to 103 of them in one day,
/// and `notification`/`notifications` did not catch it. Gmail files those under
/// `CATEGORY_UPDATES`, so the category check was already declining them — but
/// that check is Google's judgement rather than ours, and a rule that depends on
/// somebody else's classifier for its biggest single case is one classifier
/// change away from a flood.
///
/// Deliberately **not** `alert`/`alerts`. `alerts@cronitor.io` is the owner's
/// highest-volume sender among mail that actually earns a suggestion, and
/// whether an alert deserves an answer is a judgement about content — which is
/// exactly what this predicate refuses to make.
const NO_REPLY_PARTS: [&str; 14] = [
    "noreply",
    "no-reply",
    "donotreply",
    "do-not-reply",
    "notification",
    "notifications",
    "notifier",
    "notify",
    "mailer-daemon",
    "postmaster",
    "bounce",
    "bounces",
    "automated",
    "autoreply",
];

/// Everything the rule needs about one message that just arrived, drawn from the
/// wire response while its headers are still in hand.
///
/// Addresses are stored exactly as parsed; comparison lowercases. Headers are
/// `Option<String>` because absent and empty are different facts — an empty
/// `Precedence` is a sender who wrote a broken header, not a sender who wrote
/// `bulk`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Candidate {
    pub from_email: String,
    /// Every address on `To`, as parsed.
    pub to: Vec<String>,
    /// Every address on `Cc`.
    pub cc: Vec<String>,
    /// The message's full Gmail label set.
    pub labels: Vec<String>,
    pub list_unsubscribe: Option<String>,
    pub list_id: Option<String>,
    pub precedence: Option<String>,
    pub auto_submitted: Option<String>,
    pub auto_response_suppress: Option<String>,
    /// Whether some other message in the same thread came from this account's
    /// own address — "I am part of this conversation".
    pub thread_has_own_message: bool,
}

/// Why a message was passed over. One variant per exclusion, so a test can name
/// the rule it is exercising and a log line says something useful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decline {
    /// Not in the inbox — filed, archived, or never delivered there.
    NotInInbox,
    /// Carries `SENT`, `DRAFT`, `SPAM` or `TRASH`.
    NotAnArrival,
    /// From the account's own address.
    FromMyself,
    /// The account's address is on neither `To` nor `Cc`.
    NotAddressedToMe,
    /// `List-Unsubscribe` or `List-Id`.
    ListMail,
    /// `Precedence: bulk`, `list` or `junk`.
    BulkPrecedence,
    /// `Auto-Submitted` says a machine sent it, or `X-Auto-Response-Suppress`.
    Automated,
    /// The sender's address says not to answer it.
    NoReplySender,
    /// In one of Gmail's bulk categories, with nothing of his in the thread.
    BulkCategory,
}

impl Decline {
    /// A short, stable name — for a log line, and for a test to assert on.
    pub fn as_str(self) -> &'static str {
        match self {
            Decline::NotInInbox => "notInInbox",
            Decline::NotAnArrival => "notAnArrival",
            Decline::FromMyself => "fromMyself",
            Decline::NotAddressedToMe => "notAddressedToMe",
            Decline::ListMail => "listMail",
            Decline::BulkPrecedence => "bulkPrecedence",
            Decline::Automated => "automated",
            Decline::NoReplySender => "noReplySender",
            Decline::BulkCategory => "bulkCategory",
        }
    }
}

/// Does this message deserve a reply written for it?
///
/// `Ok(())` means yes. `Err(reason)` names the first rule that said no; the
/// order below is cheapest-and-most-common first, which matters because this
/// runs against every message a history sweep brings in.
pub fn earns_a_suggestion(candidate: &Candidate, own_address: &str) -> Result<(), Decline> {
    let own = own_address.trim().to_ascii_lowercase();
    let labels: BTreeSet<String> = candidate
        .labels
        .iter()
        .map(|l| l.trim().to_ascii_uppercase())
        .collect();

    for label in [SENT, DRAFT, SPAM, TRASH] {
        if labels.contains(label) {
            return Err(Decline::NotAnArrival);
        }
    }
    if !labels.contains(INBOX) {
        return Err(Decline::NotInInbox);
    }

    let from = candidate.from_email.trim().to_ascii_lowercase();
    if !own.is_empty() && from == own {
        return Err(Decline::FromMyself);
    }

    // The owner's choice of trigger, stated exactly: *addressed to him*. An
    // empty own address cannot answer the question, and answering "yes" to a
    // question you cannot evaluate is how a filter stops filtering.
    let addressed = candidate
        .to
        .iter()
        .chain(candidate.cc.iter())
        .any(|a| a.trim().to_ascii_lowercase() == own);
    if own.is_empty() || !addressed {
        return Err(Decline::NotAddressedToMe);
    }

    if present(&candidate.list_unsubscribe) || present(&candidate.list_id) {
        return Err(Decline::ListMail);
    }

    if let Some(precedence) = candidate.precedence.as_deref() {
        let value = precedence.trim().to_ascii_lowercase();
        if value == "bulk" || value == "list" || value == "junk" {
            return Err(Decline::BulkPrecedence);
        }
    }

    // RFC 3834: `no` is the only value that means a person typed it. Anything
    // else — `auto-generated`, `auto-replied`, a value nobody has heard of — is
    // a machine saying so.
    if let Some(auto) = candidate.auto_submitted.as_deref() {
        let value = auto.trim().to_ascii_lowercase();
        if !value.is_empty() && value != "no" {
            return Err(Decline::Automated);
        }
    }
    if present(&candidate.auto_response_suppress) {
        return Err(Decline::Automated);
    }

    if is_no_reply(&from) {
        return Err(Decline::NoReplySender);
    }

    let bulk = labels
        .iter()
        .any(|l| BULK_CATEGORIES.iter().any(|c| l == c));
    if bulk && !candidate.thread_has_own_message {
        return Err(Decline::BulkCategory);
    }

    Ok(())
}

fn present(header: &Option<String>) -> bool {
    header.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false)
}

/// Whether an address's local part is one of the ones that mean "not a person".
///
/// Tokenised rather than substring-matched. `contains("bounce")` would refuse to
/// answer somebody at `bouncehouse.com`, and the address before the `@` is the
/// only part that carries the convention anyway.
pub fn is_no_reply(address: &str) -> bool {
    let local = match address.split_once('@') {
        Some((local, _)) => local,
        None => address,
    }
    .to_ascii_lowercase();

    // Whole-token match, and also the hyphenated forms, which are two tokens
    // once `-` is a separator: "no" + "reply".
    let tokens: Vec<&str> = local
        .split(|c: char| c == '.' || c == '-' || c == '_' || c == '+')
        .filter(|t| !t.is_empty())
        .collect();

    for candidate in NO_REPLY_PARTS {
        if local == candidate {
            return true;
        }
        // `no-reply` and `do-not-reply` arrive as separate tokens once the
        // separators are gone; rejoining without them catches every spelling.
        let joined = tokens.join("");
        if joined == candidate.replace('-', "") {
            return true;
        }
    }
    tokens.iter().any(|t| {
        matches!(
            *t,
            "noreply"
                | "donotreply"
                | "notification"
                | "notifications"
                | "notifier"
                | "notify"
                | "postmaster"
                | "bounce"
                | "bounces"
                | "automated"
                | "autoreply"
                | "mailer"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "bruno@example.com";

    fn human() -> Candidate {
        Candidate {
            from_email: "kate@example.org".into(),
            to: vec![ME.into()],
            cc: vec![],
            labels: vec!["INBOX".into(), "UNREAD".into()],
            ..Default::default()
        }
    }

    #[test]
    fn a_person_writing_to_him_earns_one() {
        assert_eq!(earns_a_suggestion(&human(), ME), Ok(()));
    }

    #[test]
    fn a_person_writing_to_him_on_cc_earns_one() {
        let mut c = human();
        c.to = vec!["someone@else.com".into()];
        c.cc = vec![ME.into()];
        assert_eq!(earns_a_suggestion(&c, ME), Ok(()));
    }

    #[test]
    fn the_address_comparison_ignores_case_and_whitespace() {
        let mut c = human();
        c.to = vec![" Bruno@Example.COM ".into()];
        assert_eq!(earns_a_suggestion(&c, ME), Ok(()));
    }

    #[test]
    fn mail_outside_the_inbox_is_declined() {
        let mut c = human();
        c.labels = vec!["UNREAD".into()];
        assert_eq!(earns_a_suggestion(&c, ME), Err(Decline::NotInInbox));
    }

    #[test]
    fn sent_drafts_spam_and_trash_are_not_arrivals() {
        for label in ["SENT", "DRAFT", "SPAM", "TRASH"] {
            let mut c = human();
            c.labels.push(label.into());
            assert_eq!(
                earns_a_suggestion(&c, ME),
                Err(Decline::NotAnArrival),
                "{label} should not be an arrival"
            );
        }
    }

    #[test]
    fn his_own_mail_is_declined() {
        let mut c = human();
        c.from_email = "BRUNO@example.com".into();
        assert_eq!(earns_a_suggestion(&c, ME), Err(Decline::FromMyself));
    }

    #[test]
    fn mail_not_addressed_to_him_is_declined() {
        let mut c = human();
        c.to = vec!["everyone@example.org".into()];
        assert_eq!(earns_a_suggestion(&c, ME), Err(Decline::NotAddressedToMe));
    }

    #[test]
    fn an_unknown_own_address_declines_rather_than_letting_everything_through() {
        assert_eq!(earns_a_suggestion(&human(), ""), Err(Decline::NotAddressedToMe));
    }

    #[test]
    fn list_mail_is_declined_by_either_header() {
        let mut unsub = human();
        unsub.list_unsubscribe = Some("<https://example.org/u/9>".into());
        assert_eq!(earns_a_suggestion(&unsub, ME), Err(Decline::ListMail));

        let mut id = human();
        id.list_id = Some("<announce.example.org>".into());
        assert_eq!(earns_a_suggestion(&id, ME), Err(Decline::ListMail));
    }

    #[test]
    fn an_empty_list_header_is_not_a_list() {
        let mut c = human();
        c.list_unsubscribe = Some("   ".into());
        c.list_id = Some(String::new());
        assert_eq!(earns_a_suggestion(&c, ME), Ok(()));
    }

    #[test]
    fn bulk_precedence_is_declined() {
        for value in ["bulk", "List", " JUNK "] {
            let mut c = human();
            c.precedence = Some(value.into());
            assert_eq!(
                earns_a_suggestion(&c, ME),
                Err(Decline::BulkPrecedence),
                "Precedence: {value}"
            );
        }
    }

    #[test]
    fn other_precedence_values_pass() {
        let mut c = human();
        c.precedence = Some("first-class".into());
        assert_eq!(earns_a_suggestion(&c, ME), Ok(()));
    }

    #[test]
    fn auto_submitted_anything_but_no_is_automated() {
        for value in ["auto-generated", "auto-replied", "auto-notified"] {
            let mut c = human();
            c.auto_submitted = Some(value.into());
            assert_eq!(
                earns_a_suggestion(&c, ME),
                Err(Decline::Automated),
                "Auto-Submitted: {value}"
            );
        }
        let mut typed_by_a_person = human();
        typed_by_a_person.auto_submitted = Some("no".into());
        assert_eq!(earns_a_suggestion(&typed_by_a_person, ME), Ok(()));
    }

    #[test]
    fn exchange_suppression_is_automated() {
        let mut c = human();
        c.auto_response_suppress = Some("All".into());
        assert_eq!(earns_a_suggestion(&c, ME), Err(Decline::Automated));
    }

    #[test]
    fn no_reply_senders_are_declined() {
        for address in [
            "noreply@linear.app",
            "no-reply@github.com",
            "do-not-reply@bank.example",
            "donotreply@stripe.com",
            "notifications@slack.com",
            "bounces@sendgrid.net",
            "mailer-daemon@example.org",
            "postmaster@example.org",
            "noreply+abc123@notion.so",
            // The biggest single sender in his mailbox, and the one the list
            // used to miss.
            "notifier@mail.rollbar.com",
            "notify@mail.notion.so",
        ] {
            let mut c = human();
            c.from_email = address.into();
            assert_eq!(
                earns_a_suggestion(&c, ME),
                Err(Decline::NoReplySender),
                "{address} should be a no-reply sender"
            );
        }
    }

    #[test]
    fn an_alerting_sender_is_still_a_candidate() {
        // The rule declines machines, not topics. His highest-volume sender
        // among mail that earns a suggestion is an alerting address, and
        // whether an alert deserves an answer is the model's judgement to make.
        for address in ["alerts@cronitor.io", "alert@example.org"] {
            let mut c = human();
            c.from_email = address.into();
            assert_eq!(earns_a_suggestion(&c, ME), Ok(()), "{address}");
        }
    }

    #[test]
    fn an_address_that_merely_contains_a_no_reply_word_is_a_person() {
        for address in ["kate@bouncehouse.com", "rob@noreply.dev"] {
            let mut c = human();
            c.from_email = address.into();
            assert_eq!(earns_a_suggestion(&c, ME), Ok(()), "{address} is a person");
        }
    }

    #[test]
    fn gmails_bulk_categories_are_declined() {
        for category in BULK_CATEGORIES {
            let mut c = human();
            c.labels.push(category.into());
            assert_eq!(
                earns_a_suggestion(&c, ME),
                Err(Decline::BulkCategory),
                "{category} should be declined"
            );
        }
    }

    #[test]
    fn a_bulk_category_is_forgiven_in_a_thread_he_has_written_to() {
        let mut c = human();
        c.labels.push("CATEGORY_UPDATES".into());
        c.thread_has_own_message = true;
        assert_eq!(earns_a_suggestion(&c, ME), Ok(()));
    }

    #[test]
    fn category_personal_is_not_bulk() {
        let mut c = human();
        c.labels.push("CATEGORY_PERSONAL".into());
        assert_eq!(earns_a_suggestion(&c, ME), Ok(()));
    }

    #[test]
    fn every_decline_has_a_name() {
        let all = [
            Decline::NotInInbox,
            Decline::NotAnArrival,
            Decline::FromMyself,
            Decline::NotAddressedToMe,
            Decline::ListMail,
            Decline::BulkPrecedence,
            Decline::Automated,
            Decline::NoReplySender,
            Decline::BulkCategory,
        ];
        let names: BTreeSet<&str> = all.iter().map(|d| d.as_str()).collect();
        assert_eq!(names.len(), all.len(), "two declines share a name");
    }
}
