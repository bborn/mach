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

  it("moves four periods for four quick flicks", () => {
    // Each flick is cut short by the next one: the fingers land again ~230ms
    // in, macOS cancels the momentum, and a fresh ramp begins. That is the
    // stream a person paging forward quickly actually produces.
    const stream: WheelSample[] = [];
    for (let index = 0; index < 4; index += 1) {
      stream.push(...flick(1000 + index * 230, { axis: "x", direction: 1 }).slice(0, 14));
    }
    expect(run(stream, WEEK)).toEqual([1, 1, 1, 1]);
  });

  it("moves four periods even when the flicks run together with no quiet gap", () => {
    // The worst case for the idle timer: the next flick starts before the last
    // tail has finished, so every event in the stream is under 130ms from its
    // neighbour and the whole thing is one gesture by that measure. Only the
    // rise out of the decaying floor separates them.
    const stream: WheelSample[] = [];
    let clock = 1000;
    for (let index = 0; index < 4; index += 1) {
      const one = flick(clock, { axis: "x", direction: 1 }).slice(0, 20);
      stream.push(...one);
      clock = one[one.length - 1].timeStamp + FRAME;
    }
    expect(run(stream, WEEK)).toEqual([1, 1, 1, 1]);
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
    // The tail runs out, a quarter second of nothing, then a modest swipe. The
    // pause is shorter than the one that clears a fired gesture, so this leans
    // entirely on the rise out of the momentum floor.
    const first = flick(1000, { axis: "x", direction: 1 });
    const resumeAt = first[first.length - 1].timeStamp + 250;
    const second = flick(resumeAt, { axis: "x", direction: 1, peak: 26 });
    expect(run([...first, ...second], WEEK)).toEqual([1, 1]);
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

  it("moves four months for four quick vertical flicks", () => {
    const stream: WheelSample[] = [];
    for (let index = 0; index < 4; index += 1) {
      stream.push(...flick(1000 + index * 230, { axis: "y", direction: 1 }).slice(0, 14));
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
