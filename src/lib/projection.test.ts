import { describe, expect, it } from "vitest";
import type { CalendarEvent, Thread } from "@/types";
import {
  applyEventGuess,
  applyEventGuesses,
  applyGuess,
  entersMailbox,
  isPendingEvent,
  leavesMailbox,
  leavingIds,
  pendingEventId,
  placeholderEvent,
  project,
  projectEvent,
  returningRows,
  settledEventGuesses,
  settledGuesses,
  settledPendingEvents,
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

  /*
   * Deleting in Drafts. `commands::drafts` discards the draft through
   * `drafts.delete` — no label delta can express that — so the conversation
   * loses `DRAFT` as well, and the row has to leave the mailbox on the
   * keystroke rather than a refetch later.
   */
  it("takes DRAFT away too, for a conversation that is holding one", () => {
    const rows = [row(1, { labelIds: ["DRAFT"] })];
    expect(project({ kind: "trash", threadIds: [1] }, rows)).toEqual({
      1: { add: ["TRASH"], remove: ["INBOX", "DRAFT"] },
    });
    expect(leavingIds({ kind: "trash", threadIds: [1] }, rows, "DRAFT")).toEqual([1]);
  });

  it("says nothing about DRAFT for a conversation that has no draft", () => {
    // Otherwise an ordinary trash would read as having touched a draft, and
    // `settledGuesses` would be waiting for a label to go that was never there.
    expect(project({ kind: "trash", threadIds: [1] }, [row(1)])).toEqual({
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

  it("gives a reported conversation SPAM and takes INBOX away", () => {
    expect(project({ kind: "reportSpam", threadIds: [1] }, [])).toEqual({
      1: { add: ["SPAM"], remove: ["INBOX"] },
    });
    // Which is what takes the row off the inbox in the keystroke's own frame.
    expect(
      leavingIds({ kind: "reportSpam", threadIds: [1] }, [row(1)], "INBOX"),
    ).toEqual([1]);
  });

  it("puts a rescued conversation back exactly where it was", () => {
    // Undo's form. The thread was starred and in a label when it was reported,
    // and all of that comes back — not a bare INBOX.
    const rows = [row(1, { labelIds: ["SPAM", "STARRED", "Receipts"] })];
    expect(
      project(
        {
          kind: "notSpam",
          threadIds: [1],
          restore: [
            { threadId: 1, labelIds: ["INBOX", "STARRED", "Receipts"], isUnread: true },
          ],
        },
        rows,
      ),
    ).toEqual({ 1: { add: ["INBOX"], remove: ["SPAM"], unread: true } });
  });

  it("falls back to the inbox for a notSpam with no prior state", () => {
    expect(project({ kind: "notSpam", threadIds: [1], restore: undefined }, [])).toEqual({
      1: { add: ["INBOX"], remove: ["SPAM"] },
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

  it("falls back the way the backend does when there is no restore state", () => {
    expect(
      project({ kind: "untrash", threadIds: [1], restore: undefined }, []),
    ).toEqual({ 1: { add: ["INBOX"], remove: ["TRASH"] } });
  });

  /*
   * The redo of an archive: `commands::mail` inverts an unarchive to an
   * unarchive carrying the prior label set, and that set does *not* contain
   * INBOX. The row is off the list by then — the archive took it off a refetch
   * ago — and the guess used to fall through to the same `add: ["INBOX"]` as a
   * command with no restore state at all, which is the opposite of what the
   * command does. It agreed with nothing and was never retired.
   */
  it("states the restored set when the row is not loaded, rather than guessing INBOX", () => {
    expect(
      project(
        {
          kind: "unarchive",
          threadIds: [1],
          restore: [{ threadId: 1, labelIds: ["Receipts"], isUnread: false }],
        },
        [],
      ),
    ).toEqual({ 1: { add: ["Receipts"], remove: ["INBOX", "TRASH", "SPAM"], unread: false } });
  });

  it("says a restore-to-inbox enters the inbox, loaded row or not", () => {
    const guess = project(
      {
        kind: "unarchive",
        threadIds: [1],
        restore: [{ threadId: 1, labelIds: ["INBOX", "Receipts"], isUnread: true }],
      },
      [],
    )![1]!;
    expect(entersMailbox(guess, "INBOX")).toBe(true);
    expect(leavesMailbox(guess, "INBOX")).toBe(false);
    expect(guess.remove).toEqual(["TRASH", "SPAM"]);
  });

  /*
   * `is_unread` and the `UNREAD` label come from different columns, and
   * `set_thread_state` writes both verbatim. A guess that "corrected" one of
   * them against the other would be the thing that never agreed with the row
   * the restore produced, so neither is derived from the other here.
   */
  it("carries the restore state's read flag and label set as they came", () => {
    expect(
      project(
        {
          kind: "unarchive",
          threadIds: [1],
          restore: [{ threadId: 1, labelIds: ["INBOX", "UNREAD"], isUnread: false }],
        },
        [],
      ),
    ).toEqual({ 1: { add: ["INBOX", "UNREAD"], remove: ["TRASH", "SPAM"], unread: false } });
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

  /*
   * Archive's membership is a negative, so "did the guess remove ARCHIVE" is a
   * question nothing can ever answer yes to. Filing the conversation anywhere
   * is what takes it out.
   */
  describe("the archive, whose membership is an absence", () => {
    it("takes a trashed row out of the archive", () => {
      expect(leavesMailbox({ add: ["TRASH"], remove: [] }, "ARCHIVE")).toBe(true);
    });

    it("takes an unarchived row out of the archive", () => {
      expect(leavesMailbox({ add: ["INBOX"], remove: [] }, "ARCHIVE")).toBe(true);
    });

    it("takes a row marked as spam out of the archive", () => {
      expect(leavesMailbox({ add: ["SPAM"], remove: ["INBOX"] }, "ARCHIVE")).toBe(true);
    });

    it("keeps an archived row where it already is", () => {
      // Archiving again, or starring, or reading. None of it files it anywhere.
      expect(leavesMailbox({ add: [], remove: ["INBOX"] }, "ARCHIVE")).toBe(false);
      expect(leavesMailbox({ add: ["STARRED"], remove: [] }, "ARCHIVE")).toBe(false);
    });

    it("puts a newly archived row into the archive", () => {
      expect(entersMailbox({ add: [], remove: ["INBOX"] }, "ARCHIVE")).toBe(true);
      expect(entersMailbox({ add: ["INBOX"], remove: [] }, "ARCHIVE")).toBe(false);
    });
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

  it("retires a trash when the row is gone from the archive it left", () => {
    // The archive's own version of the two above. No guess can ever "remove
    // ARCHIVE", so the rule this used to inline left the guess pending for
    // good — which is the never-retired hidden-id set all over again.
    expect(settledGuesses([], { 1: { add: ["TRASH"], remove: [] } }, "ARCHIVE")).toEqual([1]);
    expect(settledGuesses([], { 1: { add: ["STARRED"], remove: [] } }, "ARCHIVE")).toEqual([]);
  });

  it("holds on until the read state matches too", () => {
    const guess: ThreadGuess = { add: [], remove: ["UNREAD"], unread: false };
    expect(settledGuesses([row(1, { unread: true })], { 1: guess }, "INBOX")).toEqual([]);
    expect(settledGuesses([row(1, { unread: false })], { 1: guess }, "INBOX")).toEqual([1]);
  });

  it("says nothing when nothing is pending", () => {
    expect(settledGuesses([row(1)], {}, "INBOX")).toEqual([]);
  });

  /*
   * A ⌘Z dispatched while the archive it takes back was still going out made
   * `{ add: ["INBOX"] }` against a list that still had the row in the inbox.
   * That list agreed instantly and for the wrong reason — it had not heard
   * about either command — so the guess was retired and the row it had just put
   * back vanished when the archive's own refetch landed.
   */
  it("does not retire a guess against the list it was made against", () => {
    const back: ThreadGuess = { add: ["INBOX"], remove: [] };
    const stale = [row(1, { labelIds: ["INBOX"] })];
    expect(settledGuesses(stale, { 1: back }, "INBOX", { 1: 4 }, 4)).toEqual([]);
    expect(settledGuesses(stale, { 1: back }, "INBOX", { 1: 4 }, 5)).toEqual([1]);
  });

  it("decides a guess with no stamp, which is every guess made outside a command", () => {
    const guess: ThreadGuess = { add: ["STARRED"], remove: [] };
    const rows = [row(1, { labelIds: ["INBOX", "STARRED"] })];
    expect(settledGuesses(rows, { 1: guess }, "INBOX", {}, 3)).toEqual([1]);
  });
});

/*
 * The undo half of the projection, and the one the reported lag lived in.
 *
 * A guess is a delta and needs a row to land on. `list_threads` stops carrying
 * an archived conversation the moment it is refetched, so a ⌘Z arriving after
 * that had nothing to apply itself to and repainted nothing: 966ms to the row
 * against a 300ms command, measured in the real window, against 13ms for the
 * same keystroke pressed before the refetch.
 */
describe("rows a guess brings back", () => {
  const back: ThreadGuess = { add: ["INBOX"], remove: [] };

  it("draws a remembered row the list has dropped", () => {
    const gone = row(7, { labelIds: ["Receipts"] });
    const drawn = returningRows({ 7: back }, new Map([[7, gone]]), "INBOX", [], null);
    expect(drawn.map((t) => t.id)).toEqual([7]);
    expect(drawn[0]!.labelIds).toContain("INBOX");
  });

  it("leaves the loaded list to speak for a row it still carries", () => {
    const loaded = row(7, { labelIds: ["INBOX"] });
    expect(returningRows({ 7: back }, new Map([[7, loaded]]), "INBOX", [loaded], null)).toEqual([]);
  });

  it("draws nothing for a guess that does not name this mailbox", () => {
    const gone = row(7, { labelIds: ["Receipts"] });
    expect(returningRows({ 7: back }, new Map([[7, gone]]), "Receipts", [], null)).toEqual([]);
  });

  it("draws nothing for an archive, which is the direction that needs no help", () => {
    const gone = row(7);
    const leaving: ThreadGuess = { add: [], remove: ["INBOX"] };
    expect(returningRows({ 7: leaving }, new Map([[7, gone]]), "INBOX", [], null)).toEqual([]);
  });

  // A guess outlives a switch of account, and so does the memory. Without this
  // an archive taken back in one mailbox would draw its row into the next one.
  it("keeps a remembered row out of another account's list", () => {
    const gone = row(7, { accountId: 2, labelIds: ["Receipts"] });
    const remembered = new Map([[7, gone]]);
    expect(returningRows({ 7: back }, remembered, "INBOX", [], 1)).toEqual([]);
    expect(returningRows({ 7: back }, remembered, "INBOX", [], 2).map((t) => t.id)).toEqual([7]);
    expect(returningRows({ 7: back }, remembered, "INBOX", [], null).map((t) => t.id)).toEqual([7]);
  });

  it("says nothing when there is nothing pending or nothing remembered", () => {
    expect(returningRows({}, new Map([[7, row(7)]]), "INBOX", [], null)).toEqual([]);
    expect(returningRows({ 7: back }, new Map(), "INBOX", [], null)).toEqual([]);
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

/* -------------------------------------------------------------------------- */
/* The calendar half                                                           */
/* -------------------------------------------------------------------------- */

const NOON = Date.UTC(2026, 0, 15, 12, 0, 0);

function block(id: number, over: Partial<CalendarEvent> = {}): CalendarEvent {
  return {
    id,
    calendarId: "primary",
    accountId: 1,
    title: `Event ${id}`,
    start: NOON,
    end: NOON + 3_600_000,
    allDay: false,
    attendees: [],
    ...over,
  };
}

describe("what a calendar command claims", () => {
  it("claims the answer for an RSVP", () => {
    expect(projectEvent({ kind: "rsvp", eventId: 3, response: "accepted" })).toEqual({
      3: { patch: { rsvp: "accepted" } },
    });
  });

  it("claims the fields an update sets, and only those", () => {
    const guesses = projectEvent({
      kind: "updateEvent",
      eventId: 3,
      patch: { startTs: 10, endTs: 20, isAllDay: false },
    });
    expect(guesses).toEqual({ 3: { patch: { start: 10, end: 20, allDay: false } } });
  });

  /*
   * A Meet link is minted by Google and read back, and "who hears about it" is
   * not a fact about the event at all. A patch carrying only those claims
   * nothing, and claiming nothing has to be `null` rather than an empty guess —
   * an empty one would be a guess that can never be wrong and never retires.
   */
  it("claims nothing for a patch that only asks for a Meet link", () => {
    expect(
      projectEvent({ kind: "updateEvent", eventId: 3, patch: { conferencing: "meet" } }),
    ).toBeNull();
  });

  it("claims the event is gone for a delete", () => {
    expect(projectEvent({ kind: "deleteEvent", eventId: 3 })).toEqual({ 3: { gone: true } });
  });

  it("re-points a move rather than claiming the event went away", () => {
    expect(
      projectEvent({ kind: "moveEvent", eventId: 3, accountId: 2, calendarId: "work" }),
    ).toEqual({ 3: { patch: { calendarId: "work", accountId: 2 } } });
  });

  it("claims nothing for a create, which has no id yet", () => {
    expect(
      projectEvent({
        kind: "createEvent",
        accountId: 1,
        calendarId: "primary",
        draft: {
          title: "Standup",
          startTs: 1,
          endTs: 2,
          isAllDay: false,
          attendees: [],
          recurrence: [],
        },
      }),
    ).toBeNull();
  });

  it("claims nothing for a mail command", () => {
    expect(projectEvent({ kind: "archive", threadIds: [1] })).toBeNull();
  });
});

describe("an event with a guess on it", () => {
  it("keeps its identity when the guess changes nothing", () => {
    const event = block(1, { rsvp: "accepted" });
    expect(applyEventGuess(event, { patch: { rsvp: "accepted" } })).toBe(event);
  });

  it("answers for the signed-in guest as well as for the event", () => {
    const event = block(1, {
      rsvp: "needsAction",
      guests: [
        { email: "me@example.test", isSelf: true, response: "needsAction" },
        { email: "them@example.test", response: "accepted" },
      ],
    });
    const next = applyEventGuess(event, { patch: { rsvp: "declined" } });
    expect(next.rsvp).toBe("declined");
    expect(next.guests?.[0]?.response).toBe("declined");
    // And says nothing about anyone else.
    expect(next.guests?.[1]?.response).toBe("accepted");
  });

  it("is dropped from the grid when the guess says it is gone", () => {
    const rows = applyEventGuesses([block(1), block(2)], { 1: { gone: true } }, []);
    expect(rows.map((e) => e.id)).toEqual([2]);
  });

  it("passes the whole window through untouched when nothing is pending", () => {
    const events = [block(1), block(2)];
    expect(applyEventGuesses(events, {}, [])).toBe(events);
  });
});

describe("retiring an event guess", () => {
  it("retires when the store says the same thing", () => {
    const events = [block(1, { rsvp: "accepted" })];
    expect(settledEventGuesses(events, { 1: { patch: { rsvp: "accepted" } } })).toEqual([1]);
  });

  it("holds on while the store still disagrees", () => {
    const events = [block(1, { rsvp: "needsAction" })];
    expect(settledEventGuesses(events, { 1: { patch: { rsvp: "accepted" } } })).toEqual([]);
  });

  it("holds a delete while the row is still there", () => {
    expect(settledEventGuesses([block(1)], { 1: { gone: true } })).toEqual([]);
  });

  it("retires a delete once the row has gone", () => {
    expect(settledEventGuesses([block(2)], { 1: { gone: true } })).toEqual([1]);
  });
});

describe("a block drawn for a create", () => {
  it("carries the draft, under an id nothing on Google could have", () => {
    const id = pendingEventId();
    expect(isPendingEvent(id)).toBe(true);
    const drawn = placeholderEvent(
      {
        kind: "createEvent",
        accountId: 4,
        calendarId: "work",
        draft: {
          title: "Standup",
          startTs: NOON,
          endTs: NOON + 900_000,
          isAllDay: false,
          attendees: [{ name: "Someone", email: "someone@example.test" }],
          recurrence: [],
        },
      },
      id,
    );
    expect(drawn).toMatchObject({
      id,
      calendarId: "work",
      accountId: 4,
      title: "Standup",
      start: NOON,
      end: NOON + 900_000,
    });
  });

  it("retires on the exact row the command layer minted", () => {
    const pending = [{ event: block(-1, { title: "Standup" }), realId: 42 }];
    expect(settledPendingEvents([block(42, { title: "Renamed by Google" })], pending)).toEqual([-1]);
  });

  it("retires on the slot when the source could not name an id", () => {
    const pending = [{ event: block(-1, { title: "Standup" }), realId: null }];
    expect(settledPendingEvents([block(9, { title: "Standup" })], pending)).toEqual([-1]);
    expect(settledPendingEvents([block(9, { title: "Something else" })], pending)).toEqual([]);
  });

  it("is not drawn beside the row it was standing in for", () => {
    const pending = [{ event: block(-1, { title: "Standup" }), realId: 42 }];
    const rows = applyEventGuesses([block(42, { title: "Standup" })], {}, pending);
    expect(rows.map((e) => e.id)).toEqual([42]);
  });
});
