//! One error shape, and the exit codes it turns into.
//!
//! Same two-field contract as everything else that crosses a boundary in this
//! codebase — `{ kind, message }` — because the caller here is very often not a
//! person. An agent driving `mach` has to tell "the app is not running" from
//! "Gmail refused this" from "there is no thread 4127", and all three arrive as
//! a non-zero exit and a line on stderr unless the kinds are kept distinct and
//! the exit codes are kept meaningful.
//!
//! # The codes
//!
//! | code | when |
//! |---|---|
//! | 0 | it worked |
//! | 1 | the action was refused: bad arguments, no such thread, Google said no |
//! | 2 | Mach is not running, and this verb needs it |
//! | 3 | no such verb, or the command line could not be parsed |
//! | 4 | consent was missing — `--yes`, or the recipients |
//! | 5 | the local store could not be opened or read |
//! | 6 | the door was reached and would not talk: stale token, bad answer |
//!
//! 2 and 4 are the two an automated caller acts on rather than reports: 2 means
//! start the app and try again, 4 means the invocation was under-authorised and
//! a human has to decide. Everything else is a fact about the request.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliError {
    pub kind: String,
    pub message: String,
}

impl CliError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> CliError {
        CliError {
            kind: kind.into(),
            message: message.into(),
        }
    }

    /// The process exit code for this error.
    ///
    /// Unrecognised kinds — anything the app grew that this binary predates —
    /// land on 1, "refused". That is the safe default: it says the action did
    /// not happen without claiming to know why, and it is never mistaken for
    /// success.
    pub fn exit_code(&self) -> i32 {
        match self.kind.as_str() {
            "notRunning" => 2,
            "unknownVerb" | "usage" => 3,
            "consentRequired" | "recipientsRequired" | "recipientsMismatch" => 4,
            "store" => 5,
            "door" | "notReady" => 6,
            _ => 1,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
