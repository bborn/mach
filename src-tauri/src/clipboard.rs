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
//! subprocess already reaches — and one unused plugin was removed from this
//! repo the week this was written. `ipc::feedback` already shells out to
//! `/usr/bin/screencapture` on the same reasoning, and this is smaller: no
//! arguments at all, the text goes down stdin, and no shell is involved to
//! have an opinion about what is in it.
//!
//! # Why `wl-copy` on Linux
//!
//! The same subprocess shape, because the argument above is about the shape
//! and not about macOS. `wl-copy` is the Wayland twin of `pbcopy`: no
//! arguments, text on stdin, and it is what every other clipboard crate would
//! end up talking to anyway — Wayland has no clipboard daemon, so whoever put
//! the text there has to stay alive and hand it out. wl-copy does that by
//! forking; see `write` below for why the `wait` still returns.
//!
//! X11 is not covered. The equivalent there is `xclip` or `xsel`, a third
//! binary to detect and a third failure message to write, for a session type
//! nobody has run this on.

use std::io::Write;
use std::process::{Command, Stdio};

#[cfg(target_os = "macos")]
const PBCOPY: &str = "/usr/bin/pbcopy";

// Not an absolute path, unlike `pbcopy`. `wl-copy` ships in a distribution
// package rather than in the base system, so its directory is a packaging
// decision — /usr/bin on Arch, /usr/local/bin elsewhere — and PATH is the
// thing that actually knows.
#[cfg(target_os = "linux")]
const WL_COPY: &str = "wl-copy";

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

/// Replace the Wayland selection's contents with `text`.
///
/// Same shape as the macOS arm above, and the same reason for it: the text is
/// a document the user is looking at, so it goes on stdin and never near a
/// command line.
#[cfg(target_os = "linux")]
pub fn write(text: &str) -> Result<(), String> {
    let mut child = Command::new(WL_COPY)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run {WL_COPY}: {e}"))?;

    child
        .stdin
        .as_mut()
        .ok_or_else(|| format!("{WL_COPY} took no input"))?
        .write_all(text.as_bytes())
        .map_err(|e| format!("could not write to {WL_COPY}: {e}"))?;

    // Dropping the handle closes stdin, which is what tells wl-copy the
    // document has ended; without it the wait below never returns.
    drop(child.stdin.take());

    // This waits for the *parent* wl-copy, which reads stdin to the end, forks
    // a copy of itself to sit on the selection, and exits. The fork is the one
    // holding the text — a Wayland selection is served by a live client, not
    // stored by the compositor — and it is reparented to init and outlives us
    // on purpose. So a success here means the text was handed over, not that
    // the clipboard has been given up.
    let status = child
        .wait()
        .map_err(|e| format!("{WL_COPY} did not finish: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{WL_COPY} exited with {status}"))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn write(_text: &str) -> Result<(), String> {
    Err("copying to the clipboard is implemented for macOS and Linux only".into())
}
