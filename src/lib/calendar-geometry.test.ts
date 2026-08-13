import { describe, expect, it } from "vitest";
import {
  CASCADE_STEP_MIN,
  HOUR_HEIGHT,
  MIN_BLOCK_HEIGHT,
  NARROW_BLOCK_WIDTH,
  READABLE_COLUMN_WIDTH,
  blockHeight,
  blockPlan,
  blockTier,
  clamp,
  clusterPlan,
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

  /*
   * The three states the grid was in when it opened on 1 AM.
   *
   * None of them is an arithmetic bug — the function answers sensibly to all
   * three. The defect was in believing the *write* that carried the answer, and
   * the assertions below are the reason to say that out loud: at 20:02 this
   * function cannot return anything near midnight, so a grid showing midnight
   * was never given the number this computed.
   */
  it("takes the floor from the working day rather than a fixed 06:30", () => {
    // Nine to five, half an hour of air above it. A day that starts at ten
    // should not open on half past six.
    const eightAm = DAY_START + 8 * HOUR;
    expect(nowScrollTop(eightAm, DAY_START, viewport, 8.5)).toBe(8.5 * HOUR_HEIGHT);
    expect(nowScrollTop(eightAm, DAY_START, viewport, 0)).toBe(8 * HOUR_HEIGHT - 125);
  });

  it("prefers the floor when the day's bottom would put it above it", () => {
    // The evening case, with the default nine o'clock working day: 20:02 is
    // 961px down, the viewport can only be scrolled to 377, and the floor is
    // 408. `clamp` prefers its minimum when the range inverts, so the grid
    // opens on the working day and runs to midnight — never above 08:30.
    const evening = DAY_START + 20 * HOUR + 2 * MINUTE;
    expect(clamp(961, 408, 377)).toBe(408);
    expect(nowScrollTop(evening, DAY_START, 775, 8.5)).toBe(8.5 * HOUR_HEIGHT);
  });

  it("answers plausibly for a viewport of zero, which is the trap", () => {
    // A grid that has not been laid out reports `clientHeight: 0`, and this
    // function has no way to know that: a quarter of nothing is nothing, so it
    // returns now's own offset and looks entirely reasonable doing it. The
    // caller cannot tell a good answer from a useless one here, which is why
    // `TimeGrid` checks that the *element* can be scrolled before it writes,
    // and asks again when it can.
    const evening = DAY_START + 20 * HOUR + 2 * MINUTE;
    expect(nowScrollTop(evening, DAY_START, 0, 8.5)).toBeCloseTo(20.0333 * HOUR_HEIGHT, 1);
    expect(nowScrollTop(evening, DAY_START, 0, 8.5)).toBeGreaterThan(0);
  });
});

describe("clamp", () => {
  it("prefers the minimum when the range inverts", () => {
    // Relied on twice: by the cascade, where every step is the floor and the
    // top block takes what is left, and by `nowScrollTop`, where a working day
    // that starts after the last scrollable pixel still wins.
    expect(clamp(50, 10, 0)).toBe(10);
    expect(clamp(-50, 10, 0)).toBe(10);
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
    const heights = [11, 20, 24, 34, 48, 96];
    const sizes = new Set([
      ...heights.map((h) => blockPlan(h).fontPx),
      ...heights.map((h) => blockPlan(h).timeFontPx),
    ]);
    expect([...sizes].sort()).toEqual([11, 12]);
  });

  it("draws the time one step below the title", () => {
    const plan = blockPlan(48);
    expect(plan.timeFontPx).toBeLessThan(plan.fontPx);
  });

  /*
   * Google's rule, and the cheapest width there is: a block's position on the
   * grid already says when it is, so the time is what a narrow block gives up.
   */
  it("drops the time and the location once the block is narrow", () => {
    const roomy = blockPlan(60, { hasLocation: true, width: NARROW_BLOCK_WIDTH });
    expect(roomy.showTime).toBe(true);
    expect(roomy.showLocation).toBe(true);

    const narrow = blockPlan(60, { hasLocation: true, width: NARROW_BLOCK_WIDTH - 1 });
    expect(narrow.showTime).toBe(false);
    expect(narrow.showLocation).toBe(false);
  });

  it("spends the line the time gave up on a third line of title", () => {
    expect(blockPlan(60, { width: 200 }).titleLines).toBe(2);
    expect(blockPlan(60, { width: 51 }).titleLines).toBe(3);
    // Only a full block ever wraps, narrow or not.
    expect(blockPlan(24, { width: 51 }).titleLines).toBe(1);
  });

  it("assumes room when no width is given", () => {
    expect(blockPlan(60, { hasLocation: true }).showTime).toBe(true);
    expect(blockPlan(60, { hasLocation: true }).showLocation).toBe(true);
  });

  it("lets only the sliver overflow its bounds, at a constant 15px line", () => {
    expect(blockPlan(11).overflow).toBe(true);
    expect(blockPlan(24).overflow).toBe(false);
    expect(blockPlan(11).lineHeightPx).toBe(15);
    expect(blockPlan(96).lineHeightPx).toBe(15);
  });
});

describe("visibleColumns", () => {
  it("leaves an ordinary cluster alone", () => {
    expect(visibleColumns(3, 156)).toBe(3);
    expect(visibleColumns(1, 20)).toBe(1);
  });

  /*
   * The old rule was "how many 40px columns fit", which capped a 156px column at
   * three events and rendered those three at 52px each: the unreadable cluster
   * the dogfood pass measured. The rule now counts 18px strips beside something
   * worth reading, so the same width carries five events and the top one is
   * legible.
   */
  it("counts cascade strips, not columns", () => {
    expect(visibleColumns(5, 156)).toBe(5);
    expect(visibleColumns(8, 100)).toBe(4);
  });

  it("spends the last column on a +N chip once the strips stop fitting", () => {
    expect(visibleColumns(12, 156)).toBe(7);
  });

  it("never caps two — half a narrow column is still an event you can click", () => {
    expect(visibleColumns(2, 44)).toBe(2);
  });

  it("always keeps at least one column", () => {
    expect(visibleColumns(4, 10)).toBe(1);
  });
});

describe("clusterPlan", () => {
  it("divides one and two events, whatever the width", () => {
    expect(clusterPlan(1, 155).mode).toBe("divide");
    expect(clusterPlan(2, 60).mode).toBe("divide");
  });

  it("divides three events when a third of the column is still readable", () => {
    expect(clusterPlan(3, 3 * READABLE_COLUMN_WIDTH).mode).toBe("divide");
  });

  /*
   * The measured defect: a 1440px window gives a week column 155px, and three
   * concurrent events divided that into 51px each — about four characters.
   */
  it("cascades three events in a week column", () => {
    const plan = clusterPlan(3, 155);
    expect(plan.mode).toBe("cascade");
    // Two strips and a readable block on top, and the whole width spent.
    expect(plan.step).toBeCloseTo((155 - 76) / 2, 5);
    expect(155 - 2 * plan.step).toBeGreaterThanOrEqual(READABLE_COLUMN_WIDTH);
  });

  it("keeps every strip at or above the floor as the cluster deepens", () => {
    for (const columns of [3, 4, 5, 6, 7]) {
      const plan = clusterPlan(columns, 155);
      expect(plan.mode).toBe("cascade");
      expect(plan.step).toBeGreaterThanOrEqual(CASCADE_STEP_MIN);
    }
  });

  it("gives the floor to the strips and the remainder to the top when it cannot have both", () => {
    // A narrow window: 97px of usable column, three events.
    const plan = clusterPlan(3, 97);
    expect(plan.step).toBe(CASCADE_STEP_MIN);
    expect(97 - 2 * plan.step).toBeGreaterThan(0);
  });

  it("never offsets a block by more than its even share", () => {
    for (const width of [60, 97, 120, 155, 200]) {
      for (const columns of [3, 4, 5]) {
        const plan = clusterPlan(columns, width);
        if (plan.mode === "cascade") expect(plan.step).toBeLessThanOrEqual(width);
      }
    }
  });
});
