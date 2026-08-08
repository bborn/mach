import { describe, expect, it } from "vitest";

import {
  applyFrecency,
  boostFor,
  decayedScore,
  load,
  record,
  save,
  topEntries,
  type FrecencyStore,
} from "./frecency";

const NOW = 1_800_000_000_000;
const DAY = 24 * 60 * 60 * 1000;
const WEEK = 7 * DAY;

describe("decay", () => {
  it("halves a use's worth every two weeks", () => {
    const fresh = decayedScore([NOW], NOW);
    const fortnight = decayedScore([NOW - 2 * WEEK], NOW);
    expect(fresh).toBeCloseTo(1, 5);
    expect(fortnight).toBeCloseTo(0.5, 2);
  });

  it("adds uses together", () => {
    expect(decayedScore([NOW, NOW, NOW], NOW)).toBeCloseTo(3, 5);
  });

  it("treats a future timestamp as now rather than inflating it", () => {
    // Clock skew or a restored backup should not mint extra weight.
    expect(decayedScore([NOW + WEEK], NOW)).toBeCloseTo(1, 5);
  });
});

describe("boost", () => {
  it("is zero for something never used", () => {
    expect(boostFor({}, "archive", NOW)).toBe(0);
  });

  it("rewards the first few uses much more than the twentieth", () => {
    const one = boostFor({ a: [NOW] }, "a", NOW);
    const five = boostFor({ a: Array(5).fill(NOW) }, "a", NOW);
    const twenty = boostFor({ a: Array(16).fill(NOW) }, "a", NOW);

    expect(five - one).toBeGreaterThan(twenty - five);
  });

  it("cannot outweigh a strong textual match", () => {
    // The resolver scores an exact-prefix hit in the dozens. History breaks
    // ties; it must not float a stale favourite above what was just typed.
    const hammered = boostFor({ a: Array(50).fill(NOW) }, "a", NOW);
    expect(hammered).toBeLessThan(13);
  });

  it("lets twenty uses last week beat one use an hour ago", () => {
    const habitual = boostFor({ a: Array(20).fill(NOW - WEEK) }, "a", NOW);
    const incidental = boostFor({ b: [NOW - 60 * 60 * 1000] }, "b", NOW);
    expect(habitual).toBeGreaterThan(incidental);
  });

  it("fades something abandoned months ago below something used today", () => {
    const march = boostFor({ a: Array(30).fill(NOW - 90 * DAY) }, "a", NOW);
    const today = boostFor({ b: [NOW] }, "b", NOW);
    expect(march).toBeLessThan(today);
  });
});

describe("record", () => {
  it("appends a use", () => {
    const store = record({}, "archive", NOW);
    expect(store["archive"]).toEqual([NOW]);
  });

  it("keeps the store bounded", () => {
    let store: FrecencyStore = {};
    for (let i = 0; i < 40; i++) store = record(store, "archive", NOW + i);
    expect(store["archive"]!.length).toBeLessThanOrEqual(16);
    // The ones kept are the newest.
    expect(store["archive"]![store["archive"]!.length - 1]).toBe(NOW + 39);
  });

  it("forgets entries nothing has touched in months", () => {
    const stale: FrecencyStore = { ancient: [NOW - 120 * DAY] };
    const store = record(stale, "fresh", NOW);
    expect(store["ancient"]).toBeUndefined();
    expect(store["fresh"]).toEqual([NOW]);
  });
});

describe("applyFrecency", () => {
  it("adds the boost to the resolver's score", () => {
    const store = record({}, "cmd:archive", NOW);
    const [out] = applyFrecency([{ id: "cmd:archive", score: 10 }], store, NOW);
    expect(out!.score).toBeGreaterThan(10);
  });

  it("leaves unknown results untouched, object identity included", () => {
    const result = { id: "cmd:never", score: 5 };
    const [out] = applyFrecency([result], {}, NOW);
    expect(out).toBe(result);
  });

  it("does not reorder a clear textual winner", () => {
    // "archive" typed exactly, versus a heavily-used unrelated command.
    const store: FrecencyStore = { "cmd:snooze": Array(30).fill(NOW) };
    const ranked = applyFrecency(
      [
        { id: "cmd:archive", score: 40 },
        { id: "cmd:snooze", score: 2 },
      ],
      store,
      NOW,
    ).sort((a, b) => (b.score ?? 0) - (a.score ?? 0));

    expect(ranked[0]!.id).toBe("cmd:archive");
  });
});

describe("topEntries", () => {
  it("offers the most-used first for an empty query", () => {
    let store: FrecencyStore = {};
    for (let i = 0; i < 5; i++) store = record(store, "cmd:archive", NOW - i);
    store = record(store, "cmd:snooze", NOW);

    const top = topEntries(store, NOW);
    expect(top[0]!.id).toBe("cmd:archive");
    expect(top[1]!.id).toBe("cmd:snooze");
  });

  it("omits entries that have decayed to nothing", () => {
    const store: FrecencyStore = { ghost: [NOW - 200 * DAY] };
    expect(topEntries(store, NOW)).toEqual([]);
  });
});

describe("persistence", () => {
  function fakeStorage(): Storage {
    const map = new Map<string, string>();
    return {
      get length() {
        return map.size;
      },
      clear: () => map.clear(),
      getItem: (k) => map.get(k) ?? null,
      key: (i) => [...map.keys()][i] ?? null,
      removeItem: (k) => void map.delete(k),
      setItem: (k, v) => void map.set(k, v),
    };
  }

  it("round-trips", () => {
    const storage = fakeStorage();
    const store = record({}, "cmd:archive", NOW);
    save(store, storage);
    expect(load(storage)).toEqual(store);
  });

  it("survives corrupt storage rather than breaking the palette", () => {
    const storage = fakeStorage();
    storage.setItem("mach.palette.frecency.v1", "{not json");
    expect(load(storage)).toEqual({});
  });

  it("ignores entries of the wrong shape", () => {
    const storage = fakeStorage();
    storage.setItem(
      "mach.palette.frecency.v1",
      JSON.stringify({ good: [1, 2], bad: "nope", worse: [1, "two"] }),
    );
    expect(load(storage)).toEqual({ good: [1, 2] });
  });

  it("does nothing when storage is unavailable", () => {
    expect(() => save({ a: [NOW] }, undefined)).not.toThrow();
    expect(load(undefined)).toEqual({});
  });
});
