import { describe, expect, it } from "vitest";

import { initialUi, overlayOwnsKeyboard, uiReducer } from "./useMach";

/**
 * The reducer half of the optimistic path.
 *
 * It holds one map of guesses, keyed by thread, and knows only how to add to it
 * and take from it. What a command implies, and when the loaded list has caught
 * up with it, both live in `lib/projection.ts` and are tested there.
 * `useMach.optimistic.test.tsx` renders the two together and asserts on every
 * frame, which is the only place the timing can actually be seen.
 */
describe("holding a guess", () => {
  it("starts with none", () => {
    expect(initialUi.guesses).toEqual({});
  });

  it("records what a command did before anything confirms it", () => {
    const state = uiReducer(initialUi, {
      type: "project",
      guesses: { 1: { add: [], remove: ["INBOX"] }, 2: { add: [], remove: ["INBOX"] } },
    });
    expect(Object.keys(state.guesses)).toEqual(["1", "2"]);
  });

  it("lets a later command about the same thread replace the earlier one", () => {
    // Star, then archive. Merging the two deltas would leave the star's `add`
    // to be re-applied to a row that has since been refetched with it already
    // on — and the archive is the current statement about that thread anyway.
    const starred = uiReducer(initialUi, {
      type: "project",
      guesses: { 1: { add: ["STARRED"], remove: [] } },
    });
    const archived = uiReducer(starred, {
      type: "project",
      guesses: { 1: { add: [], remove: ["INBOX"] } },
    });
    expect(archived.guesses[1]).toEqual({ add: [], remove: ["INBOX"] });
  });

  it("drops exactly the ids it is told to", () => {
    const both = uiReducer(initialUi, {
      type: "project",
      guesses: { 1: { add: [], remove: ["INBOX"] }, 2: { add: [], remove: ["INBOX"] } },
    });
    const one = uiReducer(both, { type: "forget", threadIds: [1] });
    expect(Object.keys(one.guesses)).toEqual(["2"]);
  });

  /*
   * The settle effect judges the guesses of a committed render and dispatches
   * from a passive effect, which React flushes at the head of the *next* one. A
   * ⌘Z pressed in that gap made its own guess and then watched the archive's
   * settling delete it by id — no paint, no row, nothing said. So a settlement
   * names the guess it judged, and drops it only if it is still the one there.
   */
  it("does not drop a guess made after the settlement was judged", () => {
    const archived = uiReducer(initialUi, {
      type: "project",
      guesses: { 1: { add: [], remove: ["INBOX"] } },
    });
    const judged = archived.guesses;
    const undone = uiReducer(archived, {
      type: "project",
      guesses: { 1: { add: ["INBOX"], remove: [] } },
    });
    const after = uiReducer(undone, { type: "forget", threadIds: [1], settled: judged });
    expect(after.guesses[1]).toEqual({ add: ["INBOX"], remove: [] });
    expect(after).toBe(undone);
  });

  it("still drops the guess a settlement actually judged", () => {
    const archived = uiReducer(initialUi, {
      type: "project",
      guesses: { 1: { add: [], remove: ["INBOX"] } },
    });
    const after = uiReducer(archived, {
      type: "forget",
      threadIds: [1],
      settled: archived.guesses,
    });
    expect(after.guesses).toEqual({});
  });

  it("drops whatever is there when no guess is named — a refusal, or a traversal", () => {
    const archived = uiReducer(initialUi, {
      type: "project",
      guesses: { 1: { add: [], remove: ["INBOX"] } },
    });
    expect(uiReducer(archived, { type: "forget", threadIds: [1] }).guesses).toEqual({});
  });

  it("returns the same state when there is nothing to forget", () => {
    // The retirement effect runs on every list change; a new object every time
    // would re-render the whole app on every sync pass for nothing.
    const state = uiReducer(initialUi, { type: "forget", threadIds: [9] });
    expect(state).toBe(initialUi);
  });

  it("keeps a copy of every row a command names, so a later guess can land", () => {
    const gone = {
      id: 1,
      accountId: 1,
      subject: "s",
      snippet: "",
      participants: [],
      timestamp: 1,
      unread: false,
      starred: false,
      hasAttachment: false,
      messageCount: 1,
      labelIds: ["INBOX"],
    };
    const state = uiReducer(initialUi, {
      type: "project",
      guesses: { 1: { add: [], remove: ["INBOX"] } },
      rows: [gone],
    });
    expect(state.remembered.get(1)).toBe(gone);
  });

  /*
   * A guess retires when the list agrees with it, and the list is always a copy
   * fetched in the past — so the one it was made against is not evidence about
   * it. The stamp is what `settledGuesses` compares.
   */
  it("stamps a guess with the list it was made against, and drops the stamp with it", () => {
    const state = uiReducer(initialUi, {
      type: "project",
      guesses: { 1: { add: [], remove: ["INBOX"] } },
      listVersion: 7,
    });
    expect(state.guessedAt).toEqual({ 1: 7 });
    const after = uiReducer(state, { type: "forget", threadIds: [1] });
    expect(after.guessedAt).toEqual({});
  });

  it("guesses that an opened conversation has been read", () => {
    const state = uiReducer(initialUi, { type: "thread", threadId: 4 });
    expect(state.guesses[4]).toEqual({ add: [], remove: ["UNREAD"], unread: false });
  });

  it("does not overwrite a command's guess with the read one", () => {
    // Archiving moves the cursor onto the next row, and that row may be the one
    // just archived in a one-row list. The archive is the statement that
    // matters; a read guess written over it would put the row back.
    const archived = uiReducer(initialUi, {
      type: "project",
      guesses: { 4: { add: [], remove: ["INBOX"] } },
    });
    const moved = uiReducer(archived, { type: "thread", threadId: 4 });
    expect(moved.guesses[4]).toEqual({ add: [], remove: ["INBOX"] });
  });
});

/**
 * The gate every mode consults before it answers a key.
 *
 * Written as a function rather than as `!ui.paletteOpen && !ui.prefsOpen && …`
 * because the list version is what failed: it grew one clause per dialog, and
 * the clause for preferences was never added, so `e` archived a conversation
 * behind it.
 */
describe("who owns the keyboard", () => {
  it("gives it to the app when nothing is on screen", () => {
    expect(overlayOwnsKeyboard(initialUi)).toBe(false);
  });

  it("takes it away for any dialog, not for a list of remembered ones", () => {
    expect(overlayOwnsKeyboard({ ...initialUi, overlays: 1 })).toBe(true);
  });

  it("keeps it while a surface is stacked on another", () => {
    const inner = { ...initialUi, overlays: 2 };
    expect(overlayOwnsKeyboard(inner)).toBe(true);
    // Closing the inner one is not the same as closing both.
    expect(overlayOwnsKeyboard({ ...inner, overlays: 1 })).toBe(true);
  });
});

/**
 * Which account the sign-in dialog is signing in.
 *
 * "Add account" has nobody in mind; "Sign in again", beside a row that has lost
 * its Keychain entry, names an address. That address is the whole difference
 * between the two — it becomes Google's `login_hint` and the identity Rust
 * checks what comes back against — so a stale one is not cosmetic: it would
 * point the next sign-in at the wrong account.
 */
describe("who the sign-in dialog is for", () => {
  it("has nobody in mind to begin with", () => {
    expect(initialUi.addAccountEmail).toBeNull();
  });

  it("remembers the address a repair was started for", () => {
    const state = uiReducer(initialUi, {
      type: "addAccount",
      open: true,
      email: "bruno.bornsztein@gmail.com",
    });
    expect(state.addAccountOpen).toBe(true);
    expect(state.addAccountEmail).toBe("bruno.bornsztein@gmail.com");
  });

  it("opens for nobody in particular when adding an account", () => {
    const state = uiReducer(initialUi, { type: "addAccount", open: true });
    expect(state.addAccountEmail).toBeNull();
  });

  it("forgets the address on close, so the next sign-in cannot inherit it", () => {
    const repairing = uiReducer(initialUi, {
      type: "addAccount",
      open: true,
      email: "bruno.bornsztein@gmail.com",
    });
    const closed = uiReducer(repairing, { type: "addAccount", open: false });
    expect(closed.addAccountEmail).toBeNull();

    const adding = uiReducer(closed, { type: "addAccount", open: true });
    expect(adding.addAccountEmail).toBeNull();
  });
});
