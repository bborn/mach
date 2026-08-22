//! Whether the fingers are still on the trackpad.
//!
//! # The signal the web engine throws away
//!
//! A two-finger swipe is one movement of a hand and about a hundred and fifty
//! events. macOS emits them at roughly 60Hz while the fingers move, and then
//! keeps emitting for another second or so after the fingers have left, with
//! the deltas decaying towards zero. Both halves reach JavaScript as ordinary
//! `wheel` events and nothing on the event distinguishes them.
//!
//! The operating system knows perfectly well which is which. Every scroll
//! `NSEvent` carries two fields:
//!
//! | field | while the fingers are down | during the coast |
//! |---|---|---|
//! | `phase` | `Began`, then `Changed`…, then `Ended` | `None` |
//! | `momentumPhase` | `None` | `Began`, then `Changed`…, then `Ended` |
//!
//! WebKit reads both — it needs them for rubber-banding — and forwards neither.
//! There is no `WheelEvent` property for phase in any shipping engine, and no
//! amount of squinting at deltas and timestamps recovers it. Three separate
//! attempts to infer "the fingers have left" from the shape of the DOM stream
//! failed here, all the same way: a real finger's magnitudes swing by a factor
//! of ten between neighbouring events (`41, 10, 24, 5, 31, 5, 25, 3, 20` in a
//! captured trace), so every rule that says "wound down and then climbed again
//! means a second swipe" fires while the hand is still on the glass.
//!
//! So this reads the field instead of guessing at it.
//!
//! # A local monitor, and what it must not do
//!
//! `+[NSEvent addLocalMonitorForEventsMatchingMask:handler:]` inserts a block
//! into this application's own event stream, ahead of `-[NSApplication
//! sendEvent:]`. It sees every scroll event the app receives, which means every
//! scrollable surface in the window and not only the calendar.
//!
//! That makes one rule absolute: **the handler returns the event unchanged.**
//! Returning null swallows it, and a swallowed scroll event is the mail list,
//! the reading pane, the hour grid and the preferences sheet all refusing to
//! scroll. This module reads two integers off the event and hands it straight
//! back.
//!
//! It also decides nothing. It does not know which view is on screen, where the
//! pointer is, or whether a drag is in flight, and it must not: an event
//! monitor that steered the calendar would turn a scroll of the message list
//! into a week change. All it publishes is a fact about the hardware — "the
//! fingers are down" or "they are not" — and the frontend, which already knows
//! which element the pointer is over because the `wheel` event tells it,
//! decides what that fact is worth. A period moves only when a DOM wheel event
//! lands on the calendar grid, exactly as before.
//!
//! # Only transitions cross the bridge
//!
//! A gesture is ~150 events and at most a handful of phase changes, so emitting
//! per event would be 150 IPC messages to say the same two things. The handler
//! keeps the last state it published and emits only when it changes:
//! `fingers-down` when a hand lands, `fingers-up` when it leaves. Two or three
//! messages per swipe.
//!
//! Each carries a `gesture` counter that increments every time the fingers come
//! back down. That number is what lets the frontend tell "the tail of the swipe
//! I already acted on" from "a new swipe that happens to look identical", which
//! is the distinction the whole thing exists for — and it is a counter rather
//! than a timestamp because the two sides have no shared clock.
//!
//! # Ordering, which is not obvious
//!
//! The monitor block runs on the main thread, before `sendEvent:` dispatches
//! the event to the `WKWebView`. The DOM `wheel` event is produced later still,
//! in the web content process, and arrives back on its own schedule. So for any
//! one `NSEvent` the native reading is available *at or before* the DOM event
//! derived from it — but the emit itself has to cross into the webview, so the
//! frontend's view of the phase can lag the wheel events by a frame or two.
//!
//! Both directions of that lag are survivable, and neither is a coin flip:
//!
//! * **`fingers-up` late** — the first event or two of the momentum tail are
//!   still counted as finger travel. They are in the same direction as the
//!   swipe that just fired, so at worst a step fires a frame early.
//! * **`fingers-up` early** — the last finger event or two are discarded as
//!   tail. A step fires as soon as the travel is unambiguous, tens of events
//!   before the fingers lift, so there is nothing left to lose by then.
//! * **`fingers-down` late** — the first event or two of a *new* swipe are
//!   discarded as the old swipe's tail. The rest of the swipe still fires it.
//!
//! What is *not* survivable is inferring any of this, which is what the three
//! previous attempts did.
//!
//! # Mouse wheels have no phase at all
//!
//! A scroll wheel reports `phase == None` and `momentumPhase == None` — not
//! "fingers up", but "this device has no fingers". Publishing that as
//! `fingers-up` would be a lie that costs nothing here (the frontend routes
//! wheels down a separate path keyed on delta shape) and a trap later, so it is
//! published as its own state and the frontend leaves the wheel path alone.

// The monitor is the only reader of these three, and the monitor only exists on
// macOS — see `install`. Ungated they are three warnings on every other build.
#[cfg(target_os = "macos")]
use std::cell::Cell;
#[cfg(target_os = "macos")]
use std::rc::Rc;

use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use tauri::Emitter;
use tauri::{AppHandle, Runtime};

/// The event the frontend listens for. See `src/lib/scroll-phase.ts`.
pub const SCROLL_PHASE_EVENT: &str = "scroll-phase";

/// What the hardware is doing, as the frontend reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Fingers on the glass. Travel in this state is the hand's own.
    FingersDown,
    /// The coast after a lift, or the quiet after a gesture. Not the hand.
    FingersUp,
    /// A device that reports no phase — a scroll wheel. Not a statement about
    /// fingers, and the frontend must not read it as one.
    NoPhase,
}

/// One phase change, as it crosses to the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollPhasePayload {
    pub phase: Phase,
    /// Increments each time the fingers come back down. The frontend's only
    /// way to tell one gesture from the next.
    pub gesture: u64,
}

/// Which of the three states a scroll event is reporting.
///
/// Split out from the monitor so the mapping — the one piece of this with any
/// judgement in it — can be tested without an `NSEvent` or a run loop.
///
/// `MayBegin` is the touch that precedes movement, and it counts as fingers
/// down: it carries no delta, but a gesture that started there and only got
/// `Changed` afterwards would otherwise never be seen to begin.
#[must_use]
pub fn classify(phase: u64, momentum: u64) -> Phase {
    const NONE: u64 = 0;
    const BEGAN: u64 = 1 << 0;
    const STATIONARY: u64 = 1 << 1;
    const CHANGED: u64 = 1 << 2;
    const MAY_BEGIN: u64 = 1 << 5;

    // Momentum is checked first and unconditionally. During the coast `phase`
    // is `None`, so the order only matters for a malformed event, and taking
    // momentum's word for it is the conservative reading: a step not fired is
    // a lost swipe, a step fired off a momentum event is the original bug.
    if momentum != NONE {
        return Phase::FingersUp;
    }
    if phase & (BEGAN | MAY_BEGIN | CHANGED | STATIONARY) != 0 {
        return Phase::FingersDown;
    }
    // `Ended`, `Cancelled` — and `None`, which with no momentum either is a
    // device that does not report phase at all.
    if phase == NONE {
        Phase::NoPhase
    } else {
        Phase::FingersUp
    }
}

/// The state the monitor carries between events: what it last published.
///
/// Separate from the block so the transition rule is testable. Returns the
/// payload to emit, or `None` when nothing has changed and the event is one of
/// the hundred-odd that say the same thing as the last one.
#[must_use]
pub fn transition(last: Option<Phase>, gesture: u64, next: Phase) -> Option<ScrollPhasePayload> {
    if last == Some(next) {
        return None;
    }
    // A new gesture is fingers arriving, and only that. A lift does not start
    // one, and neither does a mouse wheel interrupting a coast.
    let gesture = if next == Phase::FingersDown && last != Some(Phase::FingersDown) {
        gesture + 1
    } else {
        gesture
    };
    Some(ScrollPhasePayload {
        phase: next,
        gesture,
    })
}

/// Start watching. Call from `setup`, on the main thread.
///
/// Failure is not fatal and not reported to the user: without this the frontend
/// falls back to reading the wheel stream alone, which is worse but not broken.
/// It is logged, because a silent fallback that nobody can see is how this area
/// came to have three failed fixes in it.
#[cfg(target_os = "macos")]
pub fn install<R: Runtime>(app: &AppHandle<R>) {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSEvent, NSEventMask};
    use std::ptr::NonNull;

    let app = app.clone();
    // `Rc<Cell<_>>` and not a lock: the block runs on the main thread and only
    // there, because that is where `sendEvent:` runs.
    let last: Rc<Cell<Option<Phase>>> = Rc::new(Cell::new(None));
    let gesture: Rc<Cell<u64>> = Rc::new(Cell::new(0));

    let handler = block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands the block a live event for the duration of the
        // call. Nothing here retains it past the return.
        let ev: &NSEvent = unsafe { event.as_ref() };

        // Read the two fields, decide, publish if it changed — and then, on
        // every path including the ones that return early, give the event back.
        // A `?` here would swallow it.
        let next = classify(ev.phase().0 as u64, ev.momentumPhase().0 as u64);
        if let Some(payload) = transition(last.get(), gesture.get(), next) {
            last.set(Some(payload.phase));
            gesture.set(payload.gesture);
            // A failed emit means the window is gone; there is nobody to tell.
            let _ = app.emit(SCROLL_PHASE_EVENT, payload);
        }

        // The whole application's scrolling depends on this line.
        event.as_ptr()
    });

    // SAFETY: the mask is a documented constant and the block matches the
    // signature AppKit calls it with.
    let token: Option<Retained<AnyObject>> = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::ScrollWheel, &handler)
    };

    match token {
        // The monitor lives as long as the process. Dropping the token removes
        // it, so it is leaked on purpose rather than parked in a static that
        // would need a lock it never uses.
        Some(token) => std::mem::forget(token),
        None => eprintln!(
            "scroll: could not install the scroll-phase monitor; \
             trackpad period navigation will fall back to reading deltas alone"
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

    // The constants, spelled out rather than imported, so a rename in AppKit
    // cannot quietly agree with itself.
    const NONE: u64 = 0;
    const BEGAN: u64 = 1;
    const STATIONARY: u64 = 2;
    const CHANGED: u64 = 4;
    const ENDED: u64 = 8;
    const CANCELLED: u64 = 16;
    const MAY_BEGIN: u64 = 32;

    #[test]
    fn fingers_on_the_glass_are_fingers_down() {
        for phase in [BEGAN, CHANGED, STATIONARY, MAY_BEGIN] {
            assert_eq!(classify(phase, NONE), Phase::FingersDown, "phase {phase}");
        }
    }

    #[test]
    fn a_lift_is_fingers_up() {
        assert_eq!(classify(ENDED, NONE), Phase::FingersUp);
        assert_eq!(classify(CANCELLED, NONE), Phase::FingersUp);
    }

    #[test]
    fn every_momentum_event_is_fingers_up() {
        for momentum in [BEGAN, CHANGED, ENDED] {
            assert_eq!(classify(NONE, momentum), Phase::FingersUp);
        }
    }

    #[test]
    fn momentum_outranks_a_stale_phase() {
        // Not a stream macOS produces, but the reading has to be the safe one.
        assert_eq!(classify(CHANGED, CHANGED), Phase::FingersUp);
    }

    #[test]
    fn a_wheel_reports_no_phase_at_all() {
        assert_eq!(classify(NONE, NONE), Phase::NoPhase);
    }

    #[test]
    fn a_repeat_of_the_last_state_says_nothing() {
        assert_eq!(transition(Some(Phase::FingersDown), 4, Phase::FingersDown), None);
        assert_eq!(transition(Some(Phase::FingersUp), 4, Phase::FingersUp), None);
    }

    #[test]
    fn fingers_landing_start_a_new_gesture() {
        let out = transition(Some(Phase::FingersUp), 4, Phase::FingersDown).unwrap();
        assert_eq!(out.phase, Phase::FingersDown);
        assert_eq!(out.gesture, 5);
    }

    #[test]
    fn a_lift_keeps_the_gesture_it_ends() {
        let out = transition(Some(Phase::FingersDown), 5, Phase::FingersUp).unwrap();
        assert_eq!(out.phase, Phase::FingersUp);
        assert_eq!(out.gesture, 5);
    }

    #[test]
    fn one_swipe_is_one_new_gesture_however_many_events_it_has() {
        // The shape of a real stream: a touch, movement, a lift, a long coast.
        let stream: Vec<(u64, u64)> = std::iter::once((MAY_BEGIN, NONE))
            .chain(std::iter::repeat_n((CHANGED, NONE), 40))
            .chain(std::iter::once((ENDED, NONE)))
            .chain(std::iter::repeat_n((NONE, CHANGED), 100))
            .collect();

        let mut last = None;
        let mut gesture = 0;
        let mut emitted = Vec::new();
        for (phase, momentum) in stream {
            if let Some(out) = transition(last, gesture, classify(phase, momentum)) {
                last = Some(out.phase);
                gesture = out.gesture;
                emitted.push(out);
            }
        }

        assert_eq!(
            emitted,
            vec![
                ScrollPhasePayload { phase: Phase::FingersDown, gesture: 1 },
                ScrollPhasePayload { phase: Phase::FingersUp, gesture: 1 },
            ],
            "142 events should cross the bridge as two messages"
        );
    }

    #[test]
    fn a_second_swipe_over_the_first_ones_coast_gets_its_own_number() {
        // Fingers land again mid-coast, which is the case every failed fix
        // could not see. macOS cancels the momentum and the phase comes back.
        let stream = [
            (NONE, CHANGED),
            (NONE, CHANGED),
            (BEGAN, NONE),
            (CHANGED, NONE),
            (ENDED, NONE),
            (NONE, CHANGED),
        ];

        let mut last = Some(Phase::FingersUp);
        let mut gesture = 1;
        let mut numbers = Vec::new();
        for (phase, momentum) in stream {
            if let Some(out) = transition(last, gesture, classify(phase, momentum)) {
                last = Some(out.phase);
                gesture = out.gesture;
                numbers.push((out.phase, out.gesture));
            }
        }

        assert_eq!(
            numbers,
            vec![(Phase::FingersDown, 2), (Phase::FingersUp, 2)],
            "the second swipe must not share the first one's number"
        );
    }
}
