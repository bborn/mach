/**
 * The arithmetic behind carrying the caret between the dock and the window.
 *
 * The DOM halves of `caret-offset.ts` are two calls each and belong to the
 * browser; this is the part that can be wrong — which text node a character
 * offset lands in, and what happens at a boundary.
 */

import { describe, expect, it } from "vitest";
import { locateOffset } from "./caret-offset";

describe("locateOffset", () => {
  it("finds the offset inside a single run of text", () => {
    expect(locateOffset([11], 0)).toEqual({ index: 0, offset: 0 });
    expect(locateOffset([11], 5)).toEqual({ index: 0, offset: 5 });
    expect(locateOffset([11], 11)).toEqual({ index: 0, offset: 11 });
  });

  it("walks into the node the character is in", () => {
    // "hello" | " there" | " you"
    expect(locateOffset([5, 6, 4], 7)).toEqual({ index: 1, offset: 2 });
    expect(locateOffset([5, 6, 4], 12)).toEqual({ index: 2, offset: 1 });
  });

  it("keeps a boundary with the text before it", () => {
    // The caret at the end of "hello" is at the end of "hello", not at the
    // start of the paragraph after it.
    expect(locateOffset([5, 6], 5)).toEqual({ index: 0, offset: 5 });
  });

  it("steps over empty nodes rather than stopping in one", () => {
    expect(locateOffset([0, 4], 0)).toEqual({ index: 0, offset: 0 });
    expect(locateOffset([0, 4], 3)).toEqual({ index: 1, offset: 3 });
  });

  it("clamps past the end of a document that has shrunk", () => {
    expect(locateOffset([5, 6], 400)).toEqual({ index: 1, offset: 6 });
  });

  it("clamps a negative offset to the start", () => {
    expect(locateOffset([5], -3)).toEqual({ index: 0, offset: 0 });
  });

  it("rounds, because a caret is a whole character", () => {
    expect(locateOffset([9], 3.4)).toEqual({ index: 0, offset: 3 });
    expect(locateOffset([9], 3.6)).toEqual({ index: 0, offset: 4 });
  });

  it("has nothing to point at in an empty document", () => {
    expect(locateOffset([], 0)).toBeNull();
    expect(locateOffset([], 12)).toBeNull();
  });
});
