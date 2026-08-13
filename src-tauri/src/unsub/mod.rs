//! Leaving a mailing list without leaving the app.
//!
//! One gesture on a newsletter and it is dealt with: the conversation goes to
//! the archive immediately and the unsubscribe itself happens behind it. No
//! browser tab, no form, no waiting on the network before the app answers.
//!
//! # Three mechanisms, and only two of them are here
//!
//! | mechanism | what it takes | built |
//! |---|---|---|
//! | RFC 8058 one-click | one `POST` of a fixed body to an `https` URL | yes |
//! | RFC 2369 `mailto:` | one message to a given address with a given subject | yes |
//! | a bare `https` link | a browser, and often a form with a submit button | no — it opens the page |
//!
//! One-click covers most of what actually arrives. Google and Yahoo have
//! required it of bulk senders since February 2024, along with authentication
//! and a spam-rate ceiling, so any sender large enough to be worth unsubscribing
//! from has implemented it. `mailto:` covers the older, smaller lists —
//! Mailman, university announce lists — and Mach already sends mail, so it is a
//! send with no composer rather than a new capability.
//!
//! The third is the one that would need a browser, and it is discussed under
//! *What is not built* below.
//!
//! # The judgement this feature is mostly made of
//!
//! An unsubscribe tells the sender that this address is live and read. For a
//! newsletter he asked for, that is the point. For spam it is the single worst
//! thing he could send them, and it has been the standing advice for twenty
//! years not to.
//!
//! So the header is not the trigger. [`rule`] is, and it has three outcomes:
//! unsubscribe, *report spam instead*, or nothing. Everything interesting about
//! this feature is in that file, including a list of what it gets wrong.
//!
//! # Layout
//!
//! | module | what lives there |
//! |---|---|
//! | [`target`] | parsing `List-Unsubscribe`, and every scheme and host refusal |
//! | [`rule`] | whether it may be offered at all |
//! | [`store`] | the SQLite reads behind a [`rule::Candidate`] |
//! | [`http`] | the hardened client, which is not the one the rest of the app uses |
//! | [`run`] | doing it, and saying truthfully whether it worked |
//!
//! The action itself is [`Command::Unsubscribe`](crate::commands::Command),
//! which puts it in the catalogue the agent reads and gives it the same
//! failure reporting every other write has.
//!
//! # Security, in one place
//!
//! The unsubscribe URL is a string a stranger put in a message.
//!
//!  * **Only `https` and `mailto`.** `http` is refused rather than upgraded,
//!    and `javascript:`, `data:` and `file:` never get held at all. [`target`].
//!  * **Never navigated to in the webview.** The `POST` is made from Rust. For
//!    the one case Mach will not automate, the URL goes to the system browser
//!    through the opener plugin, which is a different process with a different
//!    origin and no access to anything of his.
//!  * **The URL never reaches the frontend.** The UI is told which of three
//!    kinds the offer is and nothing else, so there is no string in the webview
//!    for a rendered message to reach.
//!  * **No credentials, no cookies, no referrer, no proxy, capped response,
//!    fifteen-second timeout, at most three redirects and only to hosts that
//!    would have passed validation.** [`http`].
//!  * **Nothing fires on its own.** There is no sweep, no rule engine, no
//!    "unsubscribe from things you never open". He asks, once, per message.
//!  * **Nothing logs the URL or the body.** [`run::Refused`] carries a sentence
//!    from a fixed set.
//!
//! # What is not built, and what it would take
//!
//! A `List-Unsubscribe` with an `https` URL and no `List-Unsubscribe-Post` may
//! unsubscribe on `GET`, or may be a page with checkboxes and a submit button,
//! or may be a login wall. There is no way to tell them apart without fetching
//! it, and no way to act on the second without driving a browser.
//!
//! Mach opens the page instead, as a first-class keyboard-reachable action. The
//! reasoning is in the report that came with this change; the short version is
//! that a plain `GET` is *worse* than doing nothing — it is indistinguishable
//! from success, it fires whatever tracking the page carries, and a "this may
//! not have worked" toast on every one of them trains him to ignore the toast
//! that matters. A real form-filler means an embedded browser engine, a model
//! call per page, and an agent submitting a form on his behalf against a page
//! neither of them has read. That is a large, security-sensitive dependency for
//! the minority of senders who have not implemented a standard that has been
//! mandatory since 2024.

pub mod http;
pub mod rule;
pub mod run;
pub mod store;
pub mod target;

use serde::{Deserialize, Serialize};

pub use rule::{verdict, Candidate, Decline, Verdict};
pub use run::Refused;
pub use target::{Target, Unusable};

/// What the interface is allowed to know about a message's unsubscribe.
///
/// The URL is **not** here, and its absence is the design. A rendered message
/// is a document a stranger wrote; anything the frontend holds about it is
/// within reach of that document, and an unsubscribe URL identifies him to the
/// sender. So the webview learns which of three kinds the offer is, the action
/// is taken by id, and Rust looks the URL up again from the store when it is
/// needed. There is no string to steal.
///
/// Rides on [`crate::db::models::Message`], so one `get_thread` carries it and
/// opening a conversation costs no extra round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "offer", rename_all = "camelCase")]
pub enum Offer {
    /// Mach will do it. `method` is `oneClick`, `mail` or `link` — the last
    /// meaning it opens the page rather than acting.
    #[serde(rename_all = "camelCase")]
    Unsubscribe { method: String },
    /// It has an unsubscribe and using it would only confirm the address.
    /// `reason` is `notBulkMail` or `unknownSender`.
    #[serde(rename_all = "camelCase")]
    ReportSpam { reason: String },
}

impl Offer {
    /// The offer a verdict makes, or `None` when there is nothing to say.
    pub fn from_verdict(verdict: &Verdict) -> Option<Offer> {
        match verdict {
            Verdict::Unsubscribe(target) => Some(Offer::Unsubscribe {
                method: target.kind().to_string(),
            }),
            Verdict::ReportSpam(reason) => Some(Offer::ReportSpam {
                reason: match reason {
                    Decline::NotBulkMail => "notBulkMail",
                    _ => "unknownSender",
                }
                .to_string(),
            }),
            Verdict::Nothing(_) => None,
        }
    }
}
