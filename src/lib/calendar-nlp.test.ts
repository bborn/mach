import { describe, expect, it } from "vitest";
import { describeParsed, matchCalendar, parseEventText } from "./calendar-nlp";

// A Monday, 09:00 local, so "tomorrow" and "Thursday" are unambiguous.
const NOW = new Date(2026, 7, 3, 9, 0, 0, 0).getTime();

const CALENDARS = [
  { id: "work@x.com", name: "Work" },
  { id: "personal@x.com", name: "Alex Rivera" },
  { id: "fam@x.com", name: "Family" },
];

function parse(text: string) {
  return parseEventText(text, { now: NOW, calendars: CALENDARS });
}

describe("parseEventText", () => {
  it("takes the brief's fastest path in one line", () => {
    const parsed = parse("Standup tomorrow 2pm /w");
    expect(parsed.title).toBe("Standup");
    expect(parsed.calendarName).toBe("Work");
    expect(parsed.allDay).toBe(false);
    const start = new Date(parsed.start!);
    expect(start.getDate()).toBe(4);
    expect(start.getHours()).toBe(14);
    // No explicit end: the 30-minute default from §3.
    expect(parsed.end! - parsed.start!).toBe(30 * 60_000);
  });

  it("separates a place from a time when both use 'at'", () => {
    const parsed = parse("Grocery shopping at Wegmans Thursday at 5pm");
    expect(parsed.title).toBe("Grocery shopping");
    expect(parsed.location).toBe("Wegmans");
    expect(new Date(parsed.start!).getHours()).toBe(17);
    expect(new Date(parsed.start!).getDay()).toBe(4);
  });

  it("pulls invitees out of 'with'", () => {
    const parsed = parse("Lunch with Matthew at 1:30 Monday");
    expect(parsed.invitees).toEqual(["Matthew"]);
    expect(parsed.title).toBe("Lunch");
    expect(new Date(parsed.start!).getHours()).toBe(13);
    expect(new Date(parsed.start!).getMinutes()).toBe(30);
  });

  it("splits several invitees", () => {
    const parsed = parse("Sync with Ayla and Riley tomorrow at 10am");
    expect(parsed.invitees).toEqual(["Ayla", "Riley"]);
    expect(parsed.title).toBe("Sync");
  });

  it("reads a multi-day range as all-day, through the last day", () => {
    const parsed = parse("Family vacation from August 9-18");
    expect(parsed.title).toBe("Family vacation");
    expect(parsed.allDay).toBe(true);
    expect(new Date(parsed.start!).getDate()).toBe(9);
    // Inclusive of the 18th: the exclusive end lands on the 19th.
    expect(new Date(parsed.end!).getDate()).toBe(19);
  });

  it("reads an alert offset and keeps it out of the title", () => {
    const parsed = parse("Staff meeting Tuesday 2pm alert 20 min");
    expect(parsed.alertMinutes).toBe(20);
    expect(parsed.title).toBe("Staff meeting");
    expect(new Date(parsed.start!).getHours()).toBe(14);
  });

  it("converts alert units to minutes", () => {
    expect(parse("Flight tomorrow 6am alert 2 hours").alertMinutes).toBe(120);
    expect(parse("Renew passport Friday alert 3 days").alertMinutes).toBe(4320);
  });

  it("records recurrence without losing the weekday", () => {
    const parsed = parse("Soccer practice every Tuesday at 6");
    expect(parsed.recurrence).toBe("every tuesday");
    expect(parsed.title).toBe("Soccer practice");
    expect(new Date(parsed.start!).getDay()).toBe(2);
  });

  it("handles a yearly recurrence on a numeric date", () => {
    const parsed = parse("Sam's birthday every year on 5/16");
    expect(parsed.recurrence).toBe("every year");
    expect(parsed.title).toBe("Sam's birthday");
    expect(new Date(parsed.start!).getMonth()).toBe(4);
    expect(new Date(parsed.start!).getDate()).toBe(16);
  });

  it("matches a calendar on an initial, and says so when it cannot", () => {
    expect(parse("Thing tomorrow 9am /f").calendarName).toBe("Family");
    const miss = parse("Thing tomorrow 9am /zz");
    expect(miss.calendarId).toBeUndefined();
    expect(miss.unknownCalendar).toBe("zz");
    expect(miss.title).toBe("Thing");
  });

  it("survives a sentence with no date at all", () => {
    const parsed = parse("Think about the roadmap");
    expect(parsed.start).toBeNull();
    expect(parsed.title).toBe("Think about the roadmap");
    expect(describeParsed(parsed)).toContain("no date yet");
  });

  it("honours an explicit range", () => {
    const parsed = parse("Deep work tomorrow from 9am to 11am");
    expect(parsed.end! - parsed.start!).toBe(2 * 60 * 60_000);
    expect(parsed.title).toBe("Deep work");
  });

  it("looks forward, never backward, for a bare weekday", () => {
    const parsed = parse("Retro Monday 3pm");
    expect(parsed.start!).toBeGreaterThan(NOW);
  });
});

describe("describeParsed", () => {
  it("puts everything the field promised on one line", () => {
    const line = describeParsed(parse("Lunch with Matthew at Zuni Thursday at 1pm alert 15 min /w"));
    expect(line).toContain("1:00 PM");
    expect(line).toContain("at Zuni");
    expect(line).toContain("with Matthew");
    expect(line).toContain("alert 15 min");
    expect(line).toContain("Work");
  });
});

describe("matchCalendar", () => {
  it("prefers a whole-name prefix over a word inside another name", () => {
    expect(matchCalendar("a", CALENDARS)?.name).toBe("Alex Rivera");
    expect(matchCalendar("wor", CALENDARS)?.name).toBe("Work");
  });

  it("falls back to the calendar id", () => {
    expect(matchCalendar("fam@", CALENDARS)?.id).toBe("fam@x.com");
  });

  it("returns undefined rather than guessing", () => {
    expect(matchCalendar("qq", CALENDARS)).toBeUndefined();
  });
});
