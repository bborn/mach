import { describe, expect, it } from "vitest";
import {
  HOUR_HEIGHT,
  MIN_BLOCK_HEIGHT,
  blockHeight,
  blockPlan,
  blockTier,
  nowScrollTop,
  offsetForTime,
  snapTime,
  snapTimeDown,
  timeForOffset,
  visibleColumns,
} from "./calendar-geometry";
import { HOUR, MINUTE } from "./time";

const DAY_START = new Date(2026, 7, 3, 0, 0, 0, 0).getTime();

describe("the measured grid", () => {
  it("is 48px an hour, which is what makes 30 minutes 24px", () => {
    expect(HOUR_HEIGHT).toBe(48);
    expect(blockHeight(30 * MINUTE)).toBe(23); // 24 less the 1px stacking gap
  });

  it("floors a block at 11px, not 17", () => {
    // The whole point: a 15-minute event is 12px of duration and renders at
    // 11px, not at 17 — at 17 it would look the same size as a 21-minute one.
    expect(MIN_BLOCK_HEIGHT).toBe(11);
    expect(blockHeight(15 * MINUTE)).toBe(11);
    expect(blockHeight(5 * MINUTE)).toBe(11);
  });

  it("keeps longer blocks exactly proportional", () => {
    expect(blockHeight(HOUR)).toBe(47);
    expect(blockHeight(2 * HOUR)).toBe(95);
    expect(blockHeight(45 * MINUTE)).toBe(35);
  });

  it("maps time to offset and back", () => {
    const noon = DAY_START + 12 * HOUR;
    expect(offsetForTime(noon, DAY_START)).toBe(576);
    expect(timeForOffset(576, DAY_START)).toBe(noon);
  });

  it("snaps to the quarter hour", () => {
    const t = DAY_START + 9 * HOUR + 8 * MINUTE;
    expect(snapTime(t) - DAY_START).toBe(9 * HOUR + 15 * MINUTE);
    expect(snapTime(DAY_START + 9 * HOUR + 7 * MINUTE) - DAY_START).toBe(9 * HOUR);
    expect(snapTimeDown(t) - DAY_START).toBe(9 * HOUR);
  });
});

describe("nowScrollTop", () => {
  const viewport = 500;

  it("puts now a quarter of the way down the viewport", () => {
    const now = DAY_START + 14 * HOUR; // 2pm → 672px
    expect(nowScrollTop(now, DAY_START, viewport)).toBe(672 - 125);
  });

  it("never opens earlier than 06:30", () => {
    const now = DAY_START + 7 * HOUR;
    expect(nowScrollTop(now, DAY_START, viewport)).toBe(6.5 * HOUR_HEIGHT);
  });

  it("never overscrolls the bottom of the day", () => {
    const now = DAY_START + 23 * HOUR + 45 * MINUTE;
    expect(nowScrollTop(now, DAY_START, viewport)).toBe(24 * HOUR_HEIGHT - viewport);
  });

  it("survives a viewport taller than the grid", () => {
    expect(nowScrollTop(DAY_START + 12 * HOUR, DAY_START, 2000)).toBe(6.5 * HOUR_HEIGHT);
  });
});

describe("progressive degradation", () => {
  it("drops exactly one thing per step", () => {
    expect(blockTier(47)).toBe("full");
    expect(blockTier(46)).toBe("twoLine");
    expect(blockTier(33)).toBe("twoLine");
    expect(blockTier(32)).toBe("oneLine");
    expect(blockTier(23)).toBe("oneLine");
    expect(blockTier(22)).toBe("sliver");
    expect(blockTier(11)).toBe("sliver");
  });

  /**
   * The ladder is specified in §5 by *duration*, and the block a duration
   * renders as is a pixel shorter than its nominal height. Pinning the two
   * functions together is the only way that composition stays honest — pinned
   * separately, each looked right while a 30-minute meeting came out a sliver.
   */
  it("puts the brief's durations in the brief's tiers", () => {
    expect(blockTier(blockHeight(2 * HOUR))).toBe("full");
    expect(blockTier(blockHeight(HOUR))).toBe("full");
    expect(blockTier(blockHeight(45 * MINUTE))).toBe("twoLine");
    expect(blockTier(blockHeight(30 * MINUTE))).toBe("oneLine");
    expect(blockTier(blockHeight(15 * MINUTE))).toBe("sliver");
    expect(blockTier(blockHeight(5 * MINUTE))).toBe("sliver");
  });

  it("shows location only in a full block, and only when there is one", () => {
    expect(blockPlan(60, { hasLocation: true }).showLocation).toBe(true);
    expect(blockPlan(60, { hasLocation: false }).showLocation).toBe(false);
    expect(blockPlan(40, { hasLocation: true }).showLocation).toBe(false);
  });

  it("joins the time onto the title's line once the block is under 34px", () => {
    expect(blockPlan(60).inlineTime).toBe(false);
    expect(blockPlan(36).inlineTime).toBe(false);
    expect(blockPlan(24).inlineTime).toBe(true);
    expect(blockPlan(11).inlineTime).toBe(true);
  });

  it("wraps the title only in a full block", () => {
    expect(blockPlan(47).wrapTitle).toBe(true);
    expect(blockPlan(46).wrapTitle).toBe(false);
  });

  it("uses two type sizes and no more", () => {
    const sizes = new Set([11, 20, 24, 34, 48, 96].map((h) => blockPlan(h).fontPx));
    expect([...sizes].sort()).toEqual([11, 12]);
  });

  it("lets only the sliver overflow its bounds, at a constant 15px line", () => {
    expect(blockPlan(11).overflow).toBe(true);
    expect(blockPlan(24).overflow).toBe(false);
    expect(blockPlan(11).lineHeightPx).toBe(15);
    expect(blockPlan(96).lineHeightPx).toBe(15);
  });
});

describe("visibleColumns", () => {
  it("leaves a cluster alone when every column clears 40px", () => {
    expect(visibleColumns(3, 156)).toBe(3);
    expect(visibleColumns(1, 20)).toBe(1);
  });

  it("caps the columns rather than rendering a 12px sliver", () => {
    expect(visibleColumns(5, 156)).toBe(3);
    expect(visibleColumns(8, 100)).toBe(2);
  });

  it("always keeps at least one column", () => {
    expect(visibleColumns(4, 10)).toBe(1);
  });
});
