/**
 * The cases that make a multi-select feel broken when they are subtly wrong.
 *
 * Every one of these is a rule the platform already taught the user: shift
 * replaces the range, ⌘ leaves the rest alone, the anchor moves on a click and
 * not on a shift. They are asserted here because none of them is visible in a
 * screenshot — you only notice them the third time you shift+click.
 */

import { describe, expect, it } from "vitest";
import {
  allSelected,
  anchorAt,
  between,
  clear,
  commandTargets,
  emptySelection,
  extendTo,
  hasSelection,
  isSelected,
  nextAfterRemoval,
  prune,
  reanchor,
  selectAll,
  selectAllMessage,
  selectOnly,
  toggle,
  toggleAll,
  type Selection,
} from "./selection";

/** The visible list: ten rows, newest first, exactly as the UI orders them. */
const LIST = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];

function selection(over: Partial<Selection> = {}): Selection {
  return { ...emptySelection, ...over };
}

describe("plain click", () => {
  it("sets the anchor and selects nothing", () => {
    const next = anchorAt(emptySelection, 30);
    expect(next.anchor).toBe(30);
    expect(next.ids).toEqual([]);
    expect(hasSelection(next)).toBe(false);
  });

  it("throws away an existing selection", () => {
    const built = toggle(toggle(emptySelection, 20), 40);
    const next = anchorAt(built, 70);
    expect(next.ids).toEqual([]);
    expect(next.base).toEqual([]);
    expect(next.anchor).toBe(70);
  });

  it("is identity when the anchor is already there and nothing is selected", () => {
    const at30 = anchorAt(emptySelection, 30);
    expect(anchorAt(at30, 30)).toBe(at30);
  });
});

describe("moving the cursor with j / k", () => {
  it("moves the anchor without touching the selection", () => {
    const picked = toggle(toggle(emptySelection, 30), 40);
    const moved = reanchor(picked, 70);
    expect(moved.ids).toEqual([30, 40]);
    expect(moved.anchor).toBe(70);
  });

  it("makes the next shift-range grow on top of what was already ticked", () => {
    const picked = toggle(emptySelection, 10);
    const moved = reanchor(picked, 50);
    expect(extendTo(moved, 70, LIST).ids).toEqual([10, 50, 60, 70]);
  });

  it("is identity when the cursor did not actually move", () => {
    const at30 = reanchor(emptySelection, 30);
    expect(reanchor(at30, 30)).toBe(at30);
  });
});

describe("shift+click ranges", () => {
  it("selects the range forwards, inclusive of both ends", () => {
    const next = extendTo(anchorAt(emptySelection, 30), 60, LIST);
    expect(next.ids).toEqual([30, 40, 50, 60]);
  });

  it("selects the range backwards, still in list order", () => {
    const next = extendTo(anchorAt(emptySelection, 60), 30, LIST);
    expect(next.ids).toEqual([30, 40, 50, 60]);
  });

  it("selects a single row when both ends are the same row", () => {
    expect(extendTo(anchorAt(emptySelection, 40), 40, LIST).ids).toEqual([40]);
  });

  it("replaces the previous range rather than accumulating it", () => {
    const anchored = anchorAt(emptySelection, 30);
    const wide = extendTo(anchored, 90, LIST);
    expect(wide.ids).toEqual([30, 40, 50, 60, 70, 80, 90]);

    // Same anchor, a nearer target: the long range must go away entirely.
    const narrow = extendTo(wide, 50, LIST);
    expect(narrow.ids).toEqual([30, 40, 50]);
  });

  it("replaces a range that flips to the other side of the anchor", () => {
    const anchored = anchorAt(emptySelection, 50);
    const down = extendTo(anchored, 80, LIST);
    const up = extendTo(down, 20, LIST);
    expect(up.ids).toEqual([20, 30, 40, 50]);
  });

  it("leaves the anchor where the click put it", () => {
    const anchored = anchorAt(emptySelection, 30);
    const once = extendTo(anchored, 90, LIST);
    const twice = extendTo(once, 50, LIST);
    expect(once.anchor).toBe(30);
    expect(twice.anchor).toBe(30);
  });

  it("keeps rows picked out with ⌘ before the range was dragged", () => {
    const picked = toggle(emptySelection, 100);
    const anchored = { ...picked, anchor: 30, base: picked.ids };
    const ranged = extendTo(anchored, 50, LIST);
    expect(ranged.ids).toEqual([30, 40, 50, 100]);

    // …and shrinking the range still leaves the ⌘-clicked row alone.
    expect(extendTo(ranged, 40, LIST).ids).toEqual([30, 40, 100]);
  });

  it("treats the clicked row as the anchor when there is not one yet", () => {
    const next = extendTo(emptySelection, 70, LIST);
    expect(next.ids).toEqual([70]);
    expect(next.anchor).toBe(70);
  });

  it("falls back to the clicked row when the anchor has left the list", () => {
    const stale = selection({ anchor: 999 });
    expect(extendTo(stale, 60, LIST).ids).toEqual([60]);
  });
});

describe("⌘+click / x toggling", () => {
  it("adds a row without disturbing the rest", () => {
    const next = toggle(toggle(emptySelection, 20), 80);
    expect(next.ids).toEqual([20, 80]);
  });

  it("removes a row that was already selected", () => {
    const next = toggle(toggle(toggle(emptySelection, 20), 80), 20);
    expect(next.ids).toEqual([80]);
    expect(isSelected(next, 20)).toBe(false);
  });

  it("moves the anchor to the toggled row, so a range starts from the hand", () => {
    const picked = toggle(anchorAt(emptySelection, 10), 60);
    expect(picked.anchor).toBe(60);
    expect(extendTo(picked, 80, LIST).ids).toEqual([60, 70, 80]);
  });

  it("carries the toggled set into the next range as its base", () => {
    const picked = toggle(emptySelection, 10);
    const ranged = extendTo(toggle(picked, 60), 80, LIST);
    expect(ranged.ids).toEqual([10, 60, 70, 80]);
  });

  it("un-toggling out of a range shrinks the base with it", () => {
    const ranged = extendTo(anchorAt(emptySelection, 30), 50, LIST);
    const without = toggle(ranged, 40);
    expect(without.ids).toEqual([30, 50]);
    expect(without.base).toEqual([30, 50]);
  });
});

describe("select all", () => {
  it("selects every loaded row", () => {
    expect(selectAll(emptySelection, LIST).ids).toEqual(LIST);
  });

  it("toggles back to empty when everything is already selected", () => {
    const all = toggleAll(emptySelection, LIST);
    expect(all.ids).toEqual(LIST);
    expect(toggleAll(all, LIST).ids).toEqual([]);
  });

  it("selects all when only some rows are selected", () => {
    const some = toggle(emptySelection, 40);
    expect(toggleAll(some, LIST).ids).toEqual(LIST);
  });

  it("does not call an empty list fully selected", () => {
    expect(allSelected(emptySelection, [])).toBe(false);
    expect(toggleAll(emptySelection, []).ids).toEqual([]);
  });

  it("says how much of the mailbox 'all' actually was", () => {
    expect(selectAllMessage(247, true)).toBe("247 selected — loaded so far, not the whole mailbox");
    expect(selectAllMessage(12, false)).toBe("12 selected — the whole mailbox");
  });
});

describe("clearing", () => {
  it("empties the selection but remembers where the hand was", () => {
    const ranged = extendTo(anchorAt(emptySelection, 30), 60, LIST);
    const cleared = clear(ranged);
    expect(cleared.ids).toEqual([]);
    expect(cleared.base).toEqual([]);
    expect(cleared.anchor).toBe(30);
    // A shift+click after Escape still ranges from the last click.
    expect(extendTo(cleared, 50, LIST).ids).toEqual([30, 40, 50]);
  });

  it("is identity when there is nothing to clear", () => {
    expect(clear(emptySelection)).toBe(emptySelection);
  });
});

describe("pruning ids that vanished", () => {
  it("drops selected rows that have left the list", () => {
    const ranged = extendTo(anchorAt(emptySelection, 30), 60, LIST);
    const shrunk = prune(ranged, [10, 20, 30, 60, 70]);
    expect(shrunk.ids).toEqual([30, 60]);
    expect(shrunk.base).toEqual([]);
  });

  it("forgets an anchor that has left the list", () => {
    const anchored = anchorAt(emptySelection, 30);
    expect(prune(anchored, [10, 20]).anchor).toBeNull();
  });

  it("keeps the anchor while its row is still there", () => {
    const anchored = anchorAt(emptySelection, 30);
    expect(prune(anchored, LIST).anchor).toBe(30);
  });

  it("returns the same object when nothing vanished, so effects do not loop", () => {
    const ranged = extendTo(anchorAt(emptySelection, 30), 60, LIST);
    expect(prune(ranged, LIST)).toBe(ranged);
    expect(prune(emptySelection, LIST)).toBe(emptySelection);
  });

  it("empties everything when the list itself went away", () => {
    const ranged = extendTo(anchorAt(emptySelection, 30), 60, LIST);
    const gone = prune(ranged, []);
    expect(gone.ids).toEqual([]);
    expect(gone.anchor).toBeNull();
  });

  it("survives a refresh that reorders and prepends rows", () => {
    const ranged = extendTo(anchorAt(emptySelection, 30), 50, LIST);
    const refreshed = [5, 15, ...LIST];
    expect(prune(ranged, refreshed)).toBe(ranged);
    expect(extendTo(prune(ranged, refreshed), 60, refreshed).ids).toEqual([30, 40, 50, 60]);
  });
});

describe("what a command acts on", () => {
  it("is the selection when there is one", () => {
    const ranged = extendTo(anchorAt(emptySelection, 30), 60, LIST);
    expect(commandTargets(ranged, 100)).toEqual([30, 40, 50, 60]);
  });

  it("is the focused row when there is no selection", () => {
    expect(commandTargets(emptySelection, 70)).toEqual([70]);
  });

  it("is nothing when nothing is focused or selected", () => {
    expect(commandTargets(emptySelection, null)).toEqual([]);
  });

  it("hands every id to one command, not one command per id", () => {
    const fifty = Array.from({ length: 50 }, (_, i) => i + 1);
    const all = selectAll(emptySelection, fifty);
    const targets = commandTargets(all, 1);
    expect(targets).toHaveLength(50);
    expect(targets).toEqual(fifty);
  });
});

describe("where the cursor lands after a bulk action", () => {
  it("stays put when the focused row was not part of it", () => {
    expect(nextAfterRemoval(LIST, [10, 20], 60)).toBe(60);
  });

  it("moves to the next row below the removed block", () => {
    expect(nextAfterRemoval(LIST, [30, 40, 50], 30)).toBe(60);
  });

  it("falls back upwards when the removal took the tail", () => {
    expect(nextAfterRemoval(LIST, [80, 90, 100], 90)).toBe(70);
  });

  it("is nothing when the whole list went", () => {
    expect(nextAfterRemoval(LIST, LIST, 40)).toBeNull();
  });

  it("is nothing when there was no cursor", () => {
    expect(nextAfterRemoval(LIST, [10], null)).toBeNull();
  });
});

describe("re-selecting the survivors of a partial failure", () => {
  it("selects exactly the rolled-back ids, in list order", () => {
    const next = selectOnly(emptySelection, [70, 30], LIST);
    expect(next.ids).toEqual([30, 70]);
    expect(next.base).toEqual([30, 70]);
    expect(next.anchor).toBe(30);
  });

  it("drops ids that are not on screen to select", () => {
    expect(selectOnly(emptySelection, [30, 999], LIST).ids).toEqual([30]);
  });
});

describe("between", () => {
  it("is inclusive and order-independent", () => {
    expect(between(LIST, 20, 40)).toEqual([20, 30, 40]);
    expect(between(LIST, 40, 20)).toEqual([20, 30, 40]);
  });

  it("is empty when an endpoint is not in the list", () => {
    expect(between(LIST, 20, 999)).toEqual([]);
    expect(between(LIST, 999, 20)).toEqual([]);
  });
});
