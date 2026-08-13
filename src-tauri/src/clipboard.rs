//! Putting text on the system pasteboard.
//!
//! # Why this is not `navigator.clipboard.writeText`
//!
//! That was the first implementation, and it is the right answer for a *button*
//! — the event panel's "copy link" still uses it. It is the wrong answer for a
//! keystroke, because WebKit gates the async clipboard API twice over: the
//! document must be frontmost, and the write must be inside a user activation.
//! Both hold when a hand presses the key on a focused window. Neither holds
//! when the window is behind something, and neither holds for a synthetic
//! event, which is what `scripts/qa key` dispatches.
//!
//! That second one is what settled it. A QA instance runs under an Accessory
//! activation policy and structurally cannot be the frontmost application — so
//! a webview-side copy is a feature that cannot be looked at, in a codebase
//! whose standing rule is to look at the work in the real app. It failed
//! exactly that way the first time it was driven, and the toast it produced
//! said "The clipboard refused the copy", which is a true sentence about a
//! problem the owner does not have and cannot act on.
//!
//! # Why `pbcopy` and not a plugin
//!
//! `tauri-plugin-clipboard-manager` would do this. It is a dependency, a
//! permission entry and an IPC surface to reach a pasteboard that a five-line
//! subprocess already reaches, in an app that only ships for macOS — and one
//! unused plugin was removed from this repo the week this was written.
//! `ipc::feedback` already shells out to `/usr/bin/screencapture` on the same
//! reasoning, and this is smaller: no arguments at all, the text goes down
//! stdin, and no shell is involved to have an opinion about what is in it.

use std::io::Write;
use std::process::{Command, Stdio};

const PBCOPY: &str = "/usr/bin/pbcopy";

/// Replace the pasteboard's contents with `text`.
///
/// The text is a document the user is looking at, so it goes on stdin and never
/// near a command line: there is no shell, no argument, and nothing to quote.
#[cfg(target_os = "macos")]
pub fn write(text: &str) -> Result<(), String> {
    let mut child = Command::new(PBCOPY)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run {PBCOPY}: {e}"))?;

    child
        .stdin
        .as_mut()
        .ok_or_else(|| format!("{PBCOPY} took no input"))?
        .write_all(text.as_bytes())
        .map_err(|e| format!("could not write to {PBCOPY}: {e}"))?;

    // Dropping the handle closes stdin, which is what tells pbcopy the document
    // has ended; without it the wait below never returns.
    drop(child.stdin.take());

    let status = child
        .wait()
        .map_err(|e| format!("{PBCOPY} did not finish: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{PBCOPY} exited with {status}"))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn write(_text: &str) -> Result<(), String> {
    Err("copying to the clipboard is implemented for macOS only".into())
}
