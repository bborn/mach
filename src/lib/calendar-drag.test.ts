import { describe, expect, it } from "vitest";
import {
  MIN_EVENT_MINUTES,
  clampToRange,
  createResult,
  crossesDay,
  dayIndexAt,
  dragLabel,
  isCopyDrag,
  isDrag,
  moveResult,
  msToPixels,
  nudge,
  pixelsToMs,
  resizeResult,
  shiftDays,
} from "./calendar-drag";
import { HOUR_HEIGHT } from "./calendar-geometry";
import { DAY, HOUR, MINUTE } from "./time";

/** 2026-08-07 09:00 local — a Friday, well away from any DST boundary. */
const NINE = new Date(2026, 7, 7, 9, 0, 0, 0).getTime();

function origin(start = NINE, minutes = 60) {
  return {
    start,
    end: start + minutes * MINUTE,
    dayStart: new Date(2026, 7, 7).getTime(),
  };
}

describe("pixels and time", () => {
  it("converts a whole hour row to an hour", () => {
    expect(pixelsToMs(HOUR_HEIGHT)).toBe(HOUR);
    expect(msToPixels(HOUR)).toBe(HOUR_HEIGHT);
  });

  it("is the exact inverse of itself", () => {
    expect(msToPixels(pixelsToMs(37))).toBeCloseTo(37);
  });
});

describe("moveResult", () => {
  it("preserves duration", () => {
    const moved = moveResult(origin(NINE, 45), 96, 0);
    expect(moved.end - moved.start).toBe(45 * MINUTE);
  });

  it("snaps the result to the quarter hour", () => {
    // 128px is exactly two hours; 133px is 2h05m, which snaps back to 2h00.
    expect(moveResult(origin(), 133, 0).start).toBe(NINE + 2 * HOUR);
    // 144px is 2h15m and stays there.
    expect(moveResult(origin(), 144, 0).start).toBe(NINE + 2 * HOUR + 15 * MINUTE);
  });

  it("snaps the outcome, not each increment, so a slow drag does not creep", () => {
    // Ten 4px steps and one 40px step must land in the same place.
    const steps = Array.from({ length: 10 }, (_, i) => moveResult(origin(), (i + 1) * 4, 0));
    expect(steps[steps.length - 1].start).toBe(moveResult(origin(), 40, 0).start);
  });

  it("moves upwards as readily as downwards", () => {
    expect(moveResult(origin(), -HOUR_HEIGHT, 0).start).toBe(NINE - HOUR);
  });

  it("crosses a day boundary by whole calendar days, keeping the wall clock", () => {
    const moved = moveResult(origin(), 0, 3);
    expect(new Date(moved.start).getHours()).toBe(9);
    expect(new Date(moved.start).getDate()).toBe(10);
    expect(moved.end - moved.start).toBe(HOUR);
  });

  it("combines a vertical drag with a day change", () => {
    const moved = moveResult(origin(), HOUR_HEIGHT * 2, -1);
    expect(new Date(moved.start).getDate()).toBe(6);
    expect(new Date(moved.start).getHours()).toBe(11);
  });
});

describe("shiftDays across a DST boundary", () => {
  // US DST ends 2026-11-01. A plain `+ n * DAY` would land an hour out here,
  // which is exactly the bug this function exists to avoid.
  const beforeFallBack = new Date(2026, 9, 31, 9, 0, 0, 0).getTime();

  it("keeps the wall clock rather than the elapsed milliseconds", () => {
    const after = shiftDays(beforeFallBack, 2);
    expect(new Date(after).getHours()).toBe(9);
    expect(new Date(after).getDate()).toBe(2);
  });

  it("is the identity for zero", () => {
    expect(shiftDays(beforeFallBack, 0)).toBe(beforeFallBack);
  });
});

describe("resizeResult", () => {
  it("moves the bottom edge and leaves the top alone", () => {
    const resized = resizeResult(origin(), "end", HOUR_HEIGHT);
    expect(resized.start).toBe(NINE);
    expect(resized.end).toBe(NINE + 2 * HOUR);
  });

  it("moves the top edge and leaves the bottom alone", () => {
    const resized = resizeResult(origin(), "start", -HOUR_HEIGHT);
    expect(resized.start).toBe(NINE - HOUR);
    expect(resized.end).toBe(NINE + HOUR);
  });

  it("refuses to shrink the bottom edge past the minimum", () => {
    const resized = resizeResult(origin(), "end", -1000);
    expect(resized.start).toBe(NINE);
    expect(resized.end).toBe(NINE + MIN_EVENT_MINUTES * MINUTE);
  });

  it("refuses to drag the top edge past the bottom", () => {
    const resized = resizeResult(origin(), "start", 1000);
    expect(resized.end).toBe(NINE + HOUR);
    expect(resized.start).toBe(NINE + HOUR - MIN_EVENT_MINUTES * MINUTE);
  });

  it("snaps to the quarter hour like a move does", () => {
    expect(resizeResult(origin(), "end", 5).end).toBe(NINE + HOUR);
    expect(resizeResult(origin(), "end", 10).end).toBe(NINE + HOUR + 15 * MINUTE);
  });
});

describe("createResult", () => {
  it("makes a default-length event for a press with no travel", () => {
    expect(createResult(NINE, null)).toEqual({ start: NINE, end: NINE + 30 * MINUTE });
    expect(createResult(NINE, NINE)).toEqual({ start: NINE, end: NINE + 30 * MINUTE });
  });

  it("grows downwards from the anchor", () => {
    expect(createResult(NINE, NINE + 90 * MINUTE)).toEqual({
      start: NINE,
      end: NINE + 90 * MINUTE,
    });
  });

  it("treats an upward drag as the same gesture", () => {
    expect(createResult(NINE, NINE - 90 * MINUTE)).toEqual({
      start: NINE - 90 * MINUTE,
      end: NINE,
    });
  });
});

describe("nudge — the keyboard's version of a drag", () => {
  it("slides both ends by one snap step", () => {
    const moved = nudge(origin(), { kind: "move", axis: "time", steps: 1 });
    expect(moved.start).toBe(NINE + 15 * MINUTE);
    expect(moved.end - moved.start).toBe(HOUR);
  });

  it("goes backwards on a negative step", () => {
    expect(nudge(origin(), { kind: "move", axis: "time", steps: -2 }).start).toBe(
      NINE - 30 * MINUTE,
    );
  });

  it("moves whole days without touching the wall clock", () => {
    const moved = nudge(origin(), { kind: "move", axis: "day", days: 1 });
    expect(new Date(moved.start).getHours()).toBe(9);
    expect(new Date(moved.start).getDate()).toBe(8);
  });

  it("extends and shrinks the end edge", () => {
    expect(nudge(origin(), { kind: "resize", edge: "end", steps: 2 }).end).toBe(
      NINE + 90 * MINUTE,
    );
    expect(nudge(origin(), { kind: "resize", edge: "end", steps: -2 }).end).toBe(
      NINE + 30 * MINUTE,
    );
  });

  it("honours the minimum duration from the keyboard too", () => {
    const squashed = nudge(origin(), { kind: "resize", edge: "end", steps: -10 });
    expect(squashed.end - squashed.start).toBe(MIN_EVENT_MINUTES * MINUTE);
  });

  it("moves the start edge without moving the end", () => {
    const stretched = nudge(origin(), { kind: "resize", edge: "start", steps: -1 });
    expect(stretched.start).toBe(NINE - 15 * MINUTE);
    expect(stretched.end).toBe(NINE + HOUR);
  });
});

describe("dayIndexAt", () => {
  it("finds the column under the pointer", () => {
    expect(dayIndexAt(0, 0, 700, 7)).toBe(0);
    expect(dayIndexAt(350, 0, 700, 7)).toBe(3);
    expect(dayIndexAt(699, 0, 700, 7)).toBe(6);
  });

  it("accounts for the grid's own left offset", () => {
    expect(dayIndexAt(156, 56, 700, 7)).toBe(1);
  });

  it("clamps rather than inventing a column that is not rendered", () => {
    expect(dayIndexAt(-500, 0, 700, 7)).toBe(0);
    expect(dayIndexAt(5000, 0, 700, 7)).toBe(6);
  });

  it("is safe on a grid with no measured width yet", () => {
    expect(dayIndexAt(100, 0, 0, 7)).toBe(0);
  });
});

describe("feedback helpers", () => {
  it("labels a drag with its times", () => {
    expect(dragLabel({ start: NINE, end: NINE + 45 * MINUTE })).toBe("9am – 9:45am");
  });

  it("adds the day once the drag has left the column it started in", () => {
    expect(dragLabel({ start: NINE, end: NINE + HOUR }, { showDate: true })).toBe(
      "Fri 7 · 9am – 10am",
    );
  });

  it("says all day rather than a time range", () => {
    expect(dragLabel({ start: NINE, end: NINE + HOUR }, { allDay: true })).toBe("All day");
  });

  it("knows when a drag has crossed into another day", () => {
    const day = new Date(2026, 7, 7).getTime();
    expect(crossesDay({ start: NINE, end: NINE + HOUR }, day)).toBe(false);
    expect(crossesDay({ start: NINE + DAY, end: NINE + DAY + HOUR }, day)).toBe(true);
  });

  it("does not call a 2px twitch a drag", () => {
    expect(isDrag(0, 2)).toBe(false);
    expect(isDrag(0, 3)).toBe(true);
    expect(isDrag(-4, 0)).toBe(true);
  });
});

describe("isCopyDrag", () => {
  it("copies when alt is held on a move", () => {
    expect(isCopyDrag("move", true)).toBe(true);
    expect(isCopyDrag("move", false)).toBe(false);
  });

  it("ignores alt on a resize, which has nothing to copy", () => {
    expect(isCopyDrag("resize", true)).toBe(false);
  });
});

describe("clampToRange", () => {
  const first = new Date(2026, 7, 3).getTime();
  const past = new Date(2026, 7, 10).getTime();

  it("leaves an event inside the range alone", () => {
    const inside = { start: NINE, end: NINE + HOUR };
    expect(clampToRange(inside, first, past)).toEqual(inside);
  });

  it("pulls an event dragged off the front back to the first day", () => {
    const clamped = clampToRange({ start: first - DAY, end: first - DAY + HOUR }, first, past);
    expect(clamped.start).toBe(first);
    expect(clamped.end - clamped.start).toBe(HOUR);
  });

  it("pulls an event dragged off the end back to the last day", () => {
    const clamped = clampToRange({ start: past + DAY, end: past + DAY + HOUR }, first, past);
    expect(clamped.start).toBe(past - DAY);
  });
});
