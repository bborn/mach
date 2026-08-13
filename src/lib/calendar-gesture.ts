/**
 * Turning wheel events into "move one period".
 *
 * A two-finger swipe on a trackpad is not one event. macOS emits a stream at
 * roughly 60Hz while the fingers move, and then keeps emitting for another
 * second or so after the fingers have left, with the deltas decaying towards
 * zero. A rule like "deltaX over 40 means next week" turns one flick into
 * fifteen weeks, because the tail is forty more events that each clear 40.
 *
 * # What used to be here, and why it is gone
 *
 * Three fixes tried to tell the finger half from the tail by the *shape* of the
 * DOM stream — how fast it was going, whether it had wound down and climbed
 * again, whether there was a hole in it. All three failed, and a captured trace
 * says why: a real finger's magnitudes read `41, 10, 24, 5, 31, 5, 25, 3, 20`,
 * a factor of ten between neighbouring events, because a hand is not a smooth
 * ramp and WebKit's sampling is not even. Every "wound down then rose again"
 * rule fires repeatedly while the hand is still on the glass. The last of them
 * papered over that by refusing to fire twice until the stream went quiet, and
 * a tail runs about 1.2 seconds — so a second swipe did nothing until well
 * after you had made it.
 *
 * macOS knows the answer exactly. Every scroll `NSEvent` carries `phase` and
 * `momentumPhase`; WebKit reads both and forwards neither. So Rust reads them
 * off the event and publishes the one bit that matters — see
 * `src-tauri/src/scroll.rs` and `scroll-phase.ts` — and this file stops
 * guessing:
 *
 *  1. **Travel counts only while the fingers are down.** Everything after the
 *     lift is discarded outright, however large it is and however long it runs.
 *
 *  2. **A period moves once per gesture,** as soon as the travel along the
 *     committed axis is unambiguous — which is two or three events in, not at
 *     the end. Nothing later in the same gesture can fire again.
 *
 *  3. **Fingers landing start a new gesture,** by the counter Rust increments,
 *     not by a clock. A second swipe a frame after the first is a second
 *     gesture; a tail an hour long is not.
 *
 * Rule 2's axis lock survives from before and does real work: it is what stops
 * the sideways drift that rides along with every real vertical scroll from
 * paging the view. Once the accumulated travel is unambiguous the gesture is
 * horizontal or vertical for the rest of its life. macOS locks axes for the
 * same reason.
 *
 * # When there is no phase
 *
 * A mouse wheel has none — it is a different instrument, with no horizontal
 * axis, no momentum and a coarse discrete step, and it goes down its own path
 * below (`notch`) exactly as it always has.
 *
 * A trackpad with no phase means there is no Rust: a browser, the fixture
 * harness, this file's own tests. Then the fallback is the plain one — a
 * gesture is a stream of events less than `IDLE_MS` apart, and it moves one
 * period however long it runs. It cannot tell a tail from a second swipe
 * without waiting for silence, which is the whole limitation the native signal
 * exists to remove, and no path in the shipping app takes it.
 *
 * Everything here is pure — samples in, a step out — which is the only way to
 * test this at all, since the thing being tested is a sequence of a hundred and
 * fifty events with particular magnitudes, timings and phases.
 */

/**
 * A mouse wheel and a trackpad are different instruments that arrive through
 * the same event. A wheel has no horizontal axis, no momentum, and a coarse
 * discrete step; a trackpad has all three and a fine one.
 */
export type WheelDevice = "trackpad" | "wheel";

/**
 * The native reading of the trackpad at the moment the event was handled.
 * `null` when nothing is publishing one — see the header.
 */
export interface FingerPhase {
  /** Fingers on the glass. False during the coast after a lift. */
  down: boolean;
  /** Increments when a hand lands. The only marker of a gesture boundary. */
  gesture: number;
}

/** The parts of a `WheelEvent` the gesture logic reads. */
export interface WheelSample {
  deltaX: number;
  deltaY: number;
  /** `WheelEvent.DOM_DELTA_*`: 0 pixels, 1 lines, 2 pages. */
  deltaMode: number;
  /** Milliseconds on any monotonic clock. `event.timeStamp` is one. */
  timeStamp: number;
  /**
   * A pinch on the trackpad reaches the page as a wheel event with `ctrlKey`
   * set and no key held. It is a zoom, and it must not move the calendar.
   */
  ctrlKey?: boolean;
  /** From `readFingers()`, sampled by the caller at handling time. */
  phase?: FingerPhase | null;
}

/** How many periods to move: forward, back, or stay. */
export type PeriodStep = -1 | 0 | 1;

export interface GestureAxes {
  /**
   * Whether a vertical swipe also moves a period. True in month view, where
   * the grid has nothing to scroll; false in week and day, where vertical
   * belongs to the hour grid.
   */
  vertical: boolean;
}

/** Everything one gesture needs to remember. Plain data, copied on every feed. */
export interface WheelGesture {
  /** `timeStamp` of the last sample, for the idle gap that ends a gesture. */
  readonly last: number;
  /** Travel accumulated since the gesture began. */
  readonly x: number;
  readonly y: number;
  /** The axis this gesture belongs to; null until the travel says which. */
  readonly axis: "x" | "y" | null;
  /** True once a step has fired. Nothing in this gesture can fire again. */
  readonly fired: boolean;
  /** Decided from the first sample of the gesture and held for its duration. */
  readonly device: WheelDevice | null;
  /** When a step last fired, which rate-limits the wheel across gestures. */
  readonly emitted: number;
  /**
   * The native gesture number this belongs to, or null when the gesture was
   * begun without a phase to hand. A sample whose number differs from this one
   * is a new hand on the glass.
   */
  readonly gesture: number | null;
}

export interface WheelOutcome {
  readonly gesture: WheelGesture;
  readonly step: PeriodStep;
  /**
   * The gesture has taken this event over and the caller should
   * `preventDefault`. A horizontal swipe that reaches the webview unclaimed
   * becomes a back-navigation, which is a far worse outcome than a lost week.
   * A momentum tail is claimed too: the fingers are gone but the events are
   * still arriving, and an unclaimed one navigates just as well.
   */
  readonly claimed: boolean;
}

/** No gesture in progress. */
export const IDLE_GESTURE: WheelGesture = {
  last: Number.NEGATIVE_INFINITY,
  x: 0,
  y: 0,
  axis: null,
  fired: false,
  device: null,
  emitted: Number.NEGATIVE_INFINITY,
  gesture: null,
};

/**
 * A quiet gap this long ends a gesture.
 *
 * With a phase to read this is a backstop and no more: the gesture counter ends
 * gestures, and a stream that stops mid-swipe and resumes a second later is two
 * gestures by the counter before this timer has an opinion. It matters in the
 * no-phase fallback, where it is the *only* boundary, and there it has to sit
 * above the ~16ms spacing of a live stream and below the pause between two
 * deliberate flicks. Anything in the 100–150ms band does that.
 */
const IDLE_MS = 130;

/** Nominal sizes for the two non-pixel delta modes. */
const LINE_PX = 16;
const PAGE_PX = 400;

/** Travel after which a gesture stops being ambiguous and picks an axis. */
const AXIS_LOCK_PX = 24;

/**
 * How far the committed axis must beat the other one before a step fires. A
 * swipe meant to be horizontal is close to pure; anything genuinely diagonal is
 * more likely a scroll with a wobble in it.
 */
const AXIS_RATIO = 1.4;

/** Travel along the committed axis that counts as a swipe. */
const TRIGGER_PX = 42;

/**
 * First-sample magnitude that marks a mouse wheel. A trackpad ramps up from
 * near zero because a finger does; a wheel notch arrives at full size on the
 * first event.
 */
const WHEEL_FIRST_PX = 40;

/** Below this a wheel event is a stray, not a notch. */
const WHEEL_NOTCH_PX = 24;

/**
 * Spacing a wheel notch must have. Two guards rather than one, because device
 * classification is a heuristic: if a fast trackpad scroll is ever mistaken for
 * a wheel, its 16ms stream fails the gap and the mistake costs one period
 * instead of twenty.
 */
const WHEEL_GAP_MS = 30;
const WHEEL_COOLDOWN_MS = 60;

/**
 * Which instrument produced this event.
 *
 * `deltaMode` settles it outright when it is lines or pages — no trackpad
 * reports those. Otherwise the tell is a first event that is large, vertical,
 * whole-numbered and perfectly straight, which is a notch and not a finger.
 *
 * The native phase is not consulted here on purpose. It says whether fingers
 * are down, which is a fact about the *last* scroll event the application saw
 * anywhere — the mail list, a preferences sheet — and not about this one. The
 * delta shape is about this one.
 */
export function classifyDevice(sample: WheelSample): WheelDevice {
  if (sample.deltaMode !== 0) return "wheel";
  if (
    sample.deltaX === 0 &&
    Number.isInteger(sample.deltaY) &&
    Math.abs(sample.deltaY) >= WHEEL_FIRST_PX
  ) {
    return "wheel";
  }
  return "trackpad";
}

/** Delta modes other than pixels, converted to something comparable. */
function pixels(delta: number, mode: number): number {
  if (mode === 1) return delta * LINE_PX;
  if (mode === 2) return delta * PAGE_PX;
  return delta;
}

/** Whether an event on this axis belongs to the gesture rather than the page. */
function claims(axis: WheelGesture["axis"], axes: GestureAxes): boolean {
  return axis === "x" || (axis === "y" && axes.vertical);
}

/**
 * Feed one wheel event to the gesture and find out whether the period moves.
 *
 * The returned gesture replaces the one passed in; the caller keeps it in a ref
 * and hands it back on the next event.
 */
export function feedWheel(
  gesture: WheelGesture,
  sample: WheelSample,
  axes: GestureAxes,
): WheelOutcome {
  const now = sample.timeStamp;

  // A pinch abandons whatever gesture was in flight rather than pausing it, so
  // the swipe that was half-accumulated cannot finish itself after the zoom.
  if (sample.ctrlKey) {
    return {
      gesture: { ...IDLE_GESTURE, emitted: gesture.emitted },
      step: 0,
      claimed: false,
    };
  }

  const gap = now - gesture.last;
  const phase = sample.phase ?? null;
  const stale = gap > IDLE_MS;

  // Any real gap re-decides the instrument. A hand that leaves the trackpad for
  // a mouse leaves one, and the two are read from the shape of the delta rather
  // than from the phase — see `classifyDevice`.
  const device = stale ? classifyDevice(sample) : (gesture.device ?? classifyDevice(sample));

  // What ends a gesture. With a phase to read it is the counter, handled in
  // `swipe`, and the timer has to stay out of it: a stall in the middle of one
  // hand's movement is not a second swipe, and clearing `fired` on it would let
  // one hand move two periods. Without a phase, silence is the only boundary
  // there is.
  //
  // `emitted` survives the boundary on purpose: it is what stops a wheel spun
  // fast enough to break into separate gestures from running away.
  const base: WheelGesture =
    stale && (phase === null || device === "wheel")
      ? { ...IDLE_GESTURE, emitted: gesture.emitted, device }
      : { ...gesture, device };

  const dx = pixels(sample.deltaX, sample.deltaMode);
  const dy = pixels(sample.deltaY, sample.deltaMode);

  return device === "wheel"
    ? notch(base, { now, dy, gap }, axes)
    : swipe(base, { now, dx, dy, phase }, axes);
}

function notch(
  base: WheelGesture,
  { now, dy, gap }: { now: number; dy: number; gap: number },
  axes: GestureAxes,
): WheelOutcome {
  const gesture: WheelGesture = { ...base, last: now, device: "wheel" };

  // In week and day view a wheel does what it has always done: scroll the hour
  // grid. It has no horizontal axis to swipe with, and taking its vertical away
  // would trade a real scroll for a gesture the hardware cannot express well.
  if (!axes.vertical || Math.abs(dy) < WHEEL_NOTCH_PX) {
    return { gesture, step: 0, claimed: false };
  }

  if (gap < WHEEL_GAP_MS || now - base.emitted < WHEEL_COOLDOWN_MS) {
    return { gesture, step: 0, claimed: true };
  }

  return { gesture: { ...gesture, emitted: now }, step: dy > 0 ? 1 : -1, claimed: true };
}

function swipe(
  base: WheelGesture,
  {
    now,
    dx,
    dy,
    phase,
  }: { now: number; dx: number; dy: number; phase: FingerPhase | null },
  axes: GestureAxes,
): WheelOutcome {
  let start = base;

  if (phase !== null) {
    // Fingers landed since the last event this gesture saw, so whatever it was
    // doing is over — mid-tail, mid-swipe, it does not matter. Start again from
    // this event's travel. This is the case every inference-based fix got
    // wrong, and it is now a number comparison.
    if (phase.gesture !== start.gesture) {
      start = {
        ...IDLE_GESTURE,
        device: start.device,
        emitted: start.emitted,
        gesture: phase.gesture,
      };
    }

    // The coast. However far it travels and however long it runs, it is not the
    // hand and it moves nothing. Still claimed, because these events would
    // navigate the webview back just as well as the hand's would.
    if (!phase.down) {
      return {
        gesture: { ...start, last: now },
        step: 0,
        claimed: claims(start.axis, axes),
      };
    }
  }

  // One period per gesture. With a phase that means per hand-on-the-glass; the
  // fallback below means per stream-with-no-gap-in-it.
  if (start.fired) {
    return {
      gesture: { ...start, last: now },
      step: 0,
      claimed: claims(start.axis, axes),
    };
  }

  const x = start.x + dx;
  const y = start.y + dy;
  const axis =
    start.axis ??
    (Math.max(Math.abs(x), Math.abs(y)) >= AXIS_LOCK_PX
      ? Math.abs(x) >= Math.abs(y)
        ? "x"
        : "y"
      : null);
  const gesture: WheelGesture = { ...start, last: now, x, y, axis };
  const claimed = claims(axis, axes);

  if (axis === null) return { gesture, step: 0, claimed };
  if (axis === "y" && !axes.vertical) return { gesture, step: 0, claimed };

  const travel = axis === "x" ? x : y;
  const other = axis === "x" ? y : x;
  if (Math.abs(travel) < TRIGGER_PX || Math.abs(travel) < Math.abs(other) * AXIS_RATIO) {
    return { gesture, step: 0, claimed };
  }

  return {
    gesture: { ...gesture, fired: true, emitted: now },
    step: travel > 0 ? 1 : -1,
    claimed,
  };
}
