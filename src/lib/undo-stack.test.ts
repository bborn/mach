import { describe, expect, it } from "vitest";

import type { Command, CommandResult } from "./data";
import {
  MAX_DEPTH,
  describeRedo,
  describeUndo,
  emptyUndo,
  hidesThreads,
  peekRedo,
  peekUndo,
  popRedo,
  popUndo,
  pushUndo,
  recordUndo,
  redoSteps,
  refineRedo,
  refineUndo,
  restoreUndo,
  restoresThreads,
  runRedo,
  runUndo,
  undoSteps,
  type UndoHost,
  type UndoPlace,
  type UndoState,
} from "./undo-stack";

const NOW = 1_800_000_000_000;

const archive = (ids: number[]): Command => ({ kind: "archive", threadIds: ids });
const unarchive = (ids: number[]): Command => ({ kind: "unarchive", threadIds: ids });
const label = (ids: number[], add: boolean): Command => ({
  kind: "label",
  threadIds: ids,
  labelId: "Label_7",
  add,
});

function ok(applied: number[], undo?: Command): CommandResult {
  return { ok: true, message: "", applied, failed: [], undo };
}

function failed(undo?: Command): CommandResult {
  return {
    ok: false,
    message: "Google is rate limiting",
    applied: [],
    failed: [{ ids: [1], kind: "rateLimited", message: "", retriable: true, rolledBack: true }],
    undo,
  };
}

/** A list standing at `threadId`, with `ticked` rows selected. */
function at(threadId: number | null, ticked: number[] = []): UndoPlace {
  return {
    threadId,
    selection: { ids: ticked, anchor: ticked[0] ?? threadId, base: ticked },
    labelId: "INBOX",
    accountId: null,
  };
}

/**
 * A stand-in for the app, recording everything a traversal asks of it in the
 * order it asks — which is the half of undo that ordering bugs live in.
 *
 * The cursor is modelled, because where it ends up is now part of what a
 * traversal does. The fake takes every place it is handed; the real host
 * refuses the ones it cannot reach, which is a question about a list this file
 * has none of — see `useMach.undo-cursor.test.tsx`.
 */
function fakeHost(
  initial: UndoState,
  answer: (command: Command) => CommandResult | null = () => ok([1]),
  standing: UndoPlace = at(null),
) {
  let state = initial;
  let here = standing;
  const events: string[] = [];
  const ran: Command[] = [];
  const host: UndoHost = {
    read: () => state,
    write: (next) => {
      state = next;
    },
    execute: async (command) => {
      ran.push(command);
      events.push(`run:${command.kind}`);
      return answer(command);
    },
    restore: (ids) => void events.push(`restore:${ids.join(",")}`),
    hide: (ids) => void events.push(`hide:${ids.join(",")}`),
    project: (commands) => void events.push(`project:${commands.map((c) => c.kind).join(",")}`),
    place: () => here,
    returnTo: (place, arriving) => {
      events.push(`return:${place ? place.threadId : "none"}/${arriving.join(",")}`);
      if (place) here = place;
    },
    say: (message) => void events.push(`say:${message}`),
  };
  return {
    host,
    events,
    ran,
    /** Where the cursor is now — the whole point of the two methods above. */
    get cursor() {
      return here.threadId;
    },
    get ticked() {
      return here.selection.ids;
    },
    get state() {
      return state;
    },
  };
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
    expect(restoresThreads({ kind: "notSpam", threadIds: [1] })).toBe(true);
  });

  it("is false for the ones that do not", () => {
    expect(restoresThreads(archive([1]))).toBe(false);
    expect(restoresThreads({ kind: "markRead", threadIds: [1], read: true })).toBe(false);
  });
});

describe("hidesThreads", () => {
  it("is true for the commands that take rows off the list", () => {
    expect(hidesThreads(archive([1]))).toBe(true);
    expect(hidesThreads({ kind: "trash", threadIds: [1] })).toBe(true);
    expect(hidesThreads({ kind: "snooze", threadIds: [1], until: 5 })).toBe(true);
    expect(hidesThreads({ kind: "reportSpam", threadIds: [1] })).toBe(true);
  });

  it("is false for the ones that leave the list alone", () => {
    expect(hidesThreads(unarchive([1]))).toBe(false);
    expect(hidesThreads({ kind: "star", threadIds: [1], starred: true })).toBe(false);
  });
});

describe("describeUndo", () => {
  it("reads as a menu item", () => {
    const s = pushUndo(emptyUndo(), archive([1, 2, 3]), ok([1, 2, 3], unarchive([1, 2, 3])), "Archived 3 conversations", NOW);
    expect(describeUndo(peekUndo(s))).toBe("Undo archived 3 conversations");
  });

  it("lowercases the sentence, not the words inside it", () => {
    // The calendar really does label an entry `Created “Lunch with Dana”`, and
    // "Undo created “lunch with dana”" is a different, worse sentence.
    const s = recordUndo(emptyUndo(), "Created “Lunch with Dana”", archive([1]), NOW);
    expect(describeUndo(peekUndo(s))).toBe("Undo created “Lunch with Dana”");
  });

  it("is null when there is nothing to undo", () => {
    expect(describeUndo(null)).toBeNull();
    expect(describeRedo(null)).toBeNull();
  });
});

describe("recordUndo", () => {
  it("records an action known only by its inverse", () => {
    // What the calendar's write path and the plugin host both have: they know
    // what they did and how to take it back, and there is no CommandResult.
    const s = recordUndo(emptyUndo(), "Moved the event", { kind: "moveEvent", eventId: 4, accountId: 1, calendarId: "cal-a" }, NOW);
    expect(peekUndo(s)?.label).toBe("Moved the event");
  });

  it("refuses to guess how to re-apply it", () => {
    // Redo is learned from the undo's own answer, never invented here.
    const s = recordUndo(emptyUndo(), "Archived", unarchive([1]), NOW);
    expect(peekUndo(s)?.original).toBeUndefined();
    expect(redoSteps(peekUndo(s)!)).toEqual([]);
  });

  it("ignores a group that turned out to be empty", () => {
    expect(recordUndo(emptyUndo(), "Did nothing", [], NOW).done).toHaveLength(0);
  });
});

describe("steps", () => {
  it("unwinds a group backwards", () => {
    // The action labelled and *then* archived; unarchiving before removing the
    // label would put the thread back with the label still on it.
    const s = recordUndo(emptyUndo(), "Filed", [label([1], false), unarchive([1])], NOW);
    expect(undoSteps(peekUndo(s)!)).toEqual([unarchive([1]), label([1], false)]);
  });

  it("keeps a single command single", () => {
    const s = recordUndo(emptyUndo(), "Archived", unarchive([1]), NOW);
    expect(undoSteps(peekUndo(s)!)).toEqual([unarchive([1])]);
  });
});

describe("refining an entry", () => {
  it("teaches the undone entry how to re-apply itself", () => {
    let s = recordUndo(emptyUndo(), "Archived", unarchive([1]), NOW);
    const id = peekUndo(s)!.id;
    s = popUndo(s)!.state;
    s = refineRedo(s, id, [archive([1])]);
    expect(redoSteps(peekRedo(s)!)).toEqual([archive([1])]);
  });

  it("re-reverses a refined group, so undo → redo → undo keeps its order", () => {
    let s = recordUndo(emptyUndo(), "Filed", [label([1], false), unarchive([1])], NOW);
    const id = peekUndo(s)!.id;
    s = popUndo(s)!.state;
    // Collected in the order the undo dispatched them.
    s = refineRedo(s, id, [archive([1]), label([1], true)]);
    expect(redoSteps(peekRedo(s)!)).toEqual([label([1], true), archive([1])]);
  });

  it("teaches the redone entry how to take itself back", () => {
    let s = recordUndo(emptyUndo(), "Archived", unarchive([1]), NOW);
    const id = peekUndo(s)!.id;
    s = popUndo(s)!.state;
    s = refineRedo(s, id, [archive([1])]);
    s = popRedo(s)!.state;
    s = refineUndo(s, id, [unarchive([1, 2])]);
    expect(undoSteps(peekUndo(s)!)).toEqual([unarchive([1, 2])]);
  });
});

describe("runUndo", () => {
  const archived = () =>
    pushUndo(emptyUndo(), archive([1, 2]), ok([1, 2], unarchive([1, 2])), "Archived 2 conversations", NOW);

  it("dispatches the inverse and says what it took back", async () => {
    const app = fakeHost(archived(), () => ok([1, 2], archive([1, 2])));
    const outcome = await runUndo(app.host);

    expect(outcome.ok).toBe(true);
    expect(app.ran).toEqual([unarchive([1, 2])]);
    expect(app.events).toContain("say:Undid archived 2 conversations");
    expect(peekUndo(app.state)).toBeNull();
  });

  it("clears the list's optimistic hide before it runs anything", async () => {
    // The rows were hidden the instant they were archived. Putting the threads
    // back in the store without clearing that hide would restore them
    // invisibly — an undo that reports success and shows nothing.
    const app = fakeHost(archived(), () => ok([1, 2], archive([1, 2])));
    await runUndo(app.host);
    expect(app.events.slice(0, 2)).toEqual(["restore:1,2", "project:unarchive"]);
  });

  it("puts the whole result on screen before it dispatches anything", async () => {
    /*
     * The assertion that pins "instant". Every visible consequence of a ⌘Z —
     * the rows for every step, and the sentence saying what happened — is out
     * before the first command reaches the layer, which is what makes the
     * traversal's cost invisible however many steps it has and however slow
     * Google is.
     */
    const s = recordUndo(emptyUndo(), "Filed 3 conversations", [archive([3]), archive([2]), archive([1])], NOW);
    const app = fakeHost(s, () => ok([1]));
    await runUndo(app.host);

    const dispatched = app.events.findIndex((e) => e.startsWith("run:"));
    expect(app.events.slice(0, dispatched)).toEqual([
      "hide:1",
      "hide:2",
      "hide:3",
      // One projection carrying every step, in the order they will be
      // dispatched — not one per step, a round trip apart.
      "project:archive,archive,archive",
      // A plugin group carries no place, so this asks for nothing.
      "return:none/1,2,3",
      "say:Undid filed 3 conversations",
    ]);
  });

  it("hides the rows again when the inverse is the one that removes them", async () => {
    const s = recordUndo(emptyUndo(), "Moved back to the inbox", archive([3]), NOW);
    const app = fakeHost(s, () => ok([3], unarchive([3])));
    await runUndo(app.host);
    expect(app.events.slice(0, 2)).toEqual(["hide:3", "project:archive"]);
  });

  it("puts the entry back when the undo itself fails", async () => {
    // The affordance must not vanish because the network blipped: the whole
    // point of a stack that does not expire is that ⌘Z is still there.
    const app = fakeHost(archived(), () => failed());
    const outcome = await runUndo(app.host);

    expect(outcome.ok).toBe(false);
    expect(peekUndo(app.state)?.label).toBe("Archived 2 conversations");
    expect(peekRedo(app.state)).toBeNull();
    /*
     * The claim was made before the dispatch, and the traversal does not make a
     * second one on the way out.
     *
     * It is not the traversal's job to report the refusal, and it must not try:
     * `run` has already put what Google actually said into the same status,
     * after this message and therefore over it. A "Could not undo …" here would
     * replace the specific reason with a vaguer one. That the failure is
     * *visible* is asserted where the two halves actually meet, against the real
     * hook — see `useMach.optimistic.test.tsx`.
     */
    expect(app.events.filter((e) => e.startsWith("say:"))).toEqual([
      "say:Undid archived 2 conversations",
    ]);
    expect(app.events.indexOf("say:Undid archived 2 conversations")).toBeLessThan(
      app.events.findIndex((e) => e.startsWith("run:")),
    );
  });

  it("puts the entry back when the command never reached the layer", async () => {
    const app = fakeHost(archived(), () => null);
    await runUndo(app.host);
    expect(peekUndo(app.state)?.label).toBe("Archived 2 conversations");
  });

  it("learns the exact redo from the undo's own answer", async () => {
    /*
     * The calendar is the case that makes this necessary. The inverse of
     * "delete this event" is a create carrying the whole event, and the event
     * it creates has a new id — so the only honest way to re-delete it is the
     * inverse the create itself handed back.
     */
    const remade: Command = { kind: "deleteEvent", eventId: 99 };
    const s = recordUndo(emptyUndo(), "Deleted the event", { kind: "createEvent", accountId: 1, calendarId: "cal-a", draft: draft() }, NOW);
    const app = fakeHost(s, () => ok([99], remade));

    await runUndo(app.host);
    expect(redoSteps(peekRedo(app.state)!)).toEqual([remade]);
  });

  it("leaves redo unavailable when nothing ever said how to re-apply it", async () => {
    // A calendar entry recorded off its status message starts with no way back,
    // and an inverse that returns none of its own does not supply one.
    const s = recordUndo(emptyUndo(), "Sent an RSVP", { kind: "rsvp", eventId: 3, response: "accepted" }, NOW);
    const app = fakeHost(s, () => ok([3], undefined));
    await runUndo(app.host);
    expect(redoSteps(peekRedo(app.state)!)).toEqual([]);
  });

  it("runs a group backwards and stops at the first failure", async () => {
    const s = recordUndo(emptyUndo(), "Filed", [label([1], false), unarchive([1])], NOW);
    const app = fakeHost(s, (command) => (command.kind === "unarchive" ? ok([1]) : failed()));
    const outcome = await runUndo(app.host);

    expect(app.ran.map((c) => c.kind)).toEqual(["unarchive", "label"]);
    expect(outcome.ok).toBe(false);
    expect(peekUndo(app.state)?.label).toBe("Filed");
  });

  it("is a no-op on an empty stack", async () => {
    const app = fakeHost(emptyUndo());
    expect((await runUndo(app.host)).entry).toBeNull();
    expect(app.ran).toHaveLength(0);
  });

  it("takes the next entry when ⌘Z fires twice for one press", async () => {
    /*
     * ⌘Z can arrive from the keyboard and from the Edit menu for a single
     * press. The pop happens through the host's state before anything is
     * dispatched, so the second arrival can only ever reach the *next* entry —
     * what it must never do is run this one's inverse a second time.
     */
    let s = pushUndo(emptyUndo(), archive([1]), ok([1], unarchive([1])), "one", NOW);
    s = pushUndo(s, archive([2]), ok([2], unarchive([2])), "two", NOW + 1);
    const app = fakeHost(s, () => ok([1]));

    await Promise.all([runUndo(app.host), runUndo(app.host)]);
    expect(app.ran).toEqual([unarchive([2]), unarchive([1])]);
  });
});

/**
 * Where the cursor ends up.
 *
 * "when I archive a msg, then undo, my selection is wrong. e.g. the next
 * message down is selected. that's weird. when I'm on a message (selected) and
 * I archive, then undo, i'd expect the same message selected."
 *
 * Archiving moving the cursor onward is right and is not in question here. What
 * is: an undo that put the row back and left the hand beside it.
 */
describe("the cursor across a traversal", () => {
  /** Archived while standing on 2, which took the cursor down to 3. */
  const archivedFromTwo = () =>
    pushUndo(
      emptyUndo(),
      archive([2]),
      ok([2], unarchive([2])),
      "Archived 1 conversation",
      NOW,
      at(2),
    );

  it("puts the cursor back on the conversation that came back", async () => {
    const app = fakeHost(archivedFromTwo(), () => ok([2], archive([2])), at(3));
    await runUndo(app.host);
    expect(app.cursor).toBe(2);
  });

  it("returns a group's cursor to where the hand was, not to a member", async () => {
    /*
     * The reason the place is remembered rather than derived. Fifty archived
     * conversations restore fifty rows; picking one of them would be picking
     * whichever sorts first, which is not where anybody was standing.
     */
    const s = pushUndo(
      emptyUndo(),
      archive([4, 5, 6]),
      ok([4, 5, 6], unarchive([4, 5, 6])),
      "Archived 3 conversations",
      NOW,
      at(9, [4, 5, 6]),
    );
    const app = fakeHost(s, () => ok([4, 5, 6], archive([4, 5, 6])), at(10));
    await runUndo(app.host);

    expect(app.cursor).toBe(9);
    // And the ticks come back with it, so retrying is one keystroke.
    expect(app.ticked).toEqual([4, 5, 6]);
  });

  it("moves the cursor in the same tick as the rows, before any dispatch", async () => {
    const app = fakeHost(archivedFromTwo(), () => ok([2], archive([2])), at(3));
    await runUndo(app.host);

    const dispatched = app.events.findIndex((e) => e.startsWith("run:"));
    // After the projection — the row is being put back — and before the first
    // command goes anywhere near the network.
    expect(app.events.slice(0, dispatched)).toEqual([
      "restore:2",
      "project:unarchive",
      "return:2/2",
      "say:Undid archived 1 conversation",
    ]);
  });

  it("redo puts the cursor back where the archive left it", async () => {
    const app = fakeHost(archivedFromTwo(), () => ok([2], archive([2])), at(3));
    await runUndo(app.host);
    expect(app.cursor).toBe(2);

    await runRedo(app.host);
    // Not on the row it just archived again: where the archive itself had
    // moved the cursor to, which is the row after it.
    expect(app.cursor).toBe(3);
  });

  it("undo after a redo returns to where the cursor was before the redo", async () => {
    const app = fakeHost(archivedFromTwo(), () => ok([2], archive([2])), at(3));
    await runUndo(app.host);
    await runRedo(app.host);
    await runUndo(app.host);
    expect(app.cursor).toBe(2);
  });

  it("leaves the cursor alone for an entry that never had a place", async () => {
    // The calendar's own write path and the plugin host both record themselves
    // without one. A ⌘Z over an RSVP has no business moving the mail cursor.
    const s = recordUndo(emptyUndo(), "Sent an RSVP", { kind: "rsvp", eventId: 3, response: "accepted" }, NOW);
    const app = fakeHost(s, () => ok([3]), at(7));
    await runUndo(app.host);

    expect(app.cursor).toBe(7);
    expect(app.events).toContain("return:none/");
    expect(peekRedo(app.state)?.after).toBeUndefined();
  });

  it("hands the traversal's own ids over, so a long-gone row is still reachable", async () => {
    // Undone an hour later: the list dropped the conversation long ago and the
    // unarchive about to run is the whole reason it is coming back.
    const app = fakeHost(archivedFromTwo(), () => ok([2], archive([2])), at(3));
    await runUndo(app.host);
    expect(app.events).toContain("return:2/2");
  });

  it("keeps the place when the undo is refused", async () => {
    // The invariant: a refused traversal puts the entry back whole, cursor
    // included, so the ⌘Z offered again means the same thing.
    const app = fakeHost(archivedFromTwo(), () => failed(), at(3));
    await runUndo(app.host);
    expect(peekUndo(app.state)?.place).toEqual(at(2));
  });
});

describe("runRedo", () => {
  async function undone() {
    const s = pushUndo(emptyUndo(), archive([1]), ok([1], unarchive([1])), "Archived 1 conversation", NOW);
    const app = fakeHost(s, () => ok([1], archive([1])));
    await runUndo(app.host);
    return app.state;
  }

  it("re-applies what the undo took back", async () => {
    const app = fakeHost(await undone(), () => ok([1], unarchive([1])));
    const outcome = await runRedo(app.host);

    expect(outcome.ok).toBe(true);
    expect(app.ran).toEqual([archive([1])]);
    // Said before the dispatch, exactly as ⌘Z says it: they share the path, so
    // one of them being instant and the other not would be two keys.
    expect(app.events.slice(0, 4)).toEqual([
      "hide:1",
      "project:archive",
      "return:none/1",
      "say:Redid archived 1 conversation",
    ]);
    // And ⌘Z can take it back again, with the inverse the redo just returned.
    expect(undoSteps(peekUndo(app.state)!)).toEqual([unarchive([1])]);
  });

  it("says so rather than silently doing nothing when it cannot", async () => {
    // Nothing was learned on the way through, so there is no honest redo.
    let s = recordUndo(emptyUndo(), "Archived", unarchive([1]), NOW);
    s = popUndo(s)!.state;
    const app = fakeHost(s);
    const outcome = await runRedo(app.host);

    expect(outcome.ok).toBe(false);
    expect(app.ran).toHaveLength(0);
    expect(app.events).toEqual(["say:Cannot redo archived"]);
    // The entry stays where it is rather than being quietly consumed.
    expect(peekRedo(app.state)).not.toBeNull();
  });

  it("puts the entry back when the redo fails", async () => {
    const app = fakeHost(await undone(), () => failed());
    await runRedo(app.host);
    expect(peekRedo(app.state)?.label).toBe("Archived 1 conversation");
    expect(peekUndo(app.state)).toBeNull();
  });

  it("is a no-op with nothing to redo", async () => {
    const app = fakeHost(emptyUndo());
    expect((await runRedo(app.host)).entry).toBeNull();
  });
});

/**
 * ⌘⌫ and `b`, all the way round the stack.
 *
 * Both are new keys onto old commands — `trash` and `snooze` have been in the
 * command layer since it was written — so what these guard is not the commands
 * but the claim the two features make: that they are undoable exactly the way
 * archive is, with no confirmation in front of them because ⌘Z is behind them.
 * The optimistic hide is the part worth pinning, because a trash that undoes
 * into an invisible row is a bug that reports success.
 */
describe("trash and snooze on the stack", () => {
  const trash = (ids: number[]): Command => ({ kind: "trash", threadIds: ids });
  const untrash = (ids: number[]): Command => ({ kind: "untrash", threadIds: ids });
  const snooze = (ids: number[], until: number): Command => ({
    kind: "snooze",
    threadIds: ids,
    until,
  });
  const unsnooze = (ids: number[]): Command => ({ kind: "unsnooze", threadIds: ids });

  it("records a trash with untrash as its inverse", () => {
    const s = pushUndo(
      emptyUndo(),
      trash([4, 5]),
      ok([4, 5], untrash([4, 5])),
      "Trashed 2 conversations",
      NOW,
    );
    expect(peekUndo(s)?.inverse).toEqual(untrash([4, 5]));
    expect(describeUndo(peekUndo(s))).toBe("Undo trashed 2 conversations");
  });

  it("takes a trash back, clearing the hide before it dispatches", async () => {
    const s = pushUndo(
      emptyUndo(),
      trash([4, 5]),
      ok([4, 5], untrash([4, 5])),
      "Trashed 2 conversations",
      NOW,
    );
    const app = fakeHost(s, () => ok([4, 5], trash([4, 5])));
    const outcome = await runUndo(app.host);

    expect(outcome.ok).toBe(true);
    expect(app.ran).toEqual([untrash([4, 5])]);
    // Restore first, then run: the rows left the list the instant ⌘⌫ was
    // pressed, and putting the threads back without clearing that hide would
    // restore them where nobody can see them.
    expect(app.events.slice(0, 2)).toEqual(["restore:4,5", "project:untrash"]);
    expect(app.events).toContain("say:Undid trashed 2 conversations");
    expect(peekUndo(app.state)).toBeNull();
  });

  it("re-trashes on redo, hiding the rows again", async () => {
    const s = pushUndo(emptyUndo(), trash([4]), ok([4], untrash([4])), "Trashed 1 conversation", NOW);
    const undoneApp = fakeHost(s, () => ok([4], trash([4])));
    await runUndo(undoneApp.host);

    // The undo taught the entry how to re-apply itself, from the command
    // layer's own answer rather than a guess.
    expect(peekRedo(undoneApp.state)?.original).toEqual(trash([4]));

    const app = fakeHost(undoneApp.state, () => ok([4], untrash([4])));
    await runRedo(app.host);
    expect(app.ran).toEqual([trash([4])]);
    expect(app.events.slice(0, 2)).toEqual(["hide:4", "project:trash"]);
  });

  it("keeps the entry when Google refuses the untrash", async () => {
    const s = pushUndo(emptyUndo(), trash([4]), ok([4], untrash([4])), "Trashed 1 conversation", NOW);
    const app = fakeHost(s, () => failed());
    const outcome = await runUndo(app.host);

    expect(outcome.ok).toBe(false);
    // The affordance must not vanish because the network blipped.
    expect(peekUndo(app.state)?.label).toBe("Trashed 1 conversation");
  });

  it("records a snooze with the instant in its label, and unsnooze as its inverse", () => {
    const until = NOW + 86_400_000;
    const s = pushUndo(
      emptyUndo(),
      snooze([9], until),
      ok([9], unsnooze([9])),
      "Snoozed 1 conversation until Tomorrow, 8:00 AM",
      NOW,
    );
    expect(peekUndo(s)?.inverse).toEqual(unsnooze([9]));
    expect(describeUndo(peekUndo(s))).toBe(
      "Undo snoozed 1 conversation until Tomorrow, 8:00 AM",
    );
  });

  it("wakes a snoozed thread on undo and puts the row back", async () => {
    const until = NOW + 86_400_000;
    const s = pushUndo(
      emptyUndo(),
      snooze([9], until),
      ok([9], unsnooze([9])),
      "Snoozed 1 conversation until Tomorrow, 8:00 AM",
      NOW,
    );
    const app = fakeHost(s, () => ok([9], snooze([9], until)));
    const outcome = await runUndo(app.host);

    expect(outcome.ok).toBe(true);
    expect(app.ran).toEqual([unsnooze([9])]);
    expect(app.events.slice(0, 2)).toEqual(["restore:9", "project:unsnooze"]);
  });

  it("re-snoozes to the same instant on redo, not to a fresh one", async () => {
    const until = NOW + 86_400_000;
    const s = pushUndo(
      emptyUndo(),
      snooze([9], until),
      ok([9], unsnooze([9])),
      "Snoozed 1 conversation until Tomorrow, 8:00 AM",
      NOW,
    );
    const undoneApp = fakeHost(s, () => ok([9], snooze([9], until)));
    await runUndo(undoneApp.host);

    const app = fakeHost(undoneApp.state, () => ok([9], unsnooze([9])));
    await runRedo(app.host);
    // The wake time survives the round trip. A redo that recomputed "tomorrow"
    // would quietly move the thread's return by however long the user thought
    // about it.
    expect(app.ran).toEqual([snooze([9], until)]);
  });

  it("treats trash and snooze as commands that hide rows", () => {
    expect(hidesThreads(trash([1]))).toBe(true);
    expect(hidesThreads(snooze([1], NOW))).toBe(true);
    expect(restoresThreads(untrash([1]))).toBe(true);
    expect(restoresThreads(unsnooze([1]))).toBe(true);
  });
});

function draft() {
  return {
    title: "Lunch",
    startTs: NOW,
    endTs: NOW + 3_600_000,
    isAllDay: false,
    attendees: [],
    recurrence: [],
  };
}
