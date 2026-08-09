import { describe, expect, it } from "vitest";
import type { CalendarEvent } from "@/types";
import { mergeDuplicates, mergeKey, normaliseTitle } from "./calendar-merge";
import { assignHues, CALENDAR_HUES, hashString, paintFor, toneFor } from "./calendar-palette";
import { oklchHue } from "./colors";

const START = new Date(2026, 7, 3, 10, 0, 0, 0).getTime();
const END = START + 3_600_000;

let nextId = 1;
function event(over: Partial<CalendarEvent> = {}): CalendarEvent {
  return {
    id: nextId++,
    calendarId: "a@x.com",
    accountId: 1,
    title: "Design review",
    start: START,
    end: END,
    allDay: false,
    attendees: [],
    ...over,
  };
}

describe("mergeDuplicates", () => {
  it("collapses the same meeting arriving on two accounts", () => {
    const rows = [
      event({ calendarId: "a@x.com", accountId: 1, rsvp: "needsAction" }),
      event({ calendarId: "b@y.com", accountId: 2, rsvp: "accepted" }),
    ];
    const merged = mergeDuplicates(rows);
    expect(merged).toHaveLength(1);
    expect(merged[0].merged).toBe(true);
    expect(merged[0].accountIds).toEqual([2, 1]);
    expect(merged[0].calendarIds).toEqual(["b@y.com", "a@x.com"]);
  });

  it("colours the merged block from the account you actually replied on", () => {
    const rows = [
      event({ calendarId: "a@x.com", rsvp: "needsAction" }),
      event({ calendarId: "b@y.com", rsvp: "tentative" }),
    ];
    expect(mergeDuplicates(rows)[0].event.calendarId).toBe("b@y.com");
  });

  it("falls back to sidebar order when nobody has responded", () => {
    const rows = [
      event({ calendarId: "z@x.com", rsvp: "needsAction" }),
      event({ calendarId: "a@x.com", rsvp: "needsAction" }),
    ];
    const merged = mergeDuplicates(rows, { order: ["z@x.com", "a@x.com"] });
    expect(merged[0].event.calendarId).toBe("z@x.com");
  });

  it("does not merge different meetings that happen to share a time", () => {
    const rows = [event({ title: "Standup" }), event({ title: "Retro" })];
    expect(mergeDuplicates(rows)).toHaveLength(2);
  });

  it("does not merge the same title at different times", () => {
    const rows = [event(), event({ start: START + 3_600_000, end: END + 3_600_000 })];
    expect(mergeDuplicates(rows)).toHaveLength(2);
  });

  it("ignores case and stray whitespace in the title", () => {
    const rows = [
      event({ title: "Design  Review", calendarId: "a@x.com" }),
      event({ title: "design review", calendarId: "b@y.com" }),
    ];
    expect(mergeDuplicates(rows)).toHaveLength(1);
  });

  it("keeps an all-day copy separate from a timed one", () => {
    const rows = [event(), event({ allDay: true, calendarId: "b@y.com" })];
    expect(mergeDuplicates(rows)).toHaveLength(2);
  });

  it("prefers iCalUID over the title heuristic when the seam supplies it", () => {
    // The field is not in `CalendarEvent` yet; the merge reads it structurally
    // so it becomes exact the moment the backend returns it.
    const rows = [
      { ...event({ title: "Weekly sync", calendarId: "a@x.com" }), iCalUID: "uid-1" },
      { ...event({ title: "Weekly Sync (Alex)", calendarId: "b@y.com" }), iCalUID: "uid-1" },
    ] as unknown as CalendarEvent[];
    expect(mergeDuplicates(rows)).toHaveLength(1);
    expect(mergeKey(rows[0])).toContain("uid-1");
  });

  it("keeps two instances of one recurring series apart", () => {
    const rows = [
      { ...event(), iCalUID: "uid-1" },
      { ...event({ start: START + 86_400_000, end: END + 86_400_000 }), iCalUID: "uid-1" },
    ] as unknown as CalendarEvent[];
    expect(mergeDuplicates(rows)).toHaveLength(2);
  });

  it("can be turned off, and then renders every stored row", () => {
    const rows = [event({ calendarId: "a@x.com" }), event({ calendarId: "b@y.com" })];
    const off = mergeDuplicates(rows, { enabled: false });
    expect(off).toHaveLength(2);
    expect(off.every((m) => !m.merged)).toBe(true);
  });

  it("returns blocks in start order", () => {
    const rows = [
      event({ start: START + 7_200_000, end: END + 7_200_000, title: "Late" }),
      event({ title: "Early" }),
    ];
    expect(mergeDuplicates(rows).map((m) => m.event.title)).toEqual(["Early", "Late"]);
  });
});

describe("normaliseTitle", () => {
  it("only normalises what is not identity", () => {
    expect(normaliseTitle("  Weekly   Sync ")).toBe("weekly sync");
    expect(normaliseTitle("Alex’s 1:1")).toBe("alex's 1:1");
  });
});

describe("the calendar palette", () => {
  it("gives every calendar a stable hue that does not depend on load order", () => {
    const ids = ["work@x.com", "family@x.com", "holidays@x.com"];
    const a = assignHues(ids);
    const b = assignHues([...ids].reverse());
    for (const id of ids) expect(a.get(id)).toBe(b.get(id));
  });

  it("does not hand two calendars the same hue while spares remain", () => {
    const ids = Array.from({ length: CALENDAR_HUES.length }, (_, i) => `cal-${i}@x.com`);
    const hues = new Set(assignHues(ids).values());
    expect(hues.size).toBe(CALENDAR_HUES.length);
  });

  it("hashes deterministically", () => {
    expect(hashString("work@x.com")).toBe(hashString("work@x.com"));
    expect(hashString("work@x.com")).not.toBe(hashString("home@x.com"));
  });

  it("encodes status as fill treatment, never as a second hue", () => {
    const accepted = paintFor("#16a765", toneFor("accepted"), { dark: false });
    const pending = paintFor("#16a765", toneFor("needsAction"), { dark: false });
    // The unanswered block is the same hue as the accepted one, moved only in
    // lightness so it can be read as ink on the page rather than as a ground.
    expect(oklchHue(pending.timeColor)).toBeCloseTo(oklchHue(accepted.background)!, 0);
    // Page-coloured fill, dark title, hue kept for the time and the border (§6).
    expect(pending.background).toBe("var(--background)");
    expect(pending.color).toBe("var(--foreground)");
    expect(pending.border).toBeDefined();
  });

  it("pushes a past event back with opacity, keeping its hue", () => {
    const past = paintFor("#4986e7", "solid", { dark: false, past: true });
    const now = paintFor("#4986e7", "solid", { dark: false });
    expect(past.opacity).toBe(0.6);
    expect(past.background).toBe(now.background);
  });

  it("strikes a declined event through rather than recolouring it", () => {
    const declined = paintFor("#8e24aa", toneFor("declined"), { dark: false });
    expect(declined.strikethrough).toBe(true);
    expect(declined.background).toBe("var(--background)");
  });
});
