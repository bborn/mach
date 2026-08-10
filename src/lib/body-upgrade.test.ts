import { describe, expect, it } from "vitest";
import {
  correctedScrollTop,
  findScroller,
  isBeingRead,
  needsCorrection,
  shouldApplyUpgrade,
  type ScrollProbe,
} from "@/lib/body-upgrade";

describe("isBeingRead", () => {
  it("is true only when the message spans the reader's view", () => {
    // Top scrolled past, bottom not yet arrived: the reader is in the middle of
    // it, and anything that changes its height changes the sentence they are on.
    expect(isBeingRead({ top: -400, bottom: 900 })).toBe(true);
  });

  it("is false when the top is still on screen", () => {
    // Growth happens below the point of attention, which is where the anchor
    // correction already puts it.
    expect(isBeingRead({ top: 20, bottom: 4000 })).toBe(false);
  });

  it("is false for a message entirely above or entirely below", () => {
    expect(isBeingRead({ top: -900, bottom: -100 })).toBe(false);
    expect(isBeingRead({ top: 700, bottom: 1500 })).toBe(false);
  });
});

describe("shouldApplyUpgrade", () => {
  it("applies immediately when the reader has not scrolled", () => {
    // The common case by a distance: open a message, look at the top, the HTML
    // lands 200 ms later. Holding it here would hold it on almost every message
    // that is taller than the pane, which is most of them.
    expect(shouldApplyUpgrade(false, { top: -2000, bottom: 6000 })).toBe(true);
  });

  it("holds while an engaged reader is inside the message", () => {
    expect(shouldApplyUpgrade(true, { top: -400, bottom: 900 })).toBe(false);
  });

  it("applies to an engaged reader once the message is not under their eyes", () => {
    expect(shouldApplyUpgrade(true, { top: 40, bottom: 900 })).toBe(true);
    expect(shouldApplyUpgrade(true, { top: -3000, bottom: -20 })).toBe(true);
  });
});

describe("correctedScrollTop", () => {
  it("scrolls down by exactly what the box moved down", () => {
    // The message's top was 120px into the viewport and is now 480px: something
    // above it grew by 360, so the container scrolls 360 further.
    expect(correctedScrollTop(1000, 120, 480)).toBe(1360);
  });

  it("scrolls back up when the box moved up", () => {
    expect(correctedScrollTop(1000, 480, 120)).toBe(640);
  });

  it("never asks for a negative offset", () => {
    // `scrollTop` would clamp it silently, which would leave the caller
    // believing the anchor had settled when it had not.
    expect(correctedScrollTop(50, 400, 0)).toBe(0);
  });

  it("is a no-op when nothing moved", () => {
    expect(correctedScrollTop(1000, 300, 300)).toBe(1000);
  });
});

describe("needsCorrection", () => {
  it("ignores sub-pixel drift", () => {
    // Acting on it costs a reflow and buys nothing anyone can see.
    expect(needsCorrection(300, 300.4)).toBe(false);
    expect(needsCorrection(300, 302)).toBe(true);
  });
});

/* -------------------------------------------------------------------------- */

interface FakeNode {
  name: string;
  parent: FakeNode | null;
  overflowY: string;
  scrollHeight: number;
  clientHeight: number;
}

const PROBE: ScrollProbe<FakeNode> = {
  parent: (n) => n.parent,
  overflowY: (n) => n.overflowY,
  scrollHeight: (n) => n.scrollHeight,
  clientHeight: (n) => n.clientHeight,
};

function chain(...nodes: Omit<FakeNode, "parent">[]): FakeNode {
  // Innermost first; each one's parent is the next.
  let parent: FakeNode | null = null;
  for (let i = nodes.length - 1; i >= 0; i -= 1) {
    parent = { ...nodes[i]!, parent };
  }
  return parent!;
}

const STATIC = { overflowY: "visible", scrollHeight: 100, clientHeight: 100 };

describe("findScroller", () => {
  it("finds the nearest ancestor that both scrolls and has something to scroll", () => {
    const leaf = chain(
      { name: "body", ...STATIC },
      { name: "wrapper", ...STATIC },
      { name: "pane", overflowY: "auto", scrollHeight: 5000, clientHeight: 800 },
      { name: "window", overflowY: "auto", scrollHeight: 900, clientHeight: 900 },
    );
    expect(findScroller(leaf, PROBE)?.name).toBe("pane");
  });

  it("skips a scrollable ancestor with nothing to scroll", () => {
    // A pane the content has not filled yet would swallow the correction; the
    // one above it is the one that has to move.
    const leaf = chain(
      { name: "body", ...STATIC },
      { name: "empty", overflowY: "auto", scrollHeight: 400, clientHeight: 400 },
      { name: "outer", overflowY: "scroll", scrollHeight: 9000, clientHeight: 600 },
    );
    expect(findScroller(leaf, PROBE)?.name).toBe("outer");
  });

  it("never returns the node it started from", () => {
    // The message's own box may well scroll; correcting against itself would
    // move nothing and hide the fact that there was no container.
    const leaf = chain({ name: "self", overflowY: "auto", scrollHeight: 9000, clientHeight: 100 });
    expect(findScroller(leaf, PROBE)).toBeNull();
  });

  it("returns null when nothing above scrolls", () => {
    const leaf = chain({ name: "body", ...STATIC }, { name: "root", ...STATIC });
    expect(findScroller(leaf, PROBE)).toBeNull();
    expect(findScroller(null, PROBE)).toBeNull();
  });
});
