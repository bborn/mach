/**
 * The composer's size and placement, asked without a window.
 *
 * Both answers are pure functions of their inputs precisely so this file can
 * exist: a height that fits, and a placement that a round trip returns to
 * exactly where it started — which is the property the pop-out has to have,
 * because the alternative to "the same draft, drawn elsewhere" is a second
 * draft, and three duplicate-draft bugs came from that.
 */

import { describe, expect, it } from "vitest";
import { COMPOSER_HEIGHT_BOUNDS } from "@/lib/prefs";
import {
  DEFAULT_COMPOSER_HEIGHT,
  POPPED_WINDOW_CHROME,
  RESERVED_READING_HEIGHT,
  canPopOut,
  clampComposerHeight,
  composerPlacement,
  forgetPopOut,
  isPoppedOut,
  popOutComposerHeight,
  togglePopOut,
} from "./composer-layout";

const { min, max } = COMPOSER_HEIGHT_BOUNDS;
/** Tall enough that the window is never the binding constraint. */
const TALL = 4000;

describe("clampComposerHeight", () => {
  it("keeps a height that fits", () => {
    expect(clampComposerHeight(DEFAULT_COMPOSER_HEIGHT, TALL)).toBe(DEFAULT_COMPOSER_HEIGHT);
    expect(clampComposerHeight(min, TALL)).toBe(min);
    expect(clampComposerHeight(320, TALL)).toBe(320);
  });

  it("holds to the bounds", () => {
    expect(clampComposerHeight(5000, TALL)).toBe(max);
    expect(clampComposerHeight(-40, TALL)).toBe(min);
    expect(clampComposerHeight(0, TALL)).toBe(min);
  });

  it("leaves room for the conversation it answers", () => {
    const viewport = 800;
    expect(clampComposerHeight(700, viewport)).toBe(viewport - RESERVED_READING_HEIGHT);
    expect(clampComposerHeight(700, viewport)).toBeLessThan(viewport);
  });

  it("gives the floor to a column too short for even that", () => {
    expect(clampComposerHeight(300, 300)).toBe(min);
    expect(clampComposerHeight(10, 300)).toBe(min);
  });

  it("uses the bounds alone when the column has not been measured", () => {
    expect(clampComposerHeight(400, 0)).toBe(400);
    expect(clampComposerHeight(5000, 0)).toBe(max);
  });

  /*
   * The second dock, which is where this argument stopped being the window.
   *
   * The agent drawer stands at the bottom of the window and takes its height
   * off the reading column, so the room the composer has is the column's and
   * nobody else's. A composer that asked the window instead kept the height it
   * had when the drawer opened and hung out of the bottom of the pane by
   * whatever the drawer took — 87px, at 1440×757 with the drawer at its
   * default. Every pixel the column loses has to come off the ceiling, or the
   * arithmetic is not the one that keeps the footer on screen.
   */
  it("takes a pixel off the ceiling for every pixel the second dock takes", () => {
    const column = 900;
    for (const drawer of [0, 160, 320, 480]) {
      expect(clampComposerHeight(9000, column - drawer)).toBe(
        Math.max(min, column - drawer - RESERVED_READING_HEIGHT),
      );
    }
  });

  it("rounds", () => {
    expect(clampComposerHeight(200.4, TALL)).toBe(200);
    expect(clampComposerHeight(200.6, TALL)).toBe(201);
  });

  it("lands on the default when the stored value is nonsense", () => {
    expect(clampComposerHeight(Number.NaN, TALL)).toBe(DEFAULT_COMPOSER_HEIGHT);
    expect(clampComposerHeight(Number.POSITIVE_INFINITY, TALL)).toBe(DEFAULT_COMPOSER_HEIGHT);
  });

  it("is idempotent, so restoring what was stored cannot drift", () => {
    for (const height of [min - 50, min, 200, 640, max, max + 500]) {
      const once = clampComposerHeight(height, 900);
      expect(clampComposerHeight(once, 900)).toBe(once);
    }
  });
});

describe("popOutComposerHeight", () => {
  it("takes the window, less its own furniture", () => {
    expect(popOutComposerHeight(900)).toBe(900 - POPPED_WINDOW_CHROME);
  });

  it("never goes below the floor, however short the window", () => {
    expect(popOutComposerHeight(200)).toBe(min);
  });

  it("never goes past the ceiling, however tall", () => {
    expect(popOutComposerHeight(4000)).toBe(max);
  });

  it("answers before the window has been measured", () => {
    expect(popOutComposerHeight(0)).toBeGreaterThanOrEqual(min);
    expect(popOutComposerHeight(0)).toBeLessThanOrEqual(max);
  });
});

describe("canPopOut", () => {
  it("is true of everything that has a dock to leave", () => {
    expect(canPopOut("reply")).toBe(true);
    expect(canPopOut("replyAll")).toBe(true);
    expect(canPopOut("forward")).toBe(true);
    expect(canPopOut("adopted")).toBe(true);
  });

  it("is false of a new message, which is already over the window", () => {
    expect(canPopOut("new")).toBe(false);
  });
});

describe("composerPlacement", () => {
  it("docks a reply and floats a new message", () => {
    expect(composerPlacement("reply", false)).toBe("dock");
    expect(composerPlacement("new", false)).toBe("overlay");
  });

  it("floats a reply that has been popped out", () => {
    expect(composerPlacement("reply", true)).toBe("overlay");
  });

  it("leaves a new message where it was, whatever the flag says", () => {
    expect(composerPlacement("new", true)).toBe("overlay");
  });
});

describe("the pop-out round trip", () => {
  it("comes back to the dock, with the draft untouched", () => {
    const id = "draft-1";
    const out = togglePopOut([], id);
    expect(isPoppedOut(out, id)).toBe(true);
    expect(composerPlacement("reply", isPoppedOut(out, id))).toBe("overlay");

    const back = togglePopOut(out, id);
    expect(back).toEqual([]);
    expect(isPoppedOut(back, id)).toBe(false);
    expect(composerPlacement("reply", isPoppedOut(back, id))).toBe("dock");
  });

  it("never adds a second entry for the same draft", () => {
    const once = togglePopOut([], "d1");
    expect(togglePopOut(togglePopOut(once, "d1"), "d1")).toEqual(["d1"]);
  });

  it("keeps other composers where they were", () => {
    const popped = togglePopOut(togglePopOut([], "d1"), "d2");
    expect(togglePopOut(popped, "d1")).toEqual(["d2"]);
  });

  it("does not mutate the set it was given", () => {
    const before = ["d1"];
    togglePopOut(before, "d2");
    forgetPopOut(before, "d1");
    expect(before).toEqual(["d1"]);
  });

  it("forgets a composer that has closed", () => {
    expect(forgetPopOut(["d1", "d2"], "d1")).toEqual(["d2"]);
    expect(forgetPopOut(["d2"], "d1")).toEqual(["d2"]);
  });
});
