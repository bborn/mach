import { describe, expect, it } from "vitest";

import type { Command, CommandResult } from "./data";
import {
  MAX_DEPTH,
  describeUndo,
  emptyUndo,
  peekRedo,
  peekUndo,
  popRedo,
  popUndo,
  pushUndo,
  restoreUndo,
  restoresThreads,
} from "./undo-stack";

const NOW = 1_800_000_000_000;

const archive = (ids: number[]): Command => ({ kind: "archive", threadIds: ids });
const unarchive = (ids: number[]): Command => ({ kind: "unarchive", threadIds: ids });

function ok(applied: number[], undo?: Command): CommandResult {
  return { ok: true, message: "", applied, failed: [], undo };
}

describe("pushUndo", () => {
  it("records an action that has an inverse", () => {
    const s = pushUndo(emptyUndo(), archive([1]), ok([1], unarchive([1])), "Archived 1 conversation", NOW);
    expect(peekUndo(s)?.inverse).toEqual(unarchive([1]));
  });

  it("ignores a command with no inverse", () => {
    // Un-snoozing threads with different wake times cannot reverse to one
    // snooze, so the layer returns none — and an entry that cannot act is
    // worse than no entry.
    const s = pushUndo(emptyUndo(), archive([1]), ok([1], undefined), "Unsnoozed", NOW);
    expect(peekUndo(s)).toBeNull();
  });

  it("ignores an action that changed nothing", () => {
    const s = pushUndo(emptyUndo(), archive([1]), ok([], unarchive([])), "Archived", NOW);
    expect(peekUndo(s)).toBeNull();
  });

  it("records a partial failure, since the inverse covers only what applied", () => {
    const result: CommandResult = {
      ok: false,
      message: "3 failed",
      applied: [1, 2],
      failed: [{ ids: [3], kind: "rateLimited", message: "", retriable: true, rolledBack: true }],
      undo: unarchive([1, 2]),
    };
    const s = pushUndo(emptyUndo(), archive([1, 2, 3]), result, "Archived 2 conversations", NOW);
    // Undoing must not resurrect thread 3, which never moved.
    expect(peekUndo(s)?.inverse).toEqual(unarchive([1, 2]));
  });

  it("forks the timeline — a new action drops the redo side", () => {
    let s = pushUndo(emptyUndo(), archive([1]), ok([1], unarchive([1])), "Archived", NOW);
    s = popUndo(s)!.state;
    expect(peekRedo(s)).not.toBeNull();

    s = pushUndo(s, archive([2]), ok([2], unarchive([2])), "Archived", NOW);
    expect(peekRedo(s)).toBeNull();
  });

  it("keeps the stack bounded, dropping the oldest", () => {
    let s = emptyUndo();
    for (let i = 0; i < MAX_DEPTH + 10; i++) {
      s = pushUndo(s, archive([i]), ok([i], unarchive([i])), `Archived ${i}`, NOW + i);
    }
    expect(s.done).toHaveLength(MAX_DEPTH);
    expect(peekUndo(s)?.label).toBe(`Archived ${MAX_DEPTH + 9}`);
  });
});

describe("undo and redo", () => {
  it("moves the entry across on undo, so holding the key cannot fire twice", () => {
    const s0 = pushUndo(emptyUndo(), archive([1]), ok([1], unarchive([1])), "Archived", NOW);
    const first = popUndo(s0)!;
    expect(first.entry.inverse).toEqual(unarchive([1]));
    // The state transition happens up front, not on success.
    expect(popUndo(first.state)).toBeNull();
    expect(peekRedo(first.state)?.id).toBe(first.entry.id);
  });

  it("re-applies the original on redo", () => {
    let s = pushUndo(emptyUndo(), archive([7]), ok([7], unarchive([7])), "Archived", NOW);
    s = popUndo(s)!.state;
    const redo = popRedo(s)!;
    expect(redo.entry.original).toEqual(archive([7]));
    expect(peekUndo(redo.state)?.id).toBe(redo.entry.id);
  });

  it("undoes in reverse order", () => {
    let s = emptyUndo();
    s = pushUndo(s, archive([1]), ok([1], unarchive([1])), "one", NOW);
    s = pushUndo(s, archive([2]), ok([2], unarchive([2])), "two", NOW + 1);

    const a = popUndo(s)!;
    expect(a.entry.label).toBe("two");
    const b = popUndo(a.state)!;
    expect(b.entry.label).toBe("one");
  });

  it("is a no-op on an empty stack", () => {
    expect(popUndo(emptyUndo())).toBeNull();
    expect(popRedo(emptyUndo())).toBeNull();
  });

  it("puts the entry back when the undo itself fails", () => {
    // The affordance must not vanish because the network blipped.
    const s0 = pushUndo(emptyUndo(), archive([1]), ok([1], unarchive([1])), "Archived", NOW);
    const { state, entry } = popUndo(s0)!;
    const restored = restoreUndo(state, entry);

    expect(peekUndo(restored)?.id).toBe(entry.id);
    expect(peekRedo(restored)).toBeNull();
  });
});

describe("restoresThreads", () => {
  it("is true for the inverses that put threads back on screen", () => {
    // The list hides a thread optimistically on archive, so undo has to clear
    // that hide too or the thread returns to the store and stays invisible.
    expect(restoresThreads(unarchive([1]))).toBe(true);
    expect(restoresThreads({ kind: "untrash", threadIds: [1] })).toBe(true);
    expect(restoresThreads({ kind: "unsnooze", threadIds: [1] })).toBe(true);
  });

  it("is false for the ones that do not", () => {
    expect(restoresThreads(archive([1]))).toBe(false);
    expect(restoresThreads({ kind: "markRead", threadIds: [1], read: true })).toBe(false);
  });
});

describe("describeUndo", () => {
  it("reads as a menu item", () => {
    const s = pushUndo(emptyUndo(), archive([1, 2, 3]), ok([1, 2, 3], unarchive([1, 2, 3])), "Archived 3 conversations", NOW);
    expect(describeUndo(peekUndo(s))).toBe("Undo archived 3 conversations");
  });

  it("is null when there is nothing to undo", () => {
    expect(describeUndo(null)).toBeNull();
  });
});
