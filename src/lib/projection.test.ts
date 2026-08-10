import { describe, expect, it } from "vitest";
import type { Thread } from "@/types";
import {
  applyGuess,
  leavesMailbox,
  leavingIds,
  project,
  settledGuesses,
  suppressedIds,
  type ThreadGuess,
} from "./projection";

function row(id: number, over: Partial<Thread> = {}): Thread {
  return {
    id,
    accountId: 1,
    subject: `Conversation ${id}`,
    snippet: "snippet",
    participants: [{ name: "Someone", email: "someone@example.test" }],
    timestamp: 1_000,
    unread: false,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
    ...over,
  };
}

/**
 * Every mail command is a label delta, and it is the *same* delta the command
 * layer computes on the other side of the seam. These pin that agreement: if
 * `commands::mail` ever stops removing INBOX for an archive, this is the test
 * that has to change with it.
 */
describe("what a command is known to do", () => {
  it("takes an archived conversation out of the inbox", () => {
    expect(project({ kind: "archive", threadIds: [1] }, [])).toEqual({
      1: { add: [], remove: ["INBOX"] },
    });
  });

  it("gives a trashed conversation TRASH and takes INBOX away", () => {
    expect(project({ kind: "trash", threadIds: [1] }, [])).toEqual({
      1: { add: ["TRASH"], remove: ["INBOX"] },
    });
  });

  it("takes a snoozed conversation out of the inbox", () => {
    // The per-account `Mach/Snoozed` label the backend also applies has an id
    // only the store knows. Leaving it out costs nothing on screen and is what
    // lets the guess ever agree with the list again.
    expect(project({ kind: "snooze", threadIds: [1], until: 1 }, [])).toEqual({
      1: { add: [], remove: ["INBOX"] },
    });
  });

  it("puts a woken conversation back in the inbox", () => {
    expect(project({ kind: "unsnooze", threadIds: [1] }, [])).toEqual({
      1: { add: ["INBOX"], remove: [] },
    });
  });

  it("carries the read state as well as the label", () => {
    expect(project({ kind: "markRead", threadIds: [1], read: true }, [])).toEqual({
      1: { add: [], remove: ["UNREAD"], unread: false },
    });
    expect(project({ kind: "markRead", threadIds: [1], read: false }, [])).toEqual({
      1: { add: ["UNREAD"], remove: [], unread: true },
    });
  });

  it("stars and unstars through STARRED, the way Gmail does", () => {
    expect(project({ kind: "star", threadIds: [1], starred: true }, [])).toEqual({
      1: { add: ["STARRED"], remove: [] },
    });
    expect(project({ kind: "star", threadIds: [1], starred: false }, [])).toEqual({
      1: { add: [], remove: ["STARRED"] },
    });
  });

  it("names the label a label command names", () => {
    expect(project({ kind: "label", threadIds: [1], labelId: "Label_9", add: true }, [])).toEqual({
      1: { add: ["Label_9"], remove: [] },
    });
  });

  it("turns undo's restored label set into a delta against the row", () => {
    // The whole point of `restore`: undoing an archive puts back the labels the
    // conversation actually had, not a bare INBOX.
    const rows = [row(1, { labelIds: ["Receipts"] })];
    expect(
      project(
        {
          kind: "unarchive",
          threadIds: [1],
          restore: [{ threadId: 1, labelIds: ["INBOX", "Receipts", "Family"], isUnread: true }],
        },
        rows,
      ),
    ).toEqual({ 1: { add: ["INBOX", "Family"], remove: [], unread: true } });
  });

  it("falls back the way the backend does when the row is not loaded", () => {
    expect(
      project({ kind: "untrash", threadIds: [1], restore: undefined }, []),
    ).toEqual({ 1: { add: ["INBOX"], remove: ["TRASH"] } });
  });

  it("has nothing to say about a calendar command", () => {
    expect(project({ kind: "deleteEvent", eventId: 3 }, [])).toBeNull();
  });

  it("has nothing to say about an empty set", () => {
    expect(project({ kind: "archive", threadIds: [] }, [])).toBeNull();
  });
});

describe("drawing a guessed row", () => {
  it("shows the star the moment it is guessed", () => {
    const next = applyGuess(row(1), { add: ["STARRED"], remove: [] });
    expect(next.starred).toBe(true);
    expect(next.labelIds).toContain("STARRED");
  });

  it("puts the star back out on an unstar", () => {
    const next = applyGuess(row(1, { starred: true, labelIds: ["INBOX", "STARRED"] }), {
      add: [],
      remove: ["STARRED"],
    });
    expect(next.starred).toBe(false);
  });

  it("clears the unread mark on a read guess", () => {
    const next = applyGuess(row(1, { unread: true, labelIds: ["INBOX", "UNREAD"] }), {
      add: [],
      remove: ["UNREAD"],
      unread: false,
    });
    expect(next.unread).toBe(false);
  });

  it("leaves the star alone for a guess that says nothing about it", () => {
    const original = row(1, { starred: true, labelIds: ["INBOX", "STARRED"] });
    expect(applyGuess(original, { add: [], remove: ["INBOX"] }).starred).toBe(true);
  });

  it("returns the very same object when the guess changes nothing", () => {
    // Sixty rows re-rendering because a guess was applied to one of them is the
    // churn this exists to avoid.
    const original = row(1, { starred: true, labelIds: ["INBOX", "STARRED"] });
    expect(applyGuess(original, { add: ["STARRED"], remove: [] })).toBe(original);
  });
});

describe("whether the guess takes the row out of the mailbox on screen", () => {
  it("takes an archived row out of the inbox", () => {
    expect(leavesMailbox({ add: [], remove: ["INBOX"] }, "INBOX")).toBe(true);
  });

  it("leaves the same conversation in a label it still carries", () => {
    // The hidden-id set could not say this: it hid the row everywhere.
    expect(leavesMailbox({ add: [], remove: ["INBOX"] }, "Receipts")).toBe(false);
  });

  it("leaves a trashed row in the trash", () => {
    expect(leavesMailbox({ add: ["TRASH"], remove: ["INBOX"] }, "TRASH")).toBe(false);
  });

  it("takes an unstarred row out of the starred mailbox", () => {
    expect(leavesMailbox({ add: [], remove: ["STARRED"] }, "STARRED")).toBe(true);
  });

  it("leaves a read row where it is", () => {
    expect(leavesMailbox({ add: [], remove: ["UNREAD"], unread: false }, "INBOX")).toBe(false);
  });

  it("leaves a starred draft in the Drafts mailbox", () => {
    // Drafts is matched on `messages.is_draft` as well as on the DRAFT label,
    // so a rule that asked "is DRAFT still in the projected set" would make a
    // draft vanish from Drafts for a command about stars.
    expect(leavesMailbox({ add: ["STARRED"], remove: [] }, "DRAFT")).toBe(false);
  });
});

/**
 * When a guess stops being one.
 *
 * The first version of this answered "when the command comes back", which is
 * the wrong event: the command answering says the write landed in SQLite, and
 * says nothing about whether the list on screen has been refetched since. It
 * had not — `threads-changed` is coalesced over 600ms and then has its own
 * round trip — so the star went out for most of a second between the command
 * answering and the rows arriving. `useMach.optimistic.test.tsx` is that flash,
 * rendered; this is the rule that replaced the timing.
 */
describe("retiring a guess", () => {
  const starred: ThreadGuess = { add: ["STARRED"], remove: [] };

  it("retires the guess the loaded row now agrees with", () => {
    const rows = [row(1, { labelIds: ["INBOX", "STARRED"] }), row(2)];
    expect(settledGuesses(rows, { 1: starred }, "INBOX")).toEqual([1]);
  });

  it("holds on while the loaded row still disagrees", () => {
    expect(settledGuesses([row(1)], { 1: starred }, "INBOX")).toEqual([]);
  });

  it("holds on to a thread the loaded list does not carry", () => {
    // Changing mailbox empties the list and refills it. Dropping a star guess
    // for a row that is momentarily absent would unstar it for a round trip.
    expect(settledGuesses([], { 1: starred }, "INBOX")).toEqual([]);
  });

  it("retires an archive when the row is gone from the mailbox it left", () => {
    // Absence is the agreement here — it is exactly what the guess predicted.
    expect(settledGuesses([], { 1: { add: [], remove: ["INBOX"] } }, "INBOX")).toEqual([1]);
  });

  it("does not retire an archive from a mailbox it said nothing about", () => {
    // Viewing a label the archived conversation still carries: its absence from
    // *this* list is not the thing the guess predicted.
    expect(settledGuesses([], { 1: { add: [], remove: ["INBOX"] } }, "Receipts")).toEqual([]);
  });

  it("holds on until the read state matches too", () => {
    const guess: ThreadGuess = { add: [], remove: ["UNREAD"], unread: false };
    expect(settledGuesses([row(1, { unread: true })], { 1: guess }, "INBOX")).toEqual([]);
    expect(settledGuesses([row(1, { unread: false })], { 1: guess }, "INBOX")).toEqual([1]);
  });

  it("says nothing when nothing is pending", () => {
    expect(settledGuesses([row(1)], {}, "INBOX")).toEqual([]);
  });
});

describe("where the cursor has to go", () => {
  it("names the rows an archive takes out of the inbox", () => {
    const rows = [row(1), row(2), row(3)];
    expect(leavingIds({ kind: "archive", threadIds: [2] }, rows, "INBOX")).toEqual([2]);
  });

  it("names none when the conversation stays where it is", () => {
    // Archiving from a label the conversation still carries. The row does not
    // move, so neither should the cursor.
    const rows = [row(1, { labelIds: ["INBOX", "Receipts"] })];
    expect(leavingIds({ kind: "archive", threadIds: [1] }, rows, "Receipts")).toEqual([]);
  });

  it("names the rows an unstar takes out of the starred mailbox", () => {
    const rows = [row(1, { starred: true, labelIds: ["INBOX", "STARRED"] })];
    expect(
      leavingIds({ kind: "star", threadIds: [1], starred: false }, rows, "STARRED"),
    ).toEqual([1]);
  });

  it("names none for a star in the inbox", () => {
    const rows = [row(1)];
    expect(leavingIds({ kind: "star", threadIds: [1], starred: true }, rows, "INBOX")).toEqual([]);
  });

  it("names none for a calendar command", () => {
    expect(leavingIds({ kind: "deleteEvent", eventId: 1 }, [row(1)], "INBOX")).toEqual([]);
  });
});

describe("what the rail's badge subtracts", () => {
  it("counts a conversation leaving the inbox", () => {
    expect([...suppressedIds({ 1: { add: [], remove: ["INBOX"] } }, "INBOX")]).toEqual([1]);
  });

  it("counts one that has just been read", () => {
    const guesses = { 5: { add: [], remove: ["UNREAD"], unread: false } };
    expect([...suppressedIds(guesses, "INBOX")]).toEqual([5]);
  });

  it("does not count a star", () => {
    expect([...suppressedIds({ 1: { add: ["STARRED"], remove: [] } }, "INBOX")]).toEqual([]);
  });
});
