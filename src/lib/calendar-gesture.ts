/**
 * Turning wheel events into "move one period".
 *
 * A two-finger swipe on a trackpad is not one event. macOS emits a stream at
 * roughly 60Hz while the fingers move, and then — this is the part that breaks
 * naive implementations — it keeps emitting for another half second after the
 * fingers have left the glass, with the deltas decaying towards zero. A rule
 * like "deltaX over 40 means next week" turns one flick into fifteen weeks,
 * because the tail is forty more events that each clear 40.
 *
 * The DOM gives us nothing to lean on here. Safari and Chrome both withhold the
 * NSEvent momentum phase, so there is no flag that says "the fingers are gone".
 * Everything below is inference from the shape of the stream.
 *
 * Three rules do the work:
 *
 *  1. **A gesture is a stream, not an event.** Events less than `IDLE_MS` apart
 *     belong to the same gesture. A period moves at most once per gesture, so
 *     however long the tail runs, it moves one period.
 *
 *  2. **A gesture commits to an axis.** Once the accumulated travel is
 *     unambiguous, the gesture is horizontal or vertical for the rest of its
 *     life. This is what stops the sideways drift that rides along with every
 *     real trackpad scroll from paging the view: by the time the drift adds up
 *     to anything, the gesture has been vertical for a hundred events. macOS
 *     locks axes for the same reason.
 *
 *  3. **A gesture that has fired is finished.** Nothing later in the stream can
 *     make it move a second period. Only silence — `TAIL_MS` of it — starts the
 *     next gesture, so a second period costs a second swipe.
 *
 *     Two cleverer rules stood here first, and a captured trace killed both.
 *     Each watched for the stream to wind down and then climb again, on the
 *     theory that momentum decays and only fingers can accelerate: the first
 *     measured per-event magnitude, the second measured speed. Both statements
 *     about momentum are true. Neither is true of a hand still on the glass.
 *
 *     The capture is 144 events, 1313ms, 2245px, one swipe. Its finger half
 *     reads `41, 10, 24, 5, 31, 5, 25, 3, 20` — alternating by a factor of ten,
 *     event to event, because a finger is not a ramp and the sampling is not
 *     even. Every one of those troughs-and-doubles is a re-arm. That trace moved
 *     **eight** periods, five of them before the hand had left the trackpad.
 *
 *     So the rule is stated rather than inferred, and it is the owner's: never
 *     more than one period per swipe. The cost is a second flick thrown inside
 *     the first one's momentum being swallowed, and that is the right side to
 *     err on — overshooting means going back, which is the thing he reported.
 *
 * Everything here is pure — samples in, a step out — which is the only way to
 * test the momentum handling, since the thing being tested is a sequence of
 * fifty events with particular magnitudes and timings.
 */

/**
 * A mouse wheel and a trackpad are different instruments that arrive through
 * the same event. A wheel has no horizontal axis, no momentum, and a coarse
 * discrete step; a trackpad has all three and a fine one.
 */
export type WheelDevice = "trackpad" | "wheel";

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
  /** True once a step has fired. Everything after it is swallowed. */
  readonly fired: boolean;
  /** Decided from the first sample of the gesture and held for its duration. */
  readonly device: WheelDevice | null;
  /** When a step last fired, which rate-limits the wheel across gestures. */
  readonly emitted: number;
}

export interface WheelOutcome {
  readonly gesture: WheelGesture;
  readonly step: PeriodStep;
  /**
   * The gesture has taken this event over and the caller should
   * `preventDefault`. A horizontal swipe that reaches the webview unclaimed
   * becomes a back-navigation, which is a far worse outcome than a lost week.
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
};

/**
 * A quiet gap this long ends a gesture. It has to sit above the ~16ms spacing
 * of a live stream and below the pause between two deliberate flicks, and
 * anything in the 100–150ms band does that.
 */
const IDLE_MS = 130;

/**
 * How much quiet a gesture that has already moved a period needs before it
 * counts as over — and, since there is no re-arm, the whole of what separates
 * one swipe from the next.
 *
 * It is a gap between two events, not a clock on the gesture: however long a
 * tail runs it moves one period, because length does not end a gesture. Only
 * silence does.
 *
 * Two things set the number from opposite sides. Below it, anything that blocks
 * the main thread mid-tail — a slow commit, a garbage collection — puts a hole
 * in the stream, and what comes out the far side is one fat coalesced event
 * carrying every frame the stall swallowed, which clears `TRIGGER_PX` without
 * trying. A headless run with a stalling renderer managed 350ms holes. Above
 * it, every millisecond is dead time after a tail ends, where a second swipe
 * does nothing and the app feels broken.
 *
 * The captured trace never gaps by more than 66ms while it is running, so half
 * a second sits clear of both edges.
 */
const TAIL_MS = 500;

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
  const over = gap > (gesture.fired ? TAIL_MS : IDLE_MS);

  // A hole longer than the idle gap is a real seam in the stream, and the long
  // window a fired gesture gets is there to survive a stall, not to hold on to
  // the trackpad after the hand has moved to a mouse. If what comes out the far
  // side of the seam is shaped like a notch, take it as one.
  const swapped =
    !over &&
    gap > IDLE_MS &&
    gesture.device === "trackpad" &&
    classifyDevice(sample) === "wheel";

  // `emitted` survives the gesture boundary on purpose: it is what stops a
  // wheel spun fast enough to break into separate gestures from running away.
  const base: WheelGesture =
    over || swapped
      ? { ...IDLE_GESTURE, emitted: gesture.emitted, device: classifyDevice(sample) }
      : gesture;

  const dx = pixels(sample.deltaX, sample.deltaMode);
  const dy = pixels(sample.deltaY, sample.deltaMode);

  return base.device === "wheel"
    ? notch(base, { now, dy, gap }, axes)
    : swipe(base, { now, dx, dy }, axes);
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
  { now, dx, dy }: { now: number; dx: number; dy: number },
  axes: GestureAxes,
): WheelOutcome {
  const start = base;

  if (start.fired) {
    // A gesture that has fired is finished. Nothing in the rest of the stream
    // can make it fire again; only silence starts the next one.
    //
    // Two cleverer rules stood here and both were wrong on a real trackpad, so
    // this one is stated rather than inferred. The first read per-event
    // magnitude, and the second read speed, each looking for the stream to wind
    // down and climb again on the theory that momentum cannot climb. Both are
    // true of momentum and neither is true of *fingers*: a captured swipe of
    // his — 144 events, 1313ms, 2245px — alternates hard while the hand is
    // still on the glass, `41, 10, 24, 5, 31, 5, 25, 3, 20`, because a finger
    // is not a ramp. Every one of those troughs-then-doubles reads as a second
    // swipe. That trace moved **eight** periods, five of them before his
    // fingers had even left.
    //
    // So there is no re-arm. One swipe is one period, and a second period costs
    // a second swipe — which is what he asked for, and is the only rule the
    // capture supports. The cost is a second flick thrown inside the first
    // one's momentum being swallowed; overshoot is the worse failure, since
    // landing two weeks out means going back.
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
