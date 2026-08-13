import { describe, expect, it } from "vitest";
import {
  IDLE_GESTURE,
  classifyDevice,
  feedWheel,
  type FingerPhase,
  type GestureAxes,
  type PeriodStep,
  type WheelSample,
} from "./calendar-gesture";

const WEEK: GestureAxes = { vertical: false };
const MONTH: GestureAxes = { vertical: true };

/** One event every 16ms, which is what a 60Hz wheel stream actually looks like. */
const FRAME = 16;

/**
 * A two-finger flick, synthesised the way macOS emits one: a short finger phase
 * where the magnitude ramps up, then a long momentum tail decaying towards
 * zero, both at 60Hz with no gap between them.
 *
 * Each sample carries the phase the native monitor would have published for it,
 * which is the whole point: the finger events say `down`, the tail says not,
 * and both halves belong to the same gesture number. `gesture` picks that
 * number, so two flicks in a row are told apart by the same thing that tells
 * them apart in the app.
 *
 * `drift` is the small perpendicular component that rides along with every real
 * trackpad gesture — no human swipe is perfectly axis-aligned.
 */
function flick(
  start: number,
  options: {
    axis: "x" | "y";
    direction: 1 | -1;
    peak?: number;
    drift?: number;
    decay?: number;
    /** macOS only spins up momentum above a velocity; a slow nudge gets none. */
    momentum?: boolean;
    /** Which gesture the native monitor called this. */
    gesture?: number;
    /** Drop the phase entirely — a browser, where there is no Rust. */
    blind?: boolean;
  },
): WheelSample[] {
  const {
    axis,
    direction,
    peak = 38,
    drift = 0.1,
    decay = 0.9,
    momentum = true,
    gesture = 1,
    blind = false,
  } = options;
  const magnitudes: number[] = [];

  // Finger phase: the ramp a finger makes as it gets moving.
  for (const fraction of [0.05, 0.18, 0.42, 0.68, 0.88, 1]) magnitudes.push(peak * fraction);
  const fingerEvents = magnitudes.length;
  // Momentum phase: the fingers are gone and the OS keeps talking.
  if (momentum) for (let m = peak * decay; m > 0.1; m *= decay) magnitudes.push(m);

  return magnitudes.map((magnitude, index) => {
    const along = magnitude * direction;
    const across = magnitude * drift;
    return {
      deltaX: axis === "x" ? along : across,
      deltaY: axis === "x" ? across : along,
      deltaMode: 0,
      timeStamp: start + index * FRAME,
      phase: blind ? null : { down: index < fingerEvents, gesture },
    };
  });
}

/**
 * A stream written out event by event, for the cases where the exact numbers
 * are the point and a generator would hide them.
 *
 * `[magnitude, gap, down]` triples: how far the fingers moved along the axis,
 * how long since the previous event, and whether they were still on the glass.
 * The perpendicular drift rides along at the same 10% every real gesture has.
 * The gesture number increments each time `down` goes from false to true, which
 * is exactly what `scroll::transition` does in Rust.
 */
function replay(
  start: number,
  axis: "x" | "y",
  direction: 1 | -1,
  events: readonly (readonly [number, number, boolean])[],
): WheelSample[] {
  let clock = start;
  let gesture = 0;
  let wasDown = false;
  return events.map(([magnitude, gap, down]) => {
    clock += gap;
    if (down && !wasDown) gesture += 1;
    wasDown = down;
    const along = magnitude * direction;
    const across = magnitude * 0.1;
    return {
      deltaX: axis === "x" ? along : across,
      deltaY: axis === "x" ? across : along,
      deltaMode: 0,
      timeStamp: clock,
      phase: { down, gesture },
    };
  });
}

/**
 * A long two-finger vertical scroll with sideways drift. The drift is small per
 * event and always in the same direction, so over a scroll this long it adds up
 * to several hundred pixels sideways — far past any single-event threshold.
 * The fingers stay down throughout: this is a drag, not a flick.
 */
function verticalScrollWithDrift(start: number, events = 60): WheelSample[] {
  return Array.from({ length: events }, (_, index) => ({
    deltaX: 2.4,
    deltaY: index < 4 ? 12 + index * 12 : 56,
    deltaMode: 0,
    timeStamp: start + index * FRAME,
    phase: { down: true, gesture: 1 } satisfies FingerPhase,
  }));
}

/** A mouse wheel notch: coarse, whole, perfectly vertical, no tail, no phase. */
function notch(timeStamp: number, direction: 1 | -1, deltaMode = 0): WheelSample {
  return {
    deltaX: 0,
    deltaY: (deltaMode === 0 ? 100 : 3) * direction,
    deltaMode,
    timeStamp,
    phase: null,
  };
}

/** Run a stream through the gesture and collect the steps it produced. */
function run(samples: WheelSample[], axes: GestureAxes): PeriodStep[] {
  let gesture = IDLE_GESTURE;
  const steps: PeriodStep[] = [];
  for (const sample of samples) {
    const outcome = feedWheel(gesture, sample, axes);
    gesture = outcome.gesture;
    if (outcome.step !== 0) steps.push(outcome.step);
  }
  return steps;
}

/** Which events the gesture took over, as a count. */
function claimed(samples: WheelSample[], axes: GestureAxes): number {
  let gesture = IDLE_GESTURE;
  let count = 0;
  for (const sample of samples) {
    const outcome = feedWheel(gesture, sample, axes);
    gesture = outcome.gesture;
    if (outcome.claimed) count += 1;
  }
  return count;
}

/** How many events into the stream the step fired. */
function firedAt(samples: WheelSample[], axes: GestureAxes): number | null {
  let gesture = IDLE_GESTURE;
  for (const [index, sample] of samples.entries()) {
    const outcome = feedWheel(gesture, sample, axes);
    gesture = outcome.gesture;
    if (outcome.step !== 0) return index;
  }
  return null;
}

describe("classifyDevice", () => {
  it("calls line and page deltas a wheel", () => {
    expect(classifyDevice({ deltaX: 0, deltaY: 3, deltaMode: 1, timeStamp: 0 })).toBe("wheel");
    expect(classifyDevice({ deltaX: 0, deltaY: 1, deltaMode: 2, timeStamp: 0 })).toBe("wheel");
  });

  it("calls a large whole vertical-only first event a wheel", () => {
    expect(classifyDevice(notch(0, 1))).toBe("wheel");
  });

  it("calls the small ramping start of a swipe a trackpad", () => {
    const [first] = flick(0, { axis: "y", direction: 1 });
    expect(classifyDevice(first)).toBe("trackpad");
  });

  it("calls a large first event with any sideways component a trackpad", () => {
    expect(classifyDevice({ deltaX: 0.6, deltaY: 90, deltaMode: 0, timeStamp: 0 })).toBe("trackpad");
  });
});

describe("horizontal swipes in week and day view", () => {
  it("moves exactly one period per flick, however long the tail runs", () => {
    const stream = flick(1000, { axis: "x", direction: 1 });
    expect(stream.length).toBeGreaterThan(40);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("moves back on a swipe the other way", () => {
    expect(run(flick(1000, { axis: "x", direction: -1 }), WEEK)).toEqual([-1]);
  });

  it("moves one period per flick for a hard swipe with a long tail", () => {
    const stream = flick(1000, { axis: "x", direction: 1, peak: 140, decay: 0.95 });
    expect(stream.length).toBeGreaterThan(130);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("fires while the fingers are still down, not when the tail ends", () => {
    // Responsiveness is the property, and it is the one the last fix traded
    // away: it waited for silence, and a tail runs about 1.2 seconds. The step
    // has to land within the first handful of events of a flick.
    const stream = flick(1000, { axis: "x", direction: 1, peak: 140 });
    const index = firedAt(stream, WEEK);
    expect(index).not.toBeNull();
    expect(index).toBeLessThan(4);
    expect(stream[index!].phase?.down).toBe(true);
  });

  it("moves a second period on a second swipe the instant the fingers land", () => {
    // Bruno's report: "first swipe works but then next ones don't." The second
    // flick starts one frame into the first one's 40-event tail — no quiet gap
    // anywhere, and every event under 130ms from its neighbour.
    const first = flick(1000, { axis: "x", direction: 1 });
    const cut = first.slice(0, 8);
    const second = flick(cut[cut.length - 1].timeStamp + FRAME, {
      axis: "x",
      direction: 1,
      gesture: 2,
    });
    expect(run([...cut, ...second], WEEK)).toEqual([1, 1]);
  });

  it("moves eight periods for eight swipes with no pause between any of them", () => {
    const stream: WheelSample[] = [];
    let clock = 1000;
    for (let index = 0; index < 8; index += 1) {
      // Each flick cut short by the next: the fingers land again while the
      // previous tail is still running, which is what paging quickly looks like.
      const one = flick(clock, { axis: "x", direction: 1, gesture: index + 1 }).slice(0, 12);
      stream.push(...one);
      clock = one[one.length - 1].timeStamp + FRAME;
    }
    expect(run(stream, WEEK)).toEqual([1, 1, 1, 1, 1, 1, 1, 1]);
  });

  it("moves one period when the app stalls in the middle of the tail", () => {
    // Two 350ms holes punched into one flick's tail, which is what a slow
    // commit or a garbage collection does to the stream. Each hole is far
    // longer than the gap that used to separate two gestures — and now nothing
    // about a hole means anything, because the fingers are demonstrably gone.
    const stream = flick(1000, { axis: "x", direction: 1, peak: 140, decay: 0.95 });
    const stalled = stream.map((sample, index) => ({
      ...sample,
      timeStamp: sample.timeStamp + (index > 30 ? 350 : 0) + (index > 70 ? 350 : 0),
    }));
    expect(run(stalled, WEEK)).toEqual([1]);
  });

  it("moves one period when the app stalls with the fingers still down", () => {
    // The mirror of the case above, and the reason the idle timer had to be
    // taken off the phase path: firing a step re-renders a week of blocks, and
    // the hand is still moving when the main thread comes back. A 300ms hole
    // mid-swipe is one hand, not two.
    const stream = replay(1000, "x", 1, [
      [5, 16, true],
      [20, 16, true],
      [45, 16, true], // fires
      [66, 300, true], // the re-render
      [80, 16, true],
      [90, 16, true],
      [70, 16, false],
      [60, 16, false],
    ]);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("moves nothing on a tail however violently its magnitude jumps", () => {
    // Coalescing: WebKit answers a busy main thread by merging the wheel events
    // it could not deliver, so six frames of tail arrive as one fat event. It
    // used to take a rule about speed rather than size to survive this. Now the
    // event says `down: false` and there is nothing to survive.
    const stream = replay(1000, "x", 1, [
      [5, 16, true],
      [20, 16, true],
      [45, 16, true], // fires
      [66, 16, true],
      [80, 16, false],
      [286, 96, false], // six frames of tail, merged
      [70, 16, false],
      [400, 16, false], // and an absurd one, for good measure
    ]);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("moves one period when the fingers are still speeding up as the step fires", () => {
    // The bug the previous fix was written for: one swipe, two weeks. 42px goes
    // by on the third event while the hand is still accelerating, so everything
    // after it is a *rise* — which used to be the entire signal for "the fingers
    // came back down". Six events later, 120px paged a second week.
    //
    // Nothing here rises out of anything now. `fired` is set and the gesture
    // number never changed, so the rest of the ramp is arithmetic nobody reads.
    const stream = replay(1000, "x", 1, [
      [3.3, 16, true],
      [13.3, 16, true],
      [30.0, 16, true], // 46.6px accumulated — the step fires here
      [53.3, 16, true], // still climbing
      [83.3, 16, true],
      [120.0, 16, true], // the top of the flick, and the old second week
      [112.8, 16, false],
      [106.0, 16, false],
      [99.7, 16, false],
      [93.7, 16, false],
    ]);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("moves one period for a real captured swipe, phase and all", () => {
    // Not synthesised. One flick of Bruno's hand across the trackpad, recorded
    // by `scripts/wheel-trace.html` running *inside* the app — so the deltas
    // are WebKit's own and the `down` column is what the NSEvent said:
    //
    //   139 events · 1320ms · axis dx · travel 2803px · median gap 8ms
    //   1 gesture · 15 events with fingers down · 124 coasting
    //
    // Two numbers in there are the whole story. The hand was on the glass for
    // **15 of 139 events** and contributed **284px of the 2803**: 90% of the
    // travel and 89% of the stream arrived after the fingers had gone. And the
    // finger half is not a ramp — `20, 17, 13, 25, 17, 31, 18, 24, 13, 29, 15,
    // 30` falls and climbs six times in 100ms, because a hand is not smooth and
    // a 120Hz sampler is not even. Every rule that read a fall-then-rise as
    // "the fingers came back down" fired on those, mid-swipe; this trace moved
    // eight periods under the last one, five of them before the hand lifted.
    //
    // Note also that the tail's events are *bigger* than the hand's — 68, 76,
    // 73 against a peak of 31 — which is why no threshold on magnitude could
    // ever have separated them either.
    const captured: (readonly [number, number, boolean])[] = [
      // The hand. Verbatim.
      [2, 0, true],
      [12, 9, true],
      [18, 8, true],
      [20, 8, true], // 52px accumulated — the step fires here, 25ms in
      [17, 7, true],
      [13, 2, true],
      [25, 6, true],
      [17, 2, true],
      [31, 6, true],
      [18, 2, true],
      [24, 6, true],
      [13, 3, true],
      [29, 7, true],
      [15, 1, true],
      [30, 7, true],
      // The coast. The first thirteen verbatim; the rest of the 124 decay on
      // from there, which is all they do.
      [68, 4, false],
      [76, 8, false],
      [73, 9, false],
      [72, 8, false],
      [70, 8, false],
      [69, 8, false],
      [65, 9, false],
      [62, 8, false],
      [61, 8, false],
      [59, 9, false],
      [59, 9, false],
      [56, 8, false],
      [57, 8, false],
    ];
    for (let m = 56, n = 0; n < 111; n += 1, m *= 0.975) {
      captured.push([Math.round(m * 10) / 10, 8, false]);
    }

    expect(captured).toHaveLength(139);
    expect(captured.filter(([, , down]) => down)).toHaveLength(15);
    expect(run(replay(1000, "x", 1, captured), WEEK)).toEqual([1]);
  });

  it("moves two periods for that same captured swipe done twice", () => {
    // The other half of the report — "first swipe works but then next ones
    // don't". The second hand lands 120ms into the first one's 1.2-second
    // coast, which is a perfectly ordinary way to page two weeks and which the
    // last fix answered by doing nothing for the next second.
    const hand: (readonly [number, number, boolean])[] = [
      [2, 8, true],
      [12, 9, true],
      [18, 8, true],
      [20, 8, true],
      [17, 7, true],
      [13, 2, true],
    ];
    const coast: (readonly [number, number, boolean])[] = Array.from(
      { length: 15 },
      (_, index) => [70 * 0.97 ** index, 8, false] as const,
    );

    expect(run(replay(1000, "x", 1, [...hand, ...coast, ...hand, ...coast]), WEEK)).toEqual([1, 1]);
  });

  it("moves one period on a 120Hz stream, where every delta is halved", () => {
    // A ProMotion MacBook samples twice as often and reports half as much each
    // time, so the ramp is spread over twice the events.
    const magnitudes: (readonly [number, number, boolean])[] = [];
    for (let i = 1; i <= 12; i += 1) magnitudes.push([60 * (i / 12) ** 2, 8, true]);
    for (let m = 60 * 0.97; m > 0.1; m *= 0.97) magnitudes.push([m, 8, false]);
    expect(run(replay(1000, "x", 1, magnitudes), WEEK)).toEqual([1]);
  });

  it("moves one period for a slow deliberate drag with no momentum behind it", () => {
    // Below a velocity macOS spins up no momentum at all, so the stream simply
    // stops when the fingers do.
    const magnitudes: (readonly [number, number, boolean])[] = Array.from(
      { length: 30 },
      () => [6, 16, true] as const,
    );
    expect(run(replay(1000, "x", 1, magnitudes), WEEK)).toEqual([1]);
  });

  it("takes over its own events so the webview cannot navigate back", () => {
    const stream = flick(1000, { axis: "x", direction: -1 });
    // Everything past the first couple of ambiguous events, tail included — a
    // momentum event navigates back just as well as a finger one.
    expect(claimed(stream, WEEK)).toBeGreaterThan(stream.length - 4);
  });

  it("ignores a nudge too small to be deliberate", () => {
    expect(run(flick(1000, { axis: "x", direction: 1, peak: 8, momentum: false }), WEEK)).toEqual(
      [],
    );
  });

  it("ignores a tail that arrives with no finger phase in front of it", () => {
    // The app started, or the calendar mounted, in the middle of somebody
    // else's coast. There is no hand here and there must be no step.
    const tail = flick(1000, { axis: "x", direction: 1, peak: 140 }).slice(6);
    expect(tail.every((sample) => sample.phase?.down === false)).toBe(true);
    expect(run(tail, WEEK)).toEqual([]);
  });
});

describe("vertical scrolling in week and day view", () => {
  it("never moves a period, however much sideways drift adds up", () => {
    const stream = verticalScrollWithDrift(1000);
    const sideways = stream.reduce((total, sample) => total + sample.deltaX, 0);
    expect(sideways).toBeGreaterThan(140);
    expect(run(stream, WEEK)).toEqual([]);
  });

  it("leaves those events alone so the hour grid still scrolls", () => {
    expect(claimed(verticalScrollWithDrift(1000), WEEK)).toBe(0);
  });

  it("never moves a period on a vertical flick", () => {
    expect(run(flick(1000, { axis: "y", direction: 1 }), WEEK)).toEqual([]);
  });

  it("does not fire when a scroll pauses and resumes, over and over", () => {
    const stream: WheelSample[] = [];
    for (let index = 0; index < 6; index += 1) {
      stream.push(...verticalScrollWithDrift(1000 + index * 2000, 20));
    }
    expect(run(stream, WEEK)).toEqual([]);
  });
});

describe("month view", () => {
  it("moves one month per vertical flick", () => {
    expect(run(flick(1000, { axis: "y", direction: 1 }), MONTH)).toEqual([1]);
    expect(run(flick(1000, { axis: "y", direction: -1 }), MONTH)).toEqual([-1]);
  });

  it("moves one month per horizontal flick too", () => {
    expect(run(flick(1000, { axis: "x", direction: 1 }), MONTH)).toEqual([1]);
  });

  it("moves four months for four quick vertical flicks", () => {
    const stream: WheelSample[] = [];
    for (let index = 0; index < 4; index += 1) {
      stream.push(
        ...flick(1000 + index * 230, { axis: "y", direction: 1, gesture: index + 1 }).slice(0, 14),
      );
    }
    expect(run(stream, MONTH)).toEqual([1, 1, 1, 1]);
  });

  it("moves one month for a long steady drag, not one per event", () => {
    const stream = verticalScrollWithDrift(1000);
    expect(run(stream, MONTH)).toEqual([1]);
  });
});

describe("mouse wheels", () => {
  it("does nothing in week and day view, leaving the hour grid to scroll", () => {
    const spin = [0, 90, 180, 270].map((offset) => notch(1000 + offset, 1));
    expect(run(spin, WEEK)).toEqual([]);
    expect(claimed(spin, WEEK)).toBe(0);
  });

  it("moves one month per notch in month view", () => {
    const spin = [0, 90, 180, 270].map((offset) => notch(1000 + offset, 1));
    expect(run(spin, MONTH)).toEqual([1, 1, 1, 1]);
  });

  it("moves back on notches the other way", () => {
    expect(run([notch(1000, -1)], MONTH)).toEqual([-1]);
  });

  it("reads line and page deltas as notches", () => {
    expect(run([notch(1000, 1, 1), notch(1090, -1, 1)], MONTH)).toEqual([1, -1]);
    expect(run([notch(1000, 1, 2)], MONTH)).toEqual([1]);
  });

  it("is read as a wheel even when a trackpad flick's tail was still running", () => {
    // A hand that leaves the trackpad for the mouse. The notches carry no
    // phase, because a wheel has none, and they must not be absorbed as tail.
    const swipe = flick(1000, { axis: "y", direction: 1 });
    const after = swipe[swipe.length - 1].timeStamp + 200;
    const spin = [0, 90, 180].map((offset) => notch(after + offset, -1));
    expect(run([...swipe, ...spin], MONTH)).toEqual([1, -1, -1, -1]);
  });

  it("rate-limits a spin fast enough to arrive at frame rate", () => {
    const spin = Array.from({ length: 30 }, (_, index) => notch(1000 + index * FRAME, 1));
    expect(run(spin, MONTH)).toEqual([1]);
  });

  it("is unaffected by a stale phase left behind by the last trackpad gesture", () => {
    // The phase says whatever the last scroll event anywhere in the app said,
    // and a notch arriving before the monitor's `no-phase` has crossed the
    // bridge would see it. The wheel path must not read it at all.
    const spin = [0, 90, 180].map((offset) => ({
      ...notch(1000 + offset, 1),
      phase: { down: true, gesture: 7 } satisfies FingerPhase,
    }));
    expect(run(spin, MONTH)).toEqual([1, 1, 1]);
  });
});

describe("pinch to zoom", () => {
  it("is ignored on every axis and in every view", () => {
    const pinch = Array.from({ length: 20 }, (_, index) => ({
      deltaX: 0,
      deltaY: -14,
      deltaMode: 0,
      timeStamp: 1000 + index * FRAME,
      ctrlKey: true,
      phase: { down: true, gesture: 1 } satisfies FingerPhase,
    }));
    expect(run(pinch, MONTH)).toEqual([]);
    expect(run(pinch, WEEK)).toEqual([]);
    expect(claimed(pinch, MONTH)).toBe(0);
  });

  it("abandons a swipe it interrupts rather than letting it finish", () => {
    const phase = { down: true, gesture: 1 } satisfies FingerPhase;
    const before = [20, 18].map((deltaX, index) => ({
      deltaX,
      deltaY: 1,
      deltaMode: 0,
      timeStamp: 1000 + index * FRAME,
      phase,
    }));
    const after = [10, 10].map((deltaX, index) => ({
      deltaX,
      deltaY: 1,
      deltaMode: 0,
      timeStamp: 1048 + index * FRAME,
      phase,
    }));
    const pinch: WheelSample = {
      deltaX: 0,
      deltaY: -20,
      deltaMode: 0,
      timeStamp: 1032,
      ctrlKey: true,
      phase,
    };

    // Uninterrupted the four events add to 58 and move a week.
    expect(run([...before, ...after], WEEK)).toEqual([1]);
    // With the pinch between them the first 38 is gone and 20 is not enough.
    expect(run([...before, pinch, ...after], WEEK)).toEqual([]);
  });
});

describe("the lag between the native phase and the DOM stream", () => {
  // The two arrive on different pipes: the monitor block runs before
  // `sendEvent:`, the wheel event is produced in the web content process, and
  // the phase has to cross back into the webview to be read. Neither direction
  // of that lag may cost a swipe. See the header of `src-tauri/src/scroll.rs`.

  it("survives a late lift, where the tail's first events still say `down`", () => {
    const stream = replay(1000, "x", 1, [
      [5, 16, true],
      [20, 16, true],
      [45, 16, true], // fires here, long before the hand lifts
      [66, 16, true],
      [80, 16, true],
      [75, 16, true], // actually momentum; the lift has not crossed yet
      [70, 16, true],
      [66, 16, false],
      [62, 16, false],
    ]);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("survives an early lift, where the last finger events say `up`", () => {
    // The step fires two events in, so losing the end of the hand's movement
    // costs nothing at all.
    const stream = replay(1000, "x", 1, [
      [5, 16, true],
      [20, 16, true],
      [45, 16, true], // fires
      [66, 16, false], // still the hand, but the lift got here first
      [80, 16, false],
    ]);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("survives a late landing, where a new swipe's first events say `up`", () => {
    // Two events of the second swipe are discarded as the first one's tail.
    // What is left is still a swipe.
    const stream = replay(1000, "x", 1, [
      [5, 16, true],
      [20, 16, true],
      [45, 16, true], // fires
      [40, 16, false],
      [30, 16, false],
      [4, 48, false], // the second swipe starts; `down` has not arrived
      [14, 16, false],
      [30, 16, true], // and now it has
      [53, 16, true],
      [83, 16, true],
    ]);
    expect(run(stream, WEEK)).toEqual([1, 1]);
  });
});

describe("with no native phase — a browser, and nothing in the app", () => {
  // `bun run dev`, the fixture harness, and these tests without a phase. There
  // is no Rust and no monitor, so the only boundary left is silence. Worse than
  // the real path, and it has to stay correct on the cases it can see.

  it("still moves exactly one period per flick, tail and all", () => {
    const stream = flick(1000, { axis: "x", direction: 1, blind: true });
    expect(stream.length).toBeGreaterThan(40);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("treats a gap longer than the idle window as a new gesture", () => {
    const stream = [
      ...flick(1000, { axis: "x", direction: 1, blind: true }).slice(0, 10),
      ...flick(3000, { axis: "x", direction: -1, blind: true }).slice(0, 10),
    ];
    expect(run(stream, WEEK)).toEqual([1, -1]);
  });

  it("cannot tell a second swipe from a tail without a pause, and does not try", () => {
    // The limitation, stated as a test so it is not mistaken for a bug later.
    // Two flicks running together with no quiet gap read as one gesture, and
    // one gesture is one period. In the app this case is the gesture counter's,
    // and it moves two — see "moves a second period on a second swipe".
    const first = flick(1000, { axis: "x", direction: 1, blind: true }).slice(0, 8);
    const second = flick(first[first.length - 1].timeStamp + FRAME, {
      axis: "x",
      direction: 1,
      blind: true,
    });
    expect(run([...first, ...second], WEEK)).toEqual([1]);
  });
});

describe("gesture state", () => {
  it("starts idle and stays pure — the same input twice gives the same answer", () => {
    const sample = flick(1000, { axis: "x", direction: 1 })[5];
    const first = feedWheel(IDLE_GESTURE, sample, WEEK);
    const second = feedWheel(IDLE_GESTURE, sample, WEEK);
    expect(first).toEqual(second);
    expect(IDLE_GESTURE.fired).toBe(false);
  });

  it("picks up a gesture that was already under way when it started watching", () => {
    // The calendar mounts mid-swipe. The first event it sees says `down` with a
    // number this gesture has never held, which is a new gesture by every rule
    // here, and the swipe from there is enough.
    const stream = replay(1000, "x", 1, [
      [45, 16, true],
      [50, 16, true],
      [40, 16, false],
    ]);
    expect(run(stream, WEEK)).toEqual([1]);
  });
});
