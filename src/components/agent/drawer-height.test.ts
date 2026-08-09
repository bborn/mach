/**
 * The two limits on the drawer's height, and what happens where they disagree.
 *
 * The interesting cases are all the ones a drag cannot produce: a height
 * restored from a store written on a bigger display, a window shorter than the
 * drawer's own minimum, a row somebody hand-edited to a string of nonsense.
 */

import { describe, expect, it } from "vitest";
import { AGENT_DRAWER_HEIGHT_BOUNDS } from "@/lib/prefs";
import {
  DEFAULT_AGENT_DRAWER_HEIGHT,
  RESERVED_WINDOW_CHROME,
  clampDrawerHeight,
} from "./drawer-height";

const { min, max } = AGENT_DRAWER_HEIGHT_BOUNDS;
const TALL = 1200;

describe("clampDrawerHeight", () => {
  it("leaves a height that fits alone", () => {
    expect(clampDrawerHeight(320, TALL)).toBe(320);
    expect(clampDrawerHeight(min, TALL)).toBe(min);
  });

  it("holds to the bounds however tall the window is", () => {
    expect(clampDrawerHeight(5000, 4000)).toBe(max);
    expect(clampDrawerHeight(-40, TALL)).toBe(min);
    expect(clampDrawerHeight(0, TALL)).toBe(min);
  });

  it("leaves the window something to be a window with", () => {
    const viewport = 700;
    expect(clampDrawerHeight(690, viewport)).toBe(viewport - RESERVED_WINDOW_CHROME);
    // …which is the whole point: the app behind it keeps a usable strip.
    expect(clampDrawerHeight(690, viewport)).toBeLessThan(viewport);
  });

  it("keeps the minimum when the window cannot even afford that", () => {
    // A drawer of two pixels is not a smaller drawer, it is a broken one. The
    // list gives way instead.
    expect(clampDrawerHeight(300, 200)).toBe(min);
    expect(clampDrawerHeight(10, 200)).toBe(min);
  });

  it("treats an unmeasured window as no window limit at all", () => {
    // Server render, or a test: the bounds are the only honest answer, and
    // clamping to zero would paint a sliver for one frame.
    expect(clampDrawerHeight(500, 0)).toBe(500);
    expect(clampDrawerHeight(5000, 0)).toBe(max);
  });

  it("rounds, because a height is a whole number of pixels", () => {
    expect(clampDrawerHeight(320.4, TALL)).toBe(320);
    expect(clampDrawerHeight(320.6, TALL)).toBe(321);
  });

  it("falls back to the default for a value that is not a number", () => {
    expect(clampDrawerHeight(Number.NaN, TALL)).toBe(DEFAULT_AGENT_DRAWER_HEIGHT);
    expect(clampDrawerHeight(Number.POSITIVE_INFINITY, TALL)).toBe(DEFAULT_AGENT_DRAWER_HEIGHT);
    // Even then it may not overflow a short window.
    expect(clampDrawerHeight(Number.NaN, 300)).toBe(min);
  });

  it("is idempotent, so restoring what it stored changes nothing", () => {
    for (const height of [100, 320, 700, 5000]) {
      const once = clampDrawerHeight(height, 900);
      expect(clampDrawerHeight(once, 900)).toBe(once);
    }
  });
});
