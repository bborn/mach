import { describe, expect, it } from "vitest";
import {
  IDLE_GESTURE,
  classifyDevice,
  feedWheel,
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
 * zero, both at 60Hz with no gap between them. A real flick's tail runs for
 * dozens of events, which is exactly the thing that has to be absorbed.
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
  },
): WheelSample[] {
  const { axis, direction, peak = 38, drift = 0.1, decay = 0.9, momentum = true } = options;
  const magnitudes: number[] = [];

  // Finger phase: the ramp a finger makes as it gets moving.
  for (const fraction of [0.05, 0.18, 0.42, 0.68, 0.88, 1]) magnitudes.push(peak * fraction);
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
    };
  });
}

/**
 * A stream written out event by event, for the cases where the exact numbers
 * are the point and a generator would hide them.
 *
 * `[magnitude, gap]` pairs: how far the fingers moved along the axis, and how
 * long since the previous event. The perpendicular drift rides along at the
 * same 10% every real gesture has.
 */
function replay(
  start: number,
  axis: "x" | "y",
  direction: 1 | -1,
  events: readonly (readonly [number, number])[],
): WheelSample[] {
  let clock = start;
  return events.map(([magnitude, gap]) => {
    clock += gap;
    const along = magnitude * direction;
    const across = magnitude * 0.1;
    return {
      deltaX: axis === "x" ? along : across,
      deltaY: axis === "x" ? across : along,
      deltaMode: 0,
      timeStamp: clock,
    };
  });
}

/**
 * A long two-finger vertical scroll with sideways drift. The drift is small per
 * event and always in the same direction, so over a scroll this long it adds up
 * to several hundred pixels sideways — far past any single-event threshold.
 */
function verticalScrollWithDrift(start: number, events = 60): WheelSample[] {
  return Array.from({ length: events }, (_, index) => ({
    deltaX: 2.4,
    deltaY: index < 4 ? 12 + index * 12 : 56,
    deltaMode: 0,
    timeStamp: start + index * FRAME,
  }));
}

/** A mouse wheel notch: coarse, whole, perfectly vertical, no tail. */
function notch(timeStamp: number, direction: 1 | -1, deltaMode = 0): WheelSample {
  return {
    deltaX: 0,
    deltaY: (deltaMode === 0 ? 100 : 3) * direction,
    deltaMode,
    timeStamp,
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

  it("moves one period for four flicks thrown with no pause between them", () => {
    // This asserted four until a real capture killed the rule that produced
    // them. Each flick is cut short by the next: the fingers land again ~230ms
    // in, macOS cancels the momentum, a fresh ramp begins, and no gap in the
    // stream ever reaches `TAIL_MS`. By the only rule that survived his trace
    // that is one gesture, so it is one period.
    //
    // This is the cost of the fix and it is a real one — paging quickly now
    // needs a beat between swipes. It is the side to err on: overshooting means
    // going back, which is what he reported. Restoring it needs a captured
    // *double* swipe to calibrate against, which we do not have.
    const stream: WheelSample[] = [];
    for (let index = 0; index < 4; index += 1) {
      stream.push(...flick(1000 + index * 230, { axis: "x", direction: 1 }).slice(0, 14));
    }
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("moves one period when the flicks run together with no quiet gap at all", () => {
    // The same thing at its most extreme: the next flick starts before the last
    // tail has finished, so every event is under 130ms from its neighbour and
    // the whole stream is unambiguously one gesture.
    const stream: WheelSample[] = [];
    let clock = 1000;
    for (let index = 0; index < 4; index += 1) {
      const one = flick(clock, { axis: "x", direction: 1 }).slice(0, 20);
      stream.push(...one);
      clock = one[one.length - 1].timeStamp + FRAME;
    }
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("moves one period when the app stalls in the middle of the tail", () => {
    // Two 350ms holes punched into one flick's tail, which is what a slow
    // commit or a garbage collection does to the stream. Each hole is longer
    // than the gap that separates two gestures, so nothing but the decaying
    // magnitude says these events still belong to the flick that fired.
    const stream = flick(1000, { axis: "x", direction: 1, peak: 140, decay: 0.95 });
    const stalled = stream.map((sample, index) => ({
      ...sample,
      timeStamp: sample.timeStamp + (index > 30 ? 350 : 0) + (index > 70 ? 350 : 0),
    }));
    expect(run(stalled, WEEK)).toEqual([1]);
  });

  it("still moves on a deliberate second swipe after the tail has died away", () => {
    // The tail runs out, silence, then a modest swipe. Silence is now the whole
    // of what separates two swipes, so the pause has to clear `TAIL_MS` — which
    // is the contract this test exists to hold: a second swipe he actually
    // waited for must move a second period.
    const first = flick(1000, { axis: "x", direction: 1 });
    const resumeAt = first[first.length - 1].timeStamp + 600;
    const second = flick(resumeAt, { axis: "x", direction: 1, peak: 26 });
    expect(run([...first, ...second], WEEK)).toEqual([1, 1]);
  });

  it("moves one period when the fingers are still speeding up as the step fires", () => {
    // The bug Bruno reported: one swipe, two weeks.
    //
    // 42px goes by on the third event, while the hand is still accelerating —
    // a flick reaches its top speed around 100ms in, and the trigger is met
    // long before that. Everything after the third event is therefore a *rise*,
    // and a rise used to be the whole signal for "the fingers came back down
    // for a second swipe". The sixth event, 120px, cleared twice the 53px
    // recorded right after the step and paged a second week from one movement.
    //
    // The magnitudes are a 120px/frame flick with a six-frame ramp and a 6%
    // per frame decay. Anything from about 120px/frame up did this, which on a
    // 120Hz display means anything from about 60.
    const stream = replay(1000, "x", 1, [
      [3.3, 16],
      [13.3, 16],
      [30.0, 16], // 46.6px accumulated — the step fires here
      [53.3, 16], // still climbing
      [83.3, 16],
      [120.0, 16], // the top of the flick, and the old second week
      [112.8, 16],
      [106.0, 16],
      [99.7, 16],
      [93.7, 16],
      [88.1, 16],
      [82.8, 16],
      [77.8, 16],
      [73.1, 16],
    ]);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("moves one period when the tail arrives coalesced behind the step's own re-render", () => {
    // Firing a step re-renders a week of blocks, which blocks the main thread,
    // and WebKit answers a busy main thread by coalescing the wheel events it
    // could not deliver. Six frames of a decaying tail arrive as one event
    // carrying six frames of travel: a sixfold jump in magnitude, and no change
    // at all in how fast the fingers were moving.
    const stream = replay(1000, "x", 1, [
      [5, 16],
      [20, 16],
      [45, 16], // fires
      [66, 16],
      [80, 16],
      [286, 96], // six frames of tail, merged
      [70, 16],
      [66, 16],
      [62, 16],
      [58, 16],
    ]);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("moves one period on a 120Hz stream, where every delta is halved", () => {
    // A ProMotion MacBook samples twice as often and reports half as much each
    // time, so the ramp is spread over twice the events. Nothing about the hand
    // changed, and nothing about the answer may either.
    const magnitudes: (readonly [number, number])[] = [];
    for (let i = 1; i <= 12; i += 1) magnitudes.push([60 * (i / 12) ** 2, 8]);
    for (let m = 60 * 0.97; m > 0.1; m *= 0.97) magnitudes.push([m, 8]);
    expect(run(replay(1000, "x", 1, magnitudes), WEEK)).toEqual([1]);
  });

  it("moves one period for two flicks a tenth of a second apart", () => {
    // Two flicks 112ms apart, the second cancelling the first's momentum with
    // no quiet gap anywhere. Every rule that told these apart also told his real
    // swipe apart from itself eight times over, so they are one gesture now.
    const stream = replay(1000, "x", 1, [
      [3.3, 16],
      [13.3, 16],
      [30.0, 16],
      [53.3, 16],
      [83.3, 16],
      [120.0, 16],
      [3.3, 48], // fingers back on the glass, and indistinguishable from noise
      [13.3, 16],
      [30.0, 16],
      [53.3, 16],
      [83.3, 16],
      [120.0, 16],
      [112.8, 16],
      [106.0, 16],
    ]);
    expect(run(stream, WEEK)).toEqual([1]);
  });

  it("moves one period for the swipe he actually captured", () => {
    /*
     * Not a model of a trackpad — his, through `scripts/wheel-trace.html`, in
     * Safari, which is the engine the app's webview is. 144 events, 1313ms,
     * 2245px of travel, median gap 8ms, one swipe.
     *
     * Every synthetic fixture in this file agreed the recogniser was right
     * before this arrived. It moved **eight** periods, five of them inside the
     * first 121ms — before his fingers had left the glass. The finger half of
     * the stream is why: `41, 10, 24, 5, 31, 5, 25, 3, 20`, a factor of ten
     * between neighbours, because a finger is not a ramp and the sampling is
     * not even. Every rule that watched for the stream to wind down and climb
     * again read each of those troughs as a fresh swipe.
     *
     * The tail was always the easy half: a clean decay from 55 to 1 over about
     * 1.2 seconds, gapping by at most 66ms, which is what `TAIL_MS` clears.
     */
    const captured: [number, number][] = [
      [1, 0], [3, 8], [1, 7], [4, 2], [3, 4], [6, 4], [4, 4], [8, 4], [4, 4], [13, 5],
      [6, 7], [24, 1], [10, 4], [35, 4], [12, 4], [29, 5], [7, 4], [41, 4], [10, 6], [24, 2],
      [5, 4], [31, 5], [5, 4], [25, 4], [3, 4], [20, 4], [19, 9], [8, 4], [7, 4], [44, 13],
      [53, 9], [55, 7], [54, 8], [55, 9], [53, 10], [53, 6], [50, 8], [50, 9], [50, 9], [47, 8],
      [46, 8], [45, 9], [45, 9], [43, 7], [43, 13], [40, 4], [40, 9], [38, 7], [38, 9], [36, 8],
      [34, 9], [34, 8], [32, 8], [32, 9], [30, 9], [29, 7], [28, 8], [28, 9], [27, 14], [26, 2],
      [25, 8], [23, 9], [23, 9], [22, 8], [21, 8], [21, 8], [19, 9], [19, 8], [18, 8], [17, 9],
      [17, 9], [17, 7], [15, 8], [15, 9], [13, 9], [13, 8], [13, 8], [12, 9], [11, 9], [11, 7],
      [11, 8], [11, 9], [10, 9], [10, 7], [9, 9], [9, 9], [8, 8], [8, 8], [8, 8], [8, 10],
      [7, 7], [7, 8], [7, 8], [7, 9], [6, 9], [6, 7], [5, 8], [5, 10], [5, 8], [5, 8],
      [4, 8], [4, 9], [4, 9], [4, 7], [4, 12], [4, 5], [3, 9], [3, 7], [3, 8], [3, 10],
      [3, 8], [3, 8], [3, 8], [3, 9], [3, 8], [3, 8], [3, 8], [2, 10], [2, 7], [2, 8],
      [2, 8], [2, 10], [2, 8], [2, 8], [2, 8], [2, 9], [2, 8], [2, 8], [2, 8], [3, 18],
      [2, 17], [2, 18], [2, 15], [2, 17], [1, 16], [1, 17], [1, 17], [1, 16], [1, 17], [1, 16],
      [1, 34], [1, 33], [1, 34], [1, 66],
    ];
    expect(run(replay(1000, "x", 1, captured), WEEK)).toEqual([1]);
  });

  it("moves two periods for two of those, with a pause between them", () => {
    // The other half of the contract: a second swipe he waited for still pages.
    const one: [number, number][] = [[3, 8], [13, 8], [30, 8], [53, 8], [83, 8], [120, 8]];
    const first = replay(1000, "x", 1, one);
    const second = replay(first[first.length - 1].timeStamp + 600, "x", 1, one);
    expect(run([...first, ...second], WEEK)).toEqual([1, 1]);
  });

  it("moves one period for a slow deliberate drag with no momentum behind it", () => {
    // Below a velocity macOS spins up no momentum at all, so the stream simply
    // stops when the fingers do. Nothing here ever rises, so nothing re-arms.
    const magnitudes: (readonly [number, number])[] = Array.from({ length: 30 }, () => [6, 16]);
    expect(run(replay(1000, "x", 1, magnitudes), WEEK)).toEqual([1]);
  });

  it("takes over its own events so the webview cannot navigate back", () => {
    const stream = flick(1000, { axis: "x", direction: -1 });
    // Everything past the first couple of ambiguous events, tail included.
    expect(claimed(stream, WEEK)).toBeGreaterThan(stream.length - 4);
  });

  it("ignores a nudge too small to be deliberate", () => {
    expect(run(flick(1000, { axis: "x", direction: 1, peak: 8, momentum: false }), WEEK)).toEqual(
      [],
    );
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

  it("moves one month for four vertical flicks with no pause between them", () => {
    const stream: WheelSample[] = [];
    for (let index = 0; index < 4; index += 1) {
      stream.push(...flick(1000 + index * 230, { axis: "y", direction: 1 }).slice(0, 14));
    }
    expect(run(stream, MONTH)).toEqual([1]);
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

  it("is read as a wheel even while a trackpad flick's momentum window is open", () => {
    // A hand that leaves the trackpad for the mouse. The flick's fired gesture
    // holds its momentum lock for a while yet, and every notch that lands
    // inside it would otherwise be absorbed as tail.
    const swipe = flick(1000, { axis: "y", direction: 1 });
    const after = swipe[swipe.length - 1].timeStamp + 200;
    const spin = [0, 90, 180].map((offset) => notch(after + offset, -1));
    expect(run([...swipe, ...spin], MONTH)).toEqual([1, -1, -1, -1]);
  });

  it("rate-limits a spin fast enough to arrive at frame rate", () => {
    const spin = Array.from({ length: 30 }, (_, index) => notch(1000 + index * FRAME, 1));
    expect(run(spin, MONTH)).toEqual([1]);
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
    }));
    expect(run(pinch, MONTH)).toEqual([]);
    expect(run(pinch, WEEK)).toEqual([]);
    expect(claimed(pinch, MONTH)).toBe(0);
  });

  it("abandons a swipe it interrupts rather than letting it finish", () => {
    const before = [20, 18].map((deltaX, index) => ({
      deltaX,
      deltaY: 1,
      deltaMode: 0,
      timeStamp: 1000 + index * FRAME,
    }));
    const after = [10, 10].map((deltaX, index) => ({
      deltaX,
      deltaY: 1,
      deltaMode: 0,
      timeStamp: 1048 + index * FRAME,
    }));
    const pinch: WheelSample = {
      deltaX: 0,
      deltaY: -20,
      deltaMode: 0,
      timeStamp: 1032,
      ctrlKey: true,
    };

    // Uninterrupted the four events add to 58 and move a week.
    expect(run([...before, ...after], WEEK)).toEqual([1]);
    // With the pinch between them the first 38 is gone and 20 is not enough.
    expect(run([...before, pinch, ...after], WEEK)).toEqual([]);
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

  it("treats a gap longer than the idle window as a new gesture", () => {
    // A flick, then its tail truncated, then a second flick a full second
    // later: two gestures by the clock alone.
    const stream = [
      ...flick(1000, { axis: "x", direction: 1 }).slice(0, 10),
      ...flick(3000, { axis: "x", direction: -1 }).slice(0, 10),
    ];
    expect(run(stream, WEEK)).toEqual([1, -1]);
  });
});
