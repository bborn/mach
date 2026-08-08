import { describe, expect, it } from "vitest";
import { columnGeometry, layoutEvents, overlaps } from "./event-layout";

const H = 3_600_000;
/** 9am on an arbitrary day, in hours-since-that-midnight terms. */
const at = (hour: number) => hour * H;
const ev = (id: string, startHour: number, endHour: number) => ({
  id,
  start: at(startHour),
  end: at(endHour),
});

function byId(laid: ReturnType<typeof layoutEvents>) {
  return Object.fromEntries(laid.map((l) => [l.event.id, l]));
}

describe("overlaps", () => {
  it("is strict — back-to-back events do not overlap", () => {
    expect(overlaps(ev("a", 9, 10), ev("b", 10, 11))).toBe(false);
  });

  it("detects partial overlap in both directions", () => {
    expect(overlaps(ev("a", 9, 11), ev("b", 10, 12))).toBe(true);
    expect(overlaps(ev("b", 10, 12), ev("a", 9, 11))).toBe(true);
  });
});

describe("layoutEvents", () => {
  it("returns nothing for no events", () => {
    expect(layoutEvents([])).toEqual([]);
  });

  it("gives a lone event the full width", () => {
    const [only] = layoutEvents([ev("a", 9, 10)]);
    expect(only).toMatchObject({ column: 0, span: 1, columns: 1 });
  });

  it("puts overlapping events in distinct columns", () => {
    const laid = byId(layoutEvents([ev("a", 9, 11), ev("b", 10, 12)]));
    expect(laid.a.column).toBe(0);
    expect(laid.b.column).toBe(1);
    expect(laid.a.columns).toBe(2);
    expect(laid.b.columns).toBe(2);
    expect(laid.a.column).not.toBe(laid.b.column);
  });

  it("gives events with identical times distinct columns", () => {
    const laid = layoutEvents([ev("a", 9, 10), ev("b", 9, 10), ev("c", 9, 10)]);
    expect(new Set(laid.map((l) => l.column)).size).toBe(3);
    expect(laid.every((l) => l.columns === 3)).toBe(true);
  });

  it("reuses column 0 for non-overlapping events", () => {
    const laid = layoutEvents([ev("a", 9, 10), ev("b", 11, 12), ev("c", 13, 14)]);
    expect(laid.map((l) => l.column)).toEqual([0, 0, 0]);
    // Separate clusters, so each is full width rather than a third each.
    expect(laid.map((l) => l.columns)).toEqual([1, 1, 1]);
  });

  it("treats back-to-back events as separate clusters", () => {
    const laid = byId(layoutEvents([ev("a", 9, 10), ev("b", 10, 11)]));
    expect(laid.a.columns).toBe(1);
    expect(laid.b.columns).toBe(1);
  });

  it("does not let one cluster's width leak into the next", () => {
    // a/b overlap; c is later and alone, so c must stay full width.
    const laid = byId(layoutEvents([ev("a", 9, 11), ev("b", 9.5, 10), ev("c", 14, 15)]));
    expect(laid.a.columns).toBe(2);
    expect(laid.c.columns).toBe(1);
    expect(laid.c.column).toBe(0);
  });

  it("reuses a freed column within the same cluster", () => {
    // a spans the cluster; b then c share the second column in sequence.
    const laid = byId(layoutEvents([ev("a", 9, 13), ev("b", 9, 10), ev("c", 11, 12)]));
    expect(laid.a.column).toBe(0);
    expect(laid.b.column).toBe(1);
    expect(laid.c.column).toBe(1);
    expect(laid.a.columns).toBe(2);
  });

  it("handles an event spanning the whole day alongside short ones", () => {
    const laid = byId(layoutEvents([ev("all", 0, 24), ev("a", 9, 10), ev("b", 9, 10)]));
    expect(laid.all.column).toBe(0);
    expect(laid.a.column).toBe(1);
    expect(laid.b.column).toBe(2);
    expect(laid.all.columns).toBe(3);
    // The all-day block is boxed in on its right by a, so it cannot widen.
    expect(laid.all.span).toBe(1);
  });

  it("widens an event across columns that are clear to its right", () => {
    // a and b fill columns 0 and 1 in the morning; the long c takes column 2.
    // By midday a and b are over, so d can take columns 0 and 1 — but not 2,
    // because c is still running.
    const laid = byId(
      layoutEvents([ev("a", 9, 10), ev("b", 9, 10), ev("c", 9.5, 16), ev("d", 12, 13)]),
    );
    expect(laid.c.columns).toBe(3);
    expect(laid.c.column).toBe(2);
    expect(laid.d.column).toBe(0);
    expect(laid.d.span).toBe(2);
    // a is boxed in on its right by b, so it stays one column wide.
    expect(laid.a.span).toBe(1);
  });

  it("places zero-length events without breaking the cluster", () => {
    const laid = byId(layoutEvents([ev("z", 10, 10)]));
    expect(laid.z).toMatchObject({ column: 0, span: 1, columns: 1 });
  });

  it("gives a zero-length event inside another event its own column", () => {
    const laid = byId(layoutEvents([ev("a", 9, 11), ev("z", 10, 10)]));
    expect(laid.a.column).toBe(0);
    expect(laid.z.column).toBe(1);
    expect(laid.z.columns).toBe(2);
  });

  it("does not overlap a zero-length event sitting on another's boundary", () => {
    const laid = byId(layoutEvents([ev("a", 9, 11), ev("z", 11, 11)]));
    expect(laid.a.columns).toBe(1);
    expect(laid.z.columns).toBe(1);
  });

  it("is stable regardless of input order", () => {
    const forward = layoutEvents([ev("a", 9, 11), ev("b", 10, 12), ev("c", 10.5, 11)]);
    const reversed = layoutEvents([ev("c", 10.5, 11), ev("b", 10, 12), ev("a", 9, 11)]);
    expect(byId(forward).a.column).toBe(byId(reversed).a.column);
    expect(byId(forward).b.column).toBe(byId(reversed).b.column);
    expect(byId(forward).c.column).toBe(byId(reversed).c.column);
  });

  it("never assigns two overlapping events the same column", () => {
    const events = [
      ev("a", 9, 12), ev("b", 9, 9.5), ev("c", 9.25, 10), ev("d", 10, 11),
      ev("e", 10.5, 13), ev("f", 12, 12.5), ev("g", 15, 16), ev("h", 15, 17),
    ];
    const laid = layoutEvents(events);
    for (const x of laid) {
      for (const y of laid) {
        if (x === y) continue;
        if (overlaps(x.event, y.event)) expect(x.column).not.toBe(y.column);
      }
    }
  });
});

describe("columnGeometry", () => {
  it("splits the width evenly and leaves a hairline gutter", () => {
    const [a, b] = layoutEvents([ev("a", 9, 11), ev("b", 10, 12)]);
    expect(columnGeometry(a)).toEqual({ left: "calc(0% + 1px)", width: "calc(50% - 2px)" });
    expect(columnGeometry(b)).toEqual({ left: "calc(50% + 1px)", width: "calc(50% - 2px)" });
  });
});
