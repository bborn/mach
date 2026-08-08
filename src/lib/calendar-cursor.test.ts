import { describe, expect, it } from "vitest";
import type { CalendarEvent } from "@/types";
import { arrowCursor, dayKey, inReadingOrder, matchEvents, stepCursor } from "./calendar-cursor";
import { HOUR } from "./time";

let nextId = 1;
function at(dayOffset: number, hour: number, title = "Event"): CalendarEvent {
  const start = new Date(2026, 7, 3 + dayOffset, hour).getTime();
  return {
    id: nextId++,
    calendarId: "primary",
    accountId: 1,
    title,
    start,
    end: start + HOUR,
    allDay: false,
    attendees: [],
  };
}

function ids(events: readonly CalendarEvent[]): number[] {
  return events.map((e) => e.id);
}

describe("reading order", () => {
  it("sorts by day and then by time within the day", () => {
    const late = at(0, 17);
    const early = at(0, 9);
    const tomorrow = at(1, 8);
    expect(ids(inReadingOrder([tomorrow, late, early]))).toEqual([early.id, late.id, tomorrow.id]);
  });

  it("puts an all-day row in its own day rather than the previous evening", () => {
    // The store pins all-day events to UTC midnight, which is the *previous*
    // evening anywhere west of Greenwich — so a raw timestamp sort files a
    // Tuesday holiday under Monday, and the down arrow walks the wrong column.
    const holiday: CalendarEvent = {
      ...at(1, 0, "Holiday"),
      allDay: true,
      start: Date.UTC(2026, 7, 4),
      end: Date.UTC(2026, 7, 5),
    };
    const mondayEvening = at(0, 20);
    const tuesdayMorning = at(1, 9);
    expect(dayKey(holiday)).toBe(20260804);
    expect(ids(inReadingOrder([tuesdayMorning, mondayEvening, holiday]))).toEqual([
      mondayEvening.id,
      holiday.id,
      tuesdayMorning.id,
    ]);
  });
});

describe("stepCursor", () => {
  it("adopts an end of the grid when nothing is selected", () => {
    const rows = [at(0, 9), at(1, 9)];
    expect(stepCursor(rows, null, 1)).toEqual({ kind: "event", id: rows[0].id });
    expect(stepCursor(rows, null, -1)).toEqual({ kind: "event", id: rows[1].id });
  });

  it("walks reading order one event at a time", () => {
    const rows = [at(0, 9), at(0, 14), at(2, 10)];
    expect(stepCursor(rows, rows[0].id, 1)).toEqual({ kind: "event", id: rows[1].id });
    expect(stepCursor(rows, rows[1].id, 1)).toEqual({ kind: "event", id: rows[2].id });
  });

  it("asks to page rather than dead-ending at the edge", () => {
    const rows = [at(0, 9), at(1, 9)];
    expect(stepCursor(rows, rows[1].id, 1)).toEqual({ kind: "page", delta: 1, edge: "first" });
    expect(stepCursor(rows, rows[0].id, -1)).toEqual({ kind: "page", delta: -1, edge: "last" });
  });

  it("does nothing at all on an empty week", () => {
    expect(stepCursor([], null, 1)).toEqual({ kind: "none" });
  });
});

describe("arrowCursor — up and down", () => {
  it("moves down a column in time order", () => {
    const rows = [at(0, 9), at(0, 11), at(0, 15)];
    expect(arrowCursor(rows, rows[0].id, "down")).toEqual({ kind: "event", id: rows[1].id });
    expect(arrowCursor(rows, rows[2].id, "up")).toEqual({ kind: "event", id: rows[1].id });
  });

  it("falls through to the next day that has anything on it", () => {
    // A Tuesday with two meetings in a week with thirty must not feel broken.
    const monday = at(0, 15);
    const thursday = at(3, 9);
    expect(arrowCursor([monday, thursday], monday.id, "down")).toEqual({
      kind: "event",
      id: thursday.id,
    });
  });

  it("pages at the bottom of the last day", () => {
    const rows = [at(0, 9), at(6, 17)];
    expect(arrowCursor(rows, rows[1].id, "down")).toEqual({
      kind: "page",
      delta: 1,
      edge: "first",
    });
  });
});

describe("arrowCursor — left and right", () => {
  it("crosses to the nearest event on another day at about the same time", () => {
    const monday = at(0, 9, "Standup");
    const tuesdayEarly = at(1, 9, "Standup");
    const tuesdayLate = at(1, 16, "Retro");
    const move = arrowCursor([monday, tuesdayLate, tuesdayEarly], monday.id, "right");
    expect(move).toEqual({ kind: "event", id: tuesdayEarly.id });
  });

  it("skips an empty day rather than costing a keypress for it", () => {
    const monday = at(0, 9);
    const thursday = at(3, 10);
    expect(arrowCursor([monday, thursday], monday.id, "right")).toEqual({
      kind: "event",
      id: thursday.id,
    });
  });

  it("keeps the earlier event when two are equally close in time", () => {
    const monday = at(0, 12);
    const early = at(1, 11);
    const late = at(1, 13);
    expect(arrowCursor([monday, late, early], monday.id, "right")).toEqual({
      kind: "event",
      id: early.id,
    });
  });

  it("pages off either end of the week", () => {
    const rows = [at(0, 9), at(4, 9)];
    expect(arrowCursor(rows, rows[1].id, "right")).toEqual({
      kind: "page",
      delta: 1,
      edge: "first",
    });
    expect(arrowCursor(rows, rows[0].id, "left")).toEqual({
      kind: "page",
      delta: -1,
      edge: "last",
    });
  });

  it("adopts a cursor from the end the key points away from", () => {
    const rows = [at(0, 9), at(4, 9)];
    expect(arrowCursor(rows, null, "right")).toEqual({ kind: "event", id: rows[0].id });
    expect(arrowCursor(rows, null, "left")).toEqual({ kind: "event", id: rows[1].id });
  });

  it("stays where it is when the cursor is on a day of its own", () => {
    const only = at(0, 9);
    expect(arrowCursor([only], only.id, "right")).toEqual({
      kind: "page",
      delta: 1,
      edge: "first",
    });
  });
});

describe("matchEvents", () => {
  const rows = [
    { ...at(0, 9, "Standup"), location: "Room 2" },
    { ...at(1, 10, "Design review") },
    { ...at(2, 11, "Misunderstanding sync") },
    { ...at(3, 12, "1:1 with Ada") },
  ];

  it("says nothing matches an empty query rather than everything", () => {
    expect(matchEvents(rows, "")).toEqual([]);
    expect(matchEvents(rows, "   ")).toEqual([]);
  });

  it("prefers a prefix, then a word boundary, then anything", () => {
    // "stand" heads "Standup" and is buried inside "Misunderstanding".
    expect(ids(matchEvents(rows, "stand"))).toEqual([rows[0].id, rows[2].id]);
  });

  it("finds a word in the middle of a title", () => {
    expect(ids(matchEvents(rows, "review"))).toEqual([rows[1].id]);
  });

  it("searches the location too, for the meeting you remember by its room", () => {
    expect(ids(matchEvents(rows, "room 2"))).toEqual([rows[0].id]);
  });

  it("is case-insensitive", () => {
    expect(ids(matchEvents(rows, "DESIGN"))).toEqual([rows[1].id]);
  });

  it("orders equally good matches the way they are drawn", () => {
    const first = at(0, 9, "Sync");
    const second = at(2, 9, "Sync");
    expect(ids(matchEvents([second, first], "sync"))).toEqual([first.id, second.id]);
  });

  it("treats a query full of punctuation as text, not as a pattern", () => {
    const odd = at(0, 8, "Budget (Q3)");
    expect(ids(matchEvents([odd], "(q3)"))).toEqual([odd.id]);
  });

  it("returns nothing rather than throwing when nothing matches", () => {
    expect(matchEvents(rows, "zzzz")).toEqual([]);
  });
});
