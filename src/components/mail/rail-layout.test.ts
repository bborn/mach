/**
 * The rail's width, asked without a window.
 *
 * The property that matters is the one `clampComposerHeight` has: a value that
 * has been through this once does not move if it goes through again, so the
 * width that was stored on quit is the width that comes back on launch.
 */

import { describe, expect, it } from "vitest";
import { RAIL_WIDTH_BOUNDS, parseSession } from "@/lib/prefs";
import { DEFAULT_RAIL_WIDTH, clampRailWidth } from "./rail-layout";

const { min, max } = RAIL_WIDTH_BOUNDS;

describe("clampRailWidth", () => {
  it("keeps a width that fits", () => {
    expect(clampRailWidth(DEFAULT_RAIL_WIDTH)).toBe(DEFAULT_RAIL_WIDTH);
    expect(clampRailWidth(240)).toBe(240);
  });

  it("keeps the ends of the range", () => {
    expect(clampRailWidth(min)).toBe(min);
    expect(clampRailWidth(max)).toBe(max);
  });

  it("holds a width dragged past the floor to the floor", () => {
    expect(clampRailWidth(min - 1)).toBe(min);
    expect(clampRailWidth(40)).toBe(min);
    expect(clampRailWidth(0)).toBe(min);
    expect(clampRailWidth(-500)).toBe(min);
  });

  it("holds a width dragged past the ceiling to the ceiling", () => {
    expect(clampRailWidth(max + 1)).toBe(max);
    expect(clampRailWidth(4000)).toBe(max);
  });

  it("rounds", () => {
    expect(clampRailWidth(207.4)).toBe(207);
    expect(clampRailWidth(207.6)).toBe(208);
  });

  it("lands on the default when the stored value is nonsense", () => {
    expect(clampRailWidth(Number.NaN)).toBe(DEFAULT_RAIL_WIDTH);
    expect(clampRailWidth(Number.POSITIVE_INFINITY)).toBe(DEFAULT_RAIL_WIDTH);
    expect(clampRailWidth(Number.NEGATIVE_INFINITY)).toBe(DEFAULT_RAIL_WIDTH);
  });

  it("is idempotent, so restoring what was stored cannot drift", () => {
    for (const width of [min - 80, min, 200, DEFAULT_RAIL_WIDTH, max, max + 500]) {
      const once = clampRailWidth(width);
      expect(clampRailWidth(once)).toBe(once);
    }
  });
});

describe("the double-click reset", () => {
  it("goes back to the width the rail shipped at", () => {
    expect(clampRailWidth(DEFAULT_RAIL_WIDTH)).toBe(DEFAULT_RAIL_WIDTH);
  });

  it("returns a width the bounds allow, from either end of the range", () => {
    expect(clampRailWidth(DEFAULT_RAIL_WIDTH)).toBeGreaterThanOrEqual(min);
    expect(clampRailWidth(DEFAULT_RAIL_WIDTH)).toBeLessThanOrEqual(max);
    for (const from of [min, max]) {
      expect(clampRailWidth(from)).not.toBe(DEFAULT_RAIL_WIDTH);
      expect(clampRailWidth(DEFAULT_RAIL_WIDTH)).toBe(DEFAULT_RAIL_WIDTH);
    }
  });
});

describe("the bounds themselves", () => {
  it("clear the 88px macOS reserves for the traffic lights, even at the floor", () => {
    // `pl-[5.5rem]` in `chrome/TitleBar.tsx`, in the pixels this file works in.
    expect(min).toBeGreaterThan(88);
  });

  it("leave the conversation list its own minimum inside the narrowest window", () => {
    // `minWidth: 960` in `tauri.conf.json`, `LIST_WIDTH_BOUNDS.min` is 280.
    expect(max + 280).toBeLessThanOrEqual(960);
  });

  it("start at a default inside the range", () => {
    expect(DEFAULT_RAIL_WIDTH).toBeGreaterThan(min);
    expect(DEFAULT_RAIL_WIDTH).toBeLessThan(max);
  });
});

describe("what the store may hand back", () => {
  it("cannot put a width on screen that a drag could not produce", () => {
    expect(parseSession({ railWidth: 4000 }).railWidth).toBe(max);
    expect(parseSession({ railWidth: 10 }).railWidth).toBe(min);
    expect(parseSession({ railWidth: 240.4 }).railWidth).toBe(240);
  });

  it("says nothing at all rather than something wrong", () => {
    expect(parseSession({ railWidth: Number.NaN }).railWidth).toBeUndefined();
    expect(parseSession({ railWidth: "wide" }).railWidth).toBeUndefined();
    expect(parseSession({}).railWidth).toBeUndefined();
  });

  it("agrees with the clamp the rail applies at render", () => {
    for (const stored of [10, min, 200, max, 4000]) {
      const restored = parseSession({ railWidth: stored }).railWidth!;
      expect(clampRailWidth(restored)).toBe(restored);
    }
  });
});
