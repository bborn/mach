//! The keyboard, while the reader's focus is inside a message body.
//!
//! # The listener that attaches and never fires
//!
//! A message body is a sandboxed iframe with no `allow-scripts`, and the
//! instant anything in it has focus — one click to follow a link, to scroll, or
//! to select a word — its keydowns fire in *that* document. Every shortcut in
//! the app dies at once: not `e` alone, but archive, star, snooze and the way
//! back to the list.
//!
//! `MessageFrame` has tried to answer this from the web side since the first
//! report ("the R shortcut isn't working consistently"): the parent attaches a
//! capture-phase `keydown` listener to the frame's document and forwards what
//! it hears into the keymap. In Blink that works, and it is why the fix was
//! believed to have landed — a keydown dispatched into the frame really does
//! archive the thread there.
//!
//! In WebKit it never runs. `JSEventListener` refuses to invoke a listener
//! whose target's context has scripting disabled, and a frame sandboxed without
//! `allow-scripts` has scripting disabled by definition. The listener attaches,
//! never fires, and nothing anywhere says so. That is the same measurement
//! `message_body::FRAME_SANDBOX` records for the *click* listener four lines
//! above it, and it has now cost three rounds of one bug report.
//!
//! So no web-side fix is possible. Nothing inside that frame can run, and
//! events in it do not cross into the parent. The key has to be read below the
//! engine, which is what this does.
//!
//! # Why not simply move the focus out
//!
//! Because the reader would lose text selection, and selecting text in a
//! message is not negotiable. A drag-select *is* focus in the frame: blur it
//! and WebKit drops the selection, and ⌘C — which the frame handles natively
//! and correctly today — has nothing to copy. Every variant of "hand focus back
//! once the interaction settles" has to guess when a drag began, and the parent
//! cannot see a pointer inside the frame at all.
//!
//! Reading the key changes nothing about focus, so selection is untouched.
//!
//! # A local monitor, and what it must not do
//!
//! `+[NSEvent addLocalMonitorForEventsMatchingMask:handler:]` inserts a block
//! into this application's own event stream, ahead of `-[NSApplication
//! sendEvent:]`. It sees every key the app receives — the composer's typing,
//! the search field's, everything.
//!
//! So the same rule [`crate::scroll`] works under holds here, and for a larger
//! reason: **the handler returns the event unchanged.** Returning null swallows
//! it, and a swallowed key is a letter that never reaches the message a person
//! was typing. Nothing here consumes anything. The key is published *and*
//! delivered, and the frontend decides what it was worth.
//!
//! That is safe because of where it is allowed to publish from. A key acted on
//! here is one whose focus is inside a scripting-disabled, non-editable
//! document, where the letter it also gets delivered to does nothing at all.
//!
//! # It publishes a fact and decides nothing
//!
//! It does not know which view is on screen, which element has focus, or what
//! any key means. It cannot: an event monitor that decided would be a second
//! keymap to keep in step with the real one.
//!
//! What gates it is a single flag the frontend owns — [`set_frame_focus`] —
//! which says whether the keyboard is currently inside a message frame. That
//! answer is `document.activeElement`, which only the web side can read, and it
//! is also what keeps this quiet: with the flag down the handler returns
//! immediately and there is no IPC at all. A person typing a mail pays nothing
//! for this; only a person whose focus is in a message body does, and their
//! keys are the ones currently being lost.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

/// The event the frontend listens on. Mirrors `lib/frame-keyboard.ts`.
pub const FRAME_KEY_EVENT: &str = "frame-key";

/// Whether the keyboard is inside a message frame, as the frontend last said.
///
/// Down by default, so a build where the frontend never reports costs nothing
/// and behaves exactly as it does today.
static IN_FRAME: AtomicBool = AtomicBool::new(false);

/// One key, in the shape `lib/keymap.ts` matches on.
///
/// `key` is the character the layout produced with the modifiers *ignored* —
/// the same value a DOM `KeyboardEvent.key` carries, which is what
/// `tokenFromEvent` reads. `code` rides along for the one case that needs the
/// physical key: on macOS Option remaps the number row to `¡™£¢∞§¶•ª`, so
/// `alt+1` is only recognisable as `Digit1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameKey {
    pub key: String,
    pub code: String,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub alt_key: bool,
    pub shift_key: bool,
}

/// The frontend reporting where the keyboard is.
///
/// Called on every focus change that crosses the boundary, which is rare — a
/// click into a message and a click back out — rather than per keystroke.
#[tauri::command]
pub fn set_frame_focus(inside: bool) {
    IN_FRAME.store(inside, Ordering::Relaxed);
}

/// What the flag says right now. For tests and for the monitor.
pub fn keyboard_in_frame() -> bool {
    IN_FRAME.load(Ordering::Relaxed)
}

/// The virtual keycodes worth naming, and the `KeyboardEvent.key` for each.
///
/// Only the ones a binding can be written against. Everything else arrives as a
/// character through `charactersIgnoringModifiers`, which is already the right
/// answer for a letter, a digit and `#`.
///
/// Escape is the one that matters most here: it is how a reader gets out of the
/// message, and a `charactersIgnoringModifiers` of `\u{1b}` is not a token the
/// keymap knows.
pub fn named_key(key_code: u16) -> Option<&'static str> {
    Some(match key_code {
        53 => "Escape",
        36 => "Enter",
        76 => "Enter", // the numeric keypad's
        48 => "Tab",
        51 => "Backspace",
        117 => "Delete",
        _ => return None,
    })
}

/// The `KeyboardEvent.code` for the digit row, so `alt+<n>` survives Option.
///
/// Nothing else needs a code: `tokenFromEvent` reads `code` only when Alt is
/// held and the physical key is a digit.
pub fn digit_code(key_code: u16) -> Option<&'static str> {
    Some(match key_code {
        29 => "Digit0",
        18 => "Digit1",
        19 => "Digit2",
        20 => "Digit3",
        21 => "Digit4",
        23 => "Digit5",
        22 => "Digit6",
        26 => "Digit7",
        28 => "Digit8",
        25 => "Digit9",
        _ => return None,
    })
}

/// Build the payload, given what AppKit reports.
///
/// Split out from the block so the mapping — which is the only part with a
/// judgement in it — can be tested without an `NSEvent` or a run loop.
///
/// Returns `None` for a key with nothing to say: a bare modifier press, or a
/// character the layout did not produce. Publishing those would be noise the
/// frontend has to filter, and the keymap already refuses them.
pub fn frame_key(
    key_code: u16,
    characters: &str,
    meta: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
) -> Option<FrameKey> {
    let key = match named_key(key_code) {
        Some(named) => named.to_string(),
        // One character, not a string: a dead key or an IME composition can
        // report several, and none of them is a shortcut.
        None => {
            let mut chars = characters.chars();
            let first = chars.next()?;
            if chars.next().is_some() || first.is_control() {
                return None;
            }
            first.to_string()
        }
    };
    Some(FrameKey {
        key,
        code: digit_code(key_code).unwrap_or("").to_string(),
        meta_key: meta,
        ctrl_key: ctrl,
        alt_key: alt,
        shift_key: shift,
    })
}

/// `NSEventModifierFlagShift/Control/Option/Command`, named rather than
/// imported so a rename in AppKit cannot quietly agree with itself. The same
/// four `browser::is_bare_escape` spells out, for the same reason.
pub const SHIFT: u64 = 1 << 17;
pub const CONTROL: u64 = 1 << 18;
pub const OPTION: u64 = 1 << 19;
pub const COMMAND: u64 = 1 << 20;

/// One modifier out of the raw bit field, which also carries CapsLock, Fn and
/// NumericPad — none of which a binding is written against.
#[must_use]
pub fn held(modifiers: u64, flag: u64) -> bool {
    modifiers & flag != 0
}

/// Start watching. Call from `setup`, on the main thread.
///
/// Failure is not fatal and not reported to the user: without it the keyboard
/// behaves exactly as it does today, which is to say the shortcuts stay dead
/// while focus is in a message. It is logged, because a silent fallback nobody
/// can see is precisely how this area came to have three failed fixes in it.
#[cfg(target_os = "macos")]
pub fn install<R: Runtime>(app: &AppHandle<R>) {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSEvent, NSEventMask};
    use std::ptr::NonNull;

    let app = app.clone();

    let handler = block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // The cheap question first, and the reason a person typing a message
        // pays nothing for any of this.
        if !keyboard_in_frame() {
            return event.as_ptr();
        }

        // SAFETY: AppKit hands the block a live event for the duration of the
        // call. Nothing here retains it past the return.
        let ev: &NSEvent = unsafe { event.as_ref() };
        let modifiers = ev.modifierFlags().0 as u64;
        let characters = ev
            .charactersIgnoringModifiers()
            .map(|s| s.to_string())
            .unwrap_or_default();

        if let Some(payload) = frame_key(
            ev.keyCode(),
            &characters,
            held(modifiers, COMMAND),
            held(modifiers, CONTROL),
            held(modifiers, OPTION),
            held(modifiers, SHIFT),
        ) {
            // A failed emit means the window is gone; there is nobody to tell.
            let _ = app.emit(FRAME_KEY_EVENT, payload);
        }

        // Never swallowed. See the module docs: this publishes, it does not
        // consume, and the key still reaches the document underneath.
        event.as_ptr()
    });

    // SAFETY: the mask is a documented constant and the block matches the
    // signature AppKit calls it with.
    let token: Option<Retained<AnyObject>> = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler)
    };

    match token {
        // The monitor lives as long as the process. Dropping the token removes
        // it, so it is leaked on purpose rather than parked in a static that
        // would need a lock it never uses.
        Some(token) => std::mem::forget(token),
        None => eprintln!(
            "frame_keyboard: could not install the key monitor; shortcuts will stay \
             dead while focus is inside a message body"
        ),
    }
}

/// Nothing to watch anywhere else. Mach is macOS-only, and this keeps the
/// `setup` call free of a `cfg`.
#[cfg(not(target_os = "macos"))]
pub fn install<R: Runtime>(_app: &AppHandle<R>) {}

#[cfg(test)]
mod tests {
    use super::*;

    // The flags, spelled out again rather than imported, so a rename in AppKit
    // cannot quietly agree with itself. Same discipline as `scroll`.
    const NS_SHIFT: u64 = 1 << 17;
    const NS_COMMAND: u64 = 1 << 20;
    const NS_CAPS_LOCK: u64 = 1 << 16;

    fn plain(key_code: u16, characters: &str) -> Option<FrameKey> {
        frame_key(key_code, characters, false, false, false, false)
    }

    #[test]
    fn a_letter_is_the_character_the_layout_produced() {
        let key = plain(14, "e").unwrap();
        assert_eq!(key.key, "e");
        assert!(!key.meta_key && !key.ctrl_key && !key.alt_key && !key.shift_key);
    }

    /// The key this whole module exists for. `charactersIgnoringModifiers`
    /// reports `\u{1b}` for it, which is not a token the keymap has ever heard
    /// of — so it is named off the key code instead.
    #[test]
    fn escape_is_named_rather_than_left_as_a_control_character() {
        assert_eq!(plain(53, "\u{1b}").unwrap().key, "Escape");
    }

    #[test]
    fn a_bare_control_character_says_nothing() {
        // No name for this code, and nothing printable came out of it.
        assert_eq!(plain(63, "\u{10}"), None);
    }

    /// A dead key or an IME composition can report several characters, and none
    /// of them is a shortcut.
    #[test]
    fn a_composition_says_nothing() {
        assert_eq!(plain(14, "ñe"), None);
        assert_eq!(plain(14, ""), None);
    }

    /// `tokenFromEvent` prefers the physical digit when Alt is held, because
    /// macOS remaps the number row to `¡™£¢∞§¶•ª` and `alt+1` came out as
    /// `alt+¡`. That is the only thing `code` is for.
    #[test]
    fn the_digit_row_carries_its_physical_code() {
        assert_eq!(frame_key(18, "¡", false, false, true, false).unwrap().code, "Digit1");
        // Nothing else needs one, and inventing one would be a second source of
        // truth for what a key is.
        assert_eq!(plain(14, "e").unwrap().code, "");
    }

    #[test]
    fn the_modifiers_come_across() {
        let key = frame_key(0, "a", true, false, false, true).unwrap();
        assert!(key.meta_key && key.shift_key);
        assert!(!key.ctrl_key && !key.alt_key);
    }

    #[test]
    fn held_reads_one_flag_and_ignores_the_rest() {
        assert!(held(NS_COMMAND | NS_CAPS_LOCK, NS_COMMAND));
        assert!(!held(NS_CAPS_LOCK, NS_COMMAND));
        // CapsLock is not Shift, however much a key looks the same with it on.
        assert!(!held(NS_CAPS_LOCK, NS_SHIFT));
    }

    /// The flag is what keeps this quiet: down, and the handler returns on one
    /// atomic read with no IPC at all. It starts down, so a build whose
    /// frontend never reports behaves exactly as it does today.
    #[test]
    fn the_flag_starts_down_and_follows_what_the_frontend_says() {
        assert!(!keyboard_in_frame());
        set_frame_focus(true);
        assert!(keyboard_in_frame());
        set_frame_focus(false);
        assert!(!keyboard_in_frame());
    }
}
