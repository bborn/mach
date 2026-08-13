import { beforeEach, describe, expect, it } from "vitest";
import { applyScrollPhase, readFingers, resetScrollPhase } from "./scroll-phase";

describe("scroll phase", () => {
  beforeEach(resetScrollPhase);

  it("reports nothing until something publishes", () => {
    expect(readFingers()).toBeNull();
  });

  it("carries a landing through with its gesture number", () => {
    applyScrollPhase({ phase: "fingers-down", gesture: 3 });
    expect(readFingers()).toEqual({ down: true, gesture: 3 });
  });

  it("keeps the gesture number across the lift that ends it", () => {
    applyScrollPhase({ phase: "fingers-down", gesture: 3 });
    applyScrollPhase({ phase: "fingers-up", gesture: 3 });
    expect(readFingers()).toEqual({ down: false, gesture: 3 });
  });

  it("collapses a device with no phase to nothing at all", () => {
    // A scroll wheel is not "fingers up" — it has no fingers. Publishing it as
    // a lift would make the wheel path answerable to a signal about a trackpad.
    applyScrollPhase({ phase: "fingers-down", gesture: 3 });
    applyScrollPhase({ phase: "no-phase", gesture: 3 });
    expect(readFingers()).toBeNull();
  });
});
