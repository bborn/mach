// @vitest-environment jsdom

/**
 * The hit target of a short block, and why it is not the same thing as its
 * height.
 *
 * The report was "the half hour event blocks are too tiny — they need a min
 * height that's reasonable to click on". The height is not negotiable: 48px an
 * hour makes a 30-minute meeting 23px and a 15-minute one 11px, and §1 of the
 * brief exists to stop those numbers being rounded up into a lie about the
 * grid's own scale. So the *drawing* stays, and a transparent skirt above and
 * below the painted body brings the target to `MIN_HIT_HEIGHT`.
 *
 * That trade only holds if the skirt can never take a click that belonged to
 * somebody else, which is the thing this file is really about. The rule is one
 * line — every skirt sits at `Z_EVENT_HIT`, under every painted block — and the
 * consequence is that a 09:00 block reaching down into 09:30 passes *beneath*
 * the 09:30 block's body.
 *
 * `resolveHit` below is CSS's own answer written out: among the boxes covering
 * a point, the highest `z-index` wins and DOM order breaks the tie. It is a
 * model rather than the implementation — the browser does the real work — but
 * it is a faithful one for positioned siblings in a single stacking context,
 * which is what the day column renders.
 *
 * jsdom has no layout, so every box here is read off the inline styles the grid
 * actually wrote. That is the whole of what decides this: the geometry is
 * stated in pixels, not measured.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  HOUR_HEIGHT,
  MAX_HIT_OVERHANG,
  MIN_HIT_HEIGHT,
  Z_EVENT,
  Z_EVENT_HIT,
  blockHeight,
  hitSkirt,
} from "@/lib/calendar-geometry";
import type { MergedEvent } from "@/lib/calendar-merge";
import type { CalendarEvent, EventId } from "@/types";
import { MINUTE } from "@/lib/time";
import { TimeGrid } from "./TimeGrid";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

class FakeResizeObserver {
  constructor(private readonly callback: (entries: ResizeObserverEntry[]) => void) {}
  observe(target: Element) {
    this.callback([
      { target, contentRect: new DOMRect(0, 0, 200, 500) } as ResizeObserverEntry,
    ]);
  }
  disconnect() {}
}

const DAY = new Date(2026, 7, 12);
const MIDNIGHT = DAY.getTime();

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.useFakeTimers();
  vi.setSystemTime(new Date(2026, 7, 12, 14, 0, 0, 0));
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  globalThis.IS_REACT_ACT_ENVIRONMENT = undefined;
});

/** An event at `hour:minute` for `minutes`, on the one day the grid renders. */
function event(id: number, at: number, minutes: number, title = `#${id}`): MergedEvent {
  const start = MIDNIGHT + at * MINUTE;
  const row = {
    id: id as EventId,
    calendarId: "cal",
    accountId: 1,
    title,
    start,
    end: start + minutes * MINUTE,
    allDay: false,
    attendees: [],
  } as unknown as CalendarEvent;
  return { event: row, copies: [row], calendarIds: ["cal"], accountIds: [1], merged: false };
}

let opened: EventId[] = [];

function render(events: MergedEvent[]) {
  opened = [];
  act(() =>
    root.render(
      <TimeGrid
        days={[DAY]}
        events={events}
        colorFor={() => "#7c6cff"}
        dark={false}
        selectedId={null}
        onSelect={() => {}}
        onOpen={(id) => opened.push(id)}
        onDraft={() => {}}
        onMove={() => {}}
        todayNonce={0}
      />,
    ),
  );
}

/** One absolutely-positioned box the grid drew, in the day column's own space. */
interface Box {
  node: HTMLElement;
  id: number;
  top: number;
  bottom: number;
  z: number;
  /** Painted body or transparent skirt — `role="presentation"` marks the skirt. */
  skirt: boolean;
}

function boxes(): Box[] {
  return [...container.querySelectorAll<HTMLElement>("[data-event-id]")].map((node) => {
    const top = Number.parseFloat(node.style.top);
    return {
      node,
      id: Number(node.getAttribute("data-event-id")),
      top,
      bottom: top + Number.parseFloat(node.style.height),
      z: Number(node.style.zIndex),
      skirt: node.getAttribute("role") === "presentation",
    };
  });
}

function body(id: number): Box {
  const found = boxes().find((box) => box.id === id && !box.skirt);
  if (!found) throw new Error(`no painted block for ${id}`);
  return found;
}

function skirtOf(id: number): Box | undefined {
  return boxes().find((box) => box.id === id && box.skirt);
}

/**
 * Which box a press at `y` lands on: topmost layer wins, later DOM wins a tie.
 * Horizontal position is not in it — every block here has the column to itself.
 */
function resolveHit(y: number): Box | undefined {
  const hits = boxes().filter((box) => y >= box.top && y < box.bottom);
  return hits.reduce<Box | undefined>(
    (best, box) => (best === undefined || box.z >= best.z ? box : best),
    undefined,
  );
}

describe("hitSkirt", () => {
  it("leaves a block that is already comfortable alone", () => {
    expect(hitSkirt(blockHeight(60 * MINUTE))).toBe(0);
    expect(hitSkirt(MIN_HIT_HEIGHT)).toBe(0);
  });

  it("brings a 30-minute block up to the minimum", () => {
    const height = blockHeight(30 * MINUTE);
    expect(height).toBe(31);
    expect(height + 2 * hitSkirt(height)).toBeGreaterThanOrEqual(MIN_HIT_HEIGHT);
  });

  it("takes a 15-minute block as far as the overhang allows", () => {
    const height = blockHeight(15 * MINUTE);
    expect(height).toBe(15);
    // Not all the way to the minimum — the ceiling is what keeps the grid under
    // a quarter-hour meeting available to drag-create in.
    expect(hitSkirt(height)).toBe(MAX_HIT_OVERHANG);
    expect(height + 2 * hitSkirt(height)).toBe(31);
  });

  it("never reaches further than the overhang, however short the block", () => {
    for (let height = 0; height <= MIN_HIT_HEIGHT + 8; height++) {
      expect(hitSkirt(height)).toBeLessThanOrEqual(MAX_HIT_OVERHANG);
      expect(hitSkirt(height)).toBeGreaterThanOrEqual(0);
    }
  });

  it("never shrinks the reach as the block gets shorter", () => {
    for (let height = 1; height <= MIN_HIT_HEIGHT + 8; height++) {
      expect(hitSkirt(height - 1)).toBeGreaterThanOrEqual(hitSkirt(height));
    }
  });
});

describe("what the grid draws", () => {
  it("does not move or grow the painted block", () => {
    render([event(1, 9 * 60, 30)]);
    const painted = body(1);
    expect(painted.top).toBe(9 * HOUR_HEIGHT);
    expect(painted.bottom - painted.top).toBe(blockHeight(30 * MINUTE));
  });

  it("gives a 60-minute block no skirt at all", () => {
    render([event(1, 9 * 60, 60)]);
    expect(skirtOf(1)).toBeUndefined();
  });

  it("centres the skirt on the block it belongs to", () => {
    render([event(1, 9 * 60, 15)]);
    const painted = body(1);
    const skirt = skirtOf(1);
    if (!skirt) throw new Error("a 15-minute block should carry a skirt");
    expect(painted.top - skirt.top).toBe(skirt.bottom - painted.bottom);
    expect(skirt.bottom - skirt.top).toBe(blockHeight(15 * MINUTE) + 2 * MAX_HIT_OVERHANG);
  });

  it("puts every skirt under every painted block", () => {
    render([event(1, 9 * 60, 15), event(2, 10 * 60, 30)]);
    for (const box of boxes()) {
      expect(box.z).toBe(box.skirt ? Z_EVENT_HIT : Z_EVENT);
    }
    expect(Z_EVENT_HIT).toBeLessThan(Z_EVENT);
  });

  it("opens the event when the skirt is the thing clicked", () => {
    render([event(7, 9 * 60, 30)]);
    const skirt = skirtOf(7);
    if (!skirt) throw new Error("a 30-minute block should carry a skirt");
    act(() => {
      skirt.node.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(opened).toEqual([7]);
  });
});

describe("two short blocks in a row", () => {
  /*
   * 09:00–09:15 and 09:15–09:30. It was a pair of half-hours when a half-hour
   * was 23px and wanted 5px a side. At 31px it wants one, which is too small a
   * reach for the arbitration to be worth asserting — so the case moved to the
   * shortest blocks there are, which are the ones that still overhang.
   */
  function stacked() {
    render([event(1, 9 * 60, 15), event(2, 9 * 60 + 15, 15)]);
  }

  it("does not let the block above steal the block below's first pixels", () => {
    stacked();
    const lower = body(2);
    const above = skirtOf(1);
    if (!above) throw new Error("the upper block should carry a skirt");
    // Without this the rest of the test proves nothing: the upper block's skirt
    // has to actually reach into the lower block's body for there to be a
    // contest to arbitrate.
    expect(above.bottom).toBeGreaterThan(lower.top);

    // Every one of these presses still has to land on the lower block.
    for (const y of [lower.top, lower.top + 1, lower.top + 2]) {
      expect(resolveHit(y)?.id).toBe(2);
      expect(resolveHit(y)?.skirt).toBe(false);
    }
  });

  it("does not let the block below steal the block above's last pixels", () => {
    stacked();
    const upper = body(1);
    const below = skirtOf(2);
    if (!below) throw new Error("the lower block should carry a skirt");
    expect(below.top).toBeLessThan(upper.bottom);

    for (const y of [upper.bottom - 1, upper.bottom - 2]) {
      expect(resolveHit(y)?.id).toBe(1);
      expect(resolveHit(y)?.skirt).toBe(false);
    }
  });

  it("hands the empty grid above the pair to the block that starts there", () => {
    stacked();
    const upper = body(1);
    expect(resolveHit(upper.top - 1)?.id).toBe(1);
    expect(resolveHit(upper.top - hitSkirt(upper.bottom - upper.top))?.id).toBe(1);
  });

  it("stops claiming grid once the skirt has run out", () => {
    stacked();
    const upper = body(1);
    const reach = hitSkirt(upper.bottom - upper.top);
    expect(resolveHit(upper.top - reach - 1)).toBeUndefined();
  });
});

describe("an isolated short block", () => {
  it("is a target of at least the minimum, top to bottom", () => {
    const height = blockHeight(30 * MINUTE);
    render([event(1, 12 * 60 + 30, 30)]);
    const first = resolveHit(12.5 * HOUR_HEIGHT - hitSkirt(height));
    const beyond = resolveHit(12.5 * HOUR_HEIGHT + height + hitSkirt(height));
    expect(first?.id).toBe(1);
    // The bottom edge is exclusive, so one past the skirt is empty grid again.
    expect(beyond).toBeUndefined();

    let reachable = 0;
    for (let y = 0; y < 24 * HOUR_HEIGHT; y++) if (resolveHit(y)?.id === 1) reachable++;
    expect(reachable).toBeGreaterThanOrEqual(MIN_HIT_HEIGHT);
  });
});

describe("resizing a 30-minute block", () => {
  /**
   * The floor for the edge handles was a raw 24, and a 30-minute block renders
   * at 23 because `blockHeight` has already taken the stacking gap out. So the
   * most common meeting length in the week had no mouse resize at all — the
   * same off-by-one-gap the tier ladder had, in a second place.
   */
  it("has both edge handles", () => {
    render([event(1, 9 * 60, 30)]);
    const painted = body(1);
    const handles = painted.node.querySelectorAll('[role="presentation"]');
    expect(handles).toHaveLength(2);
  });

  it("still leaves a 15-minute block without them, having no room", () => {
    // 15px cannot hold two grab zones and a body between them. Moving it is
    // still possible; resizing it needs the modal, or a taller scale.
    render([event(1, 9 * 60, 15)]);
    expect(body(1).node.querySelectorAll('[role="presentation"]')).toHaveLength(0);
  });
});
