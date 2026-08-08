import { beforeEach, describe, expect, it } from "vitest";
import {
  annotationReducer,
  closeFeedback,
  emptyAnnotation,
  feedbackScore,
  hasInk,
  isFeedbackOpen,
  seedFromQuery,
  strokeWidthFor,
  visibleShapes,
  type AnnotationState,
  type AnnotationTool,
} from "./feedback";
import { resolve, type PaletteContext, type PaletteResult } from "./palette/resolver";

/* -------------------------------------------------------------------------- */
/* ⌘K                                                                          */
/* -------------------------------------------------------------------------- */

/** The palette's context with nothing in it: only the resolver chain is under test. */
function context(query: string): PaletteContext {
  return {
    query,
    threads: [],
    events: [],
    people: [],
    mailboxes: [],
    commands: [],
    actions: {
      openThread: () => {},
      openEvent: () => {},
      openMailbox: () => {},
      runCommand: () => {},
      composeTo: () => {},
    },
  };
}

function sendFeedback(query: string): PaletteResult | undefined {
  return resolve(context(query)).find((r) => r.id === "command:send-feedback");
}

describe("the ⌘K entry point", () => {
  beforeEach(() => closeFeedback());

  it("is registered in the shared resolver chain, not in the palette component", () => {
    // If this fails the action exists but nothing can reach it.
    expect(sendFeedback("feedback")).toBeDefined();
    expect(sendFeedback("feedback")?.title).toBe("Send feedback");
  });

  it("surfaces on the words that mean “change something”", () => {
    for (const query of ["feedback", "feed", "bug", "broken", "fix", "screenshot", "send fee"]) {
      expect(sendFeedback(query), query).toBeDefined();
    }
  });

  it("surfaces on a sentence, which is never a mail search", () => {
    expect(sendFeedback("move the account bar to the left")).toBeDefined();
    expect(sendFeedback("this row is too cramped")).toBeDefined();
  });

  it("stays out of the way of ordinary searching", () => {
    for (const query of ["", "tawny", "series a", "q3 numbers"]) {
      expect(sendFeedback(query), query).toBeUndefined();
    }
  });

  it("appears in explicit `>` command mode", () => {
    expect(sendFeedback(">")).toBeDefined();
    expect(sendFeedback(">feed")).toBeDefined();
  });

  it("outranks the ordinary command layer when he is asking for a change", () => {
    // Priority 30 beats the command resolver's 20, so it heads the group.
    const results = resolve(context("feedback"));
    expect(results[0]?.id).toBe("command:send-feedback");
  });

  it("running it opens the capture surface", () => {
    expect(isFeedbackOpen()).toBe(false);
    sendFeedback("feedback")?.run();
    expect(isFeedbackOpen()).toBe(true);
  });

  it("carries a typed sentence into the note, but not a keyword", () => {
    expect(seedFromQuery("move the account bar to the left")).toBe(
      "move the account bar to the left",
    );
    expect(seedFromQuery("feedback")).toBe("");
    expect(seedFromQuery(">this row is too cramped")).toBe("this row is too cramped");
  });

  it("scores the action's own name above a bare trigger word", () => {
    expect(feedbackScore("send feedback")).toBeGreaterThan(feedbackScore("broken"));
    expect(feedbackScore("broken")).toBeGreaterThan(feedbackScore("a sentence with four words"));
  });
});

/* -------------------------------------------------------------------------- */
/* Annotation                                                                  */
/* -------------------------------------------------------------------------- */

/** A drag: press, move through every point, release. */
function drag(state: AnnotationState, tool: AnnotationTool, points: [number, number][]) {
  const [first, ...rest] = points;
  let next = annotationReducer(state, {
    type: "start",
    tool,
    point: { x: first![0], y: first![1] },
  });
  for (const [x, y] of rest) next = annotationReducer(next, { type: "move", point: { x, y } });
  return annotationReducer(next, { type: "commit" });
}

describe("the annotation state machine", () => {
  it("commits a drag as one shape", () => {
    const state = drag(emptyAnnotation, "arrow", [
      [10, 10],
      [80, 90],
    ]);
    expect(state.shapes).toHaveLength(1);
    expect(state.shapes[0]?.tool).toBe("arrow");
    expect(state.draft).toBeNull();
  });

  it("keeps every point of a freehand stroke and only the ends of an arrow", () => {
    const path: [number, number][] = [
      [0, 0],
      [20, 5],
      [40, 30],
      [60, 10],
    ];
    expect(drag(emptyAnnotation, "pen", path).shapes[0]?.points).toHaveLength(4);
    expect(drag(emptyAnnotation, "arrow", path).shapes[0]?.points).toEqual([
      { x: 0, y: 0 },
      { x: 60, y: 10 },
    ]);
  });

  it("undoes one shape at a time, newest first", () => {
    let state = drag(emptyAnnotation, "arrow", [
      [0, 0],
      [50, 50],
    ]);
    state = drag(state, "rect", [
      [10, 10],
      [90, 90],
    ]);
    expect(state.shapes).toHaveLength(2);

    state = annotationReducer(state, { type: "undo" });
    expect(state.shapes).toHaveLength(1);
    expect(state.shapes[0]?.tool).toBe("arrow");

    state = annotationReducer(state, { type: "undo" });
    expect(state.shapes).toHaveLength(0);
  });

  it("undo on an empty canvas is a no-op, not an error", () => {
    const state = annotationReducer(emptyAnnotation, { type: "undo" });
    expect(state).toBe(emptyAnnotation);
    expect(hasInk(state)).toBe(false);
  });

  it("undo mid-drag abandons the stroke in progress and keeps the rest", () => {
    let state = drag(emptyAnnotation, "arrow", [
      [0, 0],
      [50, 50],
    ]);
    state = annotationReducer(state, { type: "start", tool: "pen", point: { x: 1, y: 1 } });
    state = annotationReducer(state, { type: "move", point: { x: 30, y: 30 } });
    expect(state.draft).not.toBeNull();

    state = annotationReducer(state, { type: "undo" });
    expect(state.draft).toBeNull();
    expect(state.shapes).toHaveLength(1);
  });

  it("clear removes everything including a stroke in progress", () => {
    let state = drag(emptyAnnotation, "rect", [
      [0, 0],
      [50, 50],
    ]);
    state = annotationReducer(state, { type: "start", tool: "pen", point: { x: 1, y: 1 } });
    state = annotationReducer(state, { type: "clear" });

    expect(state.shapes).toHaveLength(0);
    expect(state.draft).toBeNull();
    expect(hasInk(state)).toBe(false);
  });

  it("clear on an empty canvas changes nothing", () => {
    expect(annotationReducer(emptyAnnotation, { type: "clear" })).toBe(emptyAnnotation);
  });

  it("a stray click leaves no invisible shape to undo past", () => {
    const state = drag(emptyAnnotation, "arrow", [
      [40, 40],
      [41, 40],
    ]);
    expect(state.shapes).toHaveLength(0);
    expect(state.draft).toBeNull();
  });

  it("ignores movement when nothing is being drawn", () => {
    const state = annotationReducer(emptyAnnotation, { type: "move", point: { x: 5, y: 5 } });
    expect(state).toBe(emptyAnnotation);
  });

  it("shows the stroke in progress so the mark follows the pointer", () => {
    let state = annotationReducer(emptyAnnotation, {
      type: "start",
      tool: "rect",
      point: { x: 0, y: 0 },
    });
    state = annotationReducer(state, { type: "move", point: { x: 60, y: 60 } });
    expect(visibleShapes(state)).toHaveLength(1);
    expect(hasInk(state)).toBe(true);
  });

  it("scales the stroke with the capture so retina shots are not hairlines", () => {
    expect(strokeWidthFor(2800)).toBeGreaterThan(strokeWidthFor(1400));
    expect(strokeWidthFor(200)).toBeGreaterThanOrEqual(2);
  });
});
