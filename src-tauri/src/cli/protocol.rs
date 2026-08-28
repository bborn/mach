//! What the door is asked, what it answers, and — the part that matters — who
//! is allowed to ask for what.
//!
//! Everything here is a pure function over data. There is no database, no
//! dispatcher and no listener in this file, which is the point: the consent
//! rule is the one thing in the command-line interface that has to be readable
//! in full, in one place, by somebody deciding whether to trust it.
//! `tests/cli.rs` drives it directly.
//!
//! # The rule
//!
//! Every tool falls into exactly one of three classes, and the class decides
//! what the invocation had to carry:
//!
//! | class | what it is | what it needs |
//! |---|---|---|
//! | [`Class::Read`] | answers a question and changes nothing | nothing |
//! | [`Class::Mutate`] | changes the mailbox, undoably | `--yes`, or `MACH_CLI_YES=1` |
//! | [`Class::Outbound`] | puts a message on the wire addressed to a person | `--yes` **and** every recipient named |
//!
//! # Why sending is held stricter than archiving, and by this mechanism
//!
//! Archiving has an exact inverse, the command layer returns it, and ⌘Z still
//! works. Sending does not: once the outbox's ten seconds are up the message is
//! at Google and then at a stranger, and there is no command that unmakes that.
//! So the two cannot cost the same keystroke.
//!
//! The obvious mechanism — a second boolean, `--yes --send` — was rejected. A
//! flag is a word, and a word is exactly what a wrapper script, a shell-history
//! recall, or an agent's retry loop supplies for free. Within a day of anyone
//! finding `--send` tedious there is an alias with both flags in it, and the
//! second flag is then worth precisely nothing.
//!
//! What is required instead is that the invocation **names every recipient**,
//! and that the *app* — which is the only thing that can read the draft —
//! checks the claim:
//!
//! ```sh
//! mach send_draft --draftId d_812 --yes --to molly@example.com --to alex@lumen.example
//! ```
//!
//! The set must match the draft's own `to` + `cc` + `bcc` exactly: a missing
//! address refuses, and so does a surplus one, because both mean the operator
//! was wrong about what they were sending. The property this buys, and the
//! reason it is worth the typing, is that **a caller that appends `--yes` to
//! everything still cannot send mail.** It does not know the addresses. Anything
//! that does know them had to go and look, and having looked, it has committed
//! to them in the invocation, in the shell history, in whatever log is keeping
//! the command.
//!
//! There is deliberately no environment variable that authorises a send.
//! `MACH_CLI_YES=1` is a statement about a machine's standing posture towards
//! its own mailbox; who one particular message goes to is not a property of the
//! machine.
//!
//! The refusal prints the recipients it was expecting. That is not an oversight
//! and it is not a hole: anything running as this user can read the draft rows
//! out of the store with `sqlite3`, so withholding the list from the error
//! would be friction against the honest operator and no obstacle at all to the
//! caller worth worrying about. "Failure must be visible" is the older rule,
//! and a refusal that will not say what it refused is the failure mode this
//! codebase has paid the most for.
//!
//! # Where this runs
//!
//! On the app's side of the door, in [`super::door`], before
//! [`ToolGate::run`](crate::ipc::agent::engine::gate::ToolGate::run) is called.
//! The CLI classifies locally too, so it can refuse without a round trip and
//! print something useful — but that copy is *advice*. The endpoint is the
//! thing that cannot be talked out of it, and it re-derives the decision from
//! the request rather than trusting any summary of it that arrived with it.

use serde::{Deserialize, Serialize};

use crate::ipc::agent::engine::tools;

// ===========================================================================
// classes
// ===========================================================================

/// What a verb costs to authorise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Class {
    /// Changes nothing. Runs unasked.
    Read,
    /// Changes the mailbox or the calendar. Needs `--yes`.
    Mutate,
    /// Reaches a person, with no inverse. Needs `--yes` and the recipients.
    Outbound,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Read => "read",
            Class::Mutate => "mutate",
            Class::Outbound => "outbound",
        }
    }
}

/// Which class a tool is in.
///
/// Derived from [`tools::is_read_tool`] rather than restated, so a read added
/// to the agent's surface is a read here on the same day. The two additions on
/// top of it:
///
/// * `list_filters` is a read that happens to live at Google rather than in
///   SQLite. It needs the app to answer it, but it changes nothing, so it needs
///   no consent — "where the answer comes from" and "what it costs to
///   authorise" are different questions and this function answers only the
///   second.
/// * [`tools::SEND_TOOL`] is the whole of [`Class::Outbound`], and the
///   narrowness is deliberate rather than settled. `rsvp` mails the organiser,
///   `unsubscribe` reaches a stranger's server, and a calendar write with
///   guests on it sends every one of them an invitation — all three reach a
///   human and none is undoable the way the mailbox is. They are in
///   [`Class::Mutate`] today for two reasons: the recipient of each is a thing
///   the *store* knows, so requiring it to be named would make the operator
///   restate a lookup they never did; and the damage from getting one wrong is
///   a declined meeting rather than a letter that cannot be recalled.
///   Promoting one is a line in this function. It should be argued rather than
///   assumed.
pub fn classify(name: &str) -> Class {
    if tools::is_read_tool(name) || name == tools::LIST_FILTERS_TOOL {
        return Class::Read;
    }
    if name == tools::SEND_TOOL {
        return Class::Outbound;
    }
    Class::Mutate
}

/// Whether this verb can be answered with the app closed.
///
/// Exactly [`tools::READ_TOOLS`]: the local store and nothing else.
pub fn is_local(name: &str) -> bool {
    tools::is_read_tool(name)
}

// ===========================================================================
// the request
// ===========================================================================

/// What one invocation was authorised to do.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Consent {
    /// `--yes`, or `MACH_CLI_YES=1`.
    #[serde(default)]
    pub mutate: bool,
    /// Every address the operator claims this message is addressed to, from
    /// `--to`. `None` means none were named, which is not the same as an empty
    /// list — an empty list is a claim that the draft is addressed to nobody,
    /// and it is a claim that can be wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipients: Option<Vec<String>>,
}

/// One thing to ask the door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum DoorRequest {
    /// The whole surface, as the running app currently has it — the catalogue
    /// plus the reads plus the composer plus whatever plugins are installed
    /// *right now*, which is why it is worth asking for rather than computing
    /// locally.
    Tools,
    /// Run one tool.
    Call {
        tool: String,
        #[serde(default)]
        input: serde_json::Value,
        #[serde(default)]
        consent: Consent,
    },
}

// ===========================================================================
// the decision
// ===========================================================================

/// Why a call was refused, in a form the CLI can turn into an exit code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
    pub kind: String,
    pub message: String,
}

impl Refusal {
    pub fn new(kind: &str, message: impl Into<String>) -> Refusal {
        Refusal {
            kind: kind.to_string(),
            message: message.into(),
        }
    }
}

/// What the door does with one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run it. The gate still applies; this only says the consent was there.
    Run,
    /// Do not run it, and say this.
    Refuse(Refusal),
}

/// The consent rule, applied.
///
/// `recipients` is the draft's real address list, looked up by the caller
/// because only the app can do it. `None` means the draft could not be read,
/// which is a refusal in its own right: a send whose recipients are unknown
/// cannot be a send whose recipients were confirmed.
pub fn decide(tool: &str, consent: &Consent, recipients: Option<&[String]>) -> Decision {
    match classify(tool) {
        Class::Read => Decision::Run,

        Class::Mutate => match consent.mutate {
            true => Decision::Run,
            false => Decision::Refuse(Refusal::new(
                "consentRequired",
                format!(
                    "{tool} would change the mailbox, and nothing was done. \
                     Add --yes to authorise it, or set MACH_CLI_YES=1."
                ),
            )),
        },

        Class::Outbound => {
            let Some(actual) = recipients else {
                return Decision::Refuse(Refusal::new(
                    "noSuchDraft",
                    format!(
                        "{tool} names a draft this app cannot find, so there is nobody to \
                         confirm and nothing was sent."
                    ),
                ));
            };
            if !consent.mutate {
                return Decision::Refuse(Refusal::new(
                    "consentRequired",
                    format!(
                        "{tool} would send mail to {}, and nothing was sent. Sending needs \
                         --yes and every recipient named. {}",
                        list(actual),
                        naming(actual)
                    ),
                ));
            }
            let Some(claimed) = consent.recipients.as_deref() else {
                return Decision::Refuse(Refusal::new(
                    "recipientsRequired",
                    format!(
                        "{tool} would send mail to {}, and nothing was sent. --yes is not \
                         enough to send: name every recipient too, so that what leaves is \
                         something you said rather than something a flag allowed. {}",
                        list(actual),
                        naming(actual)
                    ),
                ));
            };
            match difference(claimed, actual) {
                None => Decision::Run,
                Some(complaint) => Decision::Refuse(Refusal::new(
                    "recipientsMismatch",
                    format!("{tool} was not sent. {complaint} {}", naming(actual)),
                )),
            }
        }
    }
}

/// The sentence that tells an operator what to type.
fn naming(actual: &[String]) -> String {
    let flags: Vec<String> = actual.iter().map(|a| format!("--to {a}")).collect();
    format!("Add: --yes {}", flags.join(" "))
}

fn list(addresses: &[String]) -> String {
    match addresses {
        [] => "nobody".to_string(),
        many => many.join(", "),
    }
}

/// `None` when the two sets are the same. Case and order do not matter; a
/// missing address and a surplus one both do, and both are named, because "you
/// are wrong about who this is going to" is the only useful thing to say and it
/// is true in either direction.
fn difference(claimed: &[String], actual: &[String]) -> Option<String> {
    let normalise = |v: &[String]| -> Vec<String> {
        let mut out: Vec<String> = v
            .iter()
            .map(|a| a.trim().to_ascii_lowercase())
            .filter(|a| !a.is_empty())
            .collect();
        out.sort();
        out.dedup();
        out
    };
    let claimed = normalise(claimed);
    let actual_lower = normalise(actual);

    let missing: Vec<String> = actual_lower
        .iter()
        .filter(|a| !claimed.contains(a))
        .cloned()
        .collect();
    let surplus: Vec<String> = claimed
        .iter()
        .filter(|a| !actual_lower.contains(a))
        .cloned()
        .collect();

    match (missing.as_slice(), surplus.as_slice()) {
        ([], []) => None,
        ([], surplus) => Some(format!(
            "It is not addressed to {} — that address is not on the draft.",
            list(surplus)
        )),
        (missing, []) => Some(format!(
            "It is also addressed to {}, which you did not name.",
            list(missing)
        )),
        (missing, surplus) => Some(format!(
            "It is addressed to {} — not to {}.",
            list(missing),
            list(surplus)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yes() -> Consent {
        Consent {
            mutate: true,
            recipients: None,
        }
    }

    #[test]
    fn a_read_needs_nothing() {
        assert_eq!(classify("search_threads"), Class::Read);
        assert_eq!(classify("get_thread"), Class::Read);
        // A read that lives at Google is still a read.
        assert_eq!(classify(tools::LIST_FILTERS_TOOL), Class::Read);
        assert_eq!(
            decide("search_threads", &Consent::default(), None),
            Decision::Run
        );
    }

    #[test]
    fn a_mutation_without_yes_refuses_and_says_how() {
        let Decision::Refuse(refusal) = decide("archive", &Consent::default(), None) else {
            panic!("archive must not run unasked");
        };
        assert_eq!(refusal.kind, "consentRequired");
        assert!(refusal.message.contains("--yes"), "{}", refusal.message);
        assert!(
            refusal.message.contains("nothing was done"),
            "{}",
            refusal.message
        );

        assert_eq!(decide("archive", &yes(), None), Decision::Run);
    }

    /// The whole point of the class, in one assertion: the flag that authorises
    /// every archive on the machine does not authorise one letter.
    #[test]
    fn yes_alone_never_sends() {
        let to = vec!["molly@example.com".to_string()];
        let Decision::Refuse(refusal) = decide(tools::SEND_TOOL, &yes(), Some(&to)) else {
            panic!("--yes must not be enough to send");
        };
        assert_eq!(refusal.kind, "recipientsRequired");
        assert!(
            refusal.message.contains("molly@example.com"),
            "{}",
            refusal.message
        );
    }

    #[test]
    fn naming_every_recipient_sends_and_naming_them_wrongly_does_not() {
        let actual = vec![
            "Molly@Example.com".to_string(),
            "alex@lumen.example".to_string(),
        ];
        let consent = |claimed: &[&str]| Consent {
            mutate: true,
            recipients: Some(claimed.iter().map(|s| s.to_string()).collect()),
        };

        // Order and case are not the claim; the set is.
        assert_eq!(
            decide(
                tools::SEND_TOOL,
                &consent(&["alex@lumen.example", "molly@example.com"]),
                Some(&actual)
            ),
            Decision::Run
        );

        // One recipient short: the bcc nobody looked at.
        let Decision::Refuse(short) = decide(
            tools::SEND_TOOL,
            &consent(&["molly@example.com"]),
            Some(&actual),
        ) else {
            panic!("a half-named recipient list is not consent");
        };
        assert_eq!(short.kind, "recipientsMismatch");
        assert!(
            short.message.contains("alex@lumen.example"),
            "{}",
            short.message
        );

        // One recipient too many: the operator is describing a different draft.
        let Decision::Refuse(over) = decide(
            tools::SEND_TOOL,
            &consent(&[
                "molly@example.com",
                "alex@lumen.example",
                "stranger@elsewhere.test",
            ]),
            Some(&actual),
        ) else {
            panic!("naming somebody who is not on the draft is being wrong about the draft");
        };
        assert_eq!(over.kind, "recipientsMismatch");
        assert!(
            over.message.contains("stranger@elsewhere.test"),
            "{}",
            over.message
        );
    }

    #[test]
    fn a_send_whose_draft_cannot_be_read_is_not_a_send() {
        let Decision::Refuse(refusal) = decide(tools::SEND_TOOL, &yes(), None) else {
            panic!("an unknown draft has no recipients to confirm");
        };
        assert_eq!(refusal.kind, "noSuchDraft");
    }

    /// Everything with a consequence is in a class that asks. The surface is
    /// walked rather than spot-checked, so a tool added to it cannot land in
    /// `Read` by being forgotten.
    #[test]
    fn nothing_outside_the_read_list_runs_unasked() {
        for tool in tools::tools() {
            let name = &tool.definition.name;
            let unasked = decide(name, &Consent::default(), None) == Decision::Run;
            assert_eq!(
                unasked,
                classify(name) == Class::Read,
                "{name} runs unasked without being a read"
            );
        }
    }
}
