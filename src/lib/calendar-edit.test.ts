import { describe, expect, it } from "vitest";
import type { CalendarEvent } from "@/types";
import {
  attendeesField,
  dateField,
  duplicateDraft,
  emptyForm,
  formDraft,
  formFromEvent,
  formPatch,
  formTimes,
  isDirty,
  isFormError,
  looksRecurring,
  nextSlot,
  parseAttendees,
  pasteDraft,
  rulesFor,
  timeField,
  timestampFrom,
  utcDateField,
  utcMidnightFrom,
  type EventForm,
} from "./calendar-edit";
import { DAY, MINUTE } from "./time";

const NINE = new Date(2026, 7, 7, 9, 0, 0, 0).getTime();

const event: CalendarEvent = {
  id: 1,
  calendarId: "primary",
  accountId: 1,
  title: "Standup",
  start: NINE,
  end: NINE + 30 * MINUTE,
  allDay: false,
  location: "Room 2",
  description: "The daily one",
  attendees: [{ name: "Ada", email: "ada@example.com" }],
};

describe("field conversion", () => {
  it("round-trips a local date and time", () => {
    expect(dateField(NINE)).toBe("2026-08-07");
    expect(timeField(NINE)).toBe("09:00");
    expect(timestampFrom("2026-08-07", "09:00")).toBe(NINE);
  });

  it("reads all-day dates in UTC, the way the store pins them", () => {
    const midnight = Date.UTC(2026, 7, 7);
    expect(utcDateField(midnight)).toBe("2026-08-07");
    expect(utcMidnightFrom("2026-08-07")).toBe(midnight);
  });

  it("refuses anything that is not a date or a time", () => {
    expect(timestampFrom("not-a-date", "09:00")).toBeNull();
    expect(timestampFrom("2026-08-07", "half nine")).toBeNull();
    expect(utcMidnightFrom("7 Aug")).toBeNull();
  });
});

describe("attendees", () => {
  it("accepts commas, semicolons and newlines", () => {
    expect(parseAttendees("a@x.com, b@x.com; c@x.com\nd@x.com").map((p) => p.email)).toEqual([
      "a@x.com",
      "b@x.com",
      "c@x.com",
      "d@x.com",
    ]);
  });

  it("understands the Name <addr> form people paste", () => {
    expect(parseAttendees("Ada Lovelace <ada@x.com>")).toEqual([
      { name: "Ada Lovelace", email: "ada@x.com" },
    ]);
  });

  it("drops anything that is not an address rather than sending it", () => {
    expect(parseAttendees("Ada, nonsense, b@x.com").map((p) => p.email)).toEqual(["b@x.com"]);
  });

  it("deduplicates case-insensitively", () => {
    expect(parseAttendees("A@x.com, a@x.com")).toHaveLength(1);
  });

  it("round-trips through the field", () => {
    const people = [{ name: "Ada", email: "ada@x.com" }, { name: "b@x.com", email: "b@x.com" }];
    expect(parseAttendees(attendeesField(people))).toEqual(people);
  });
});

describe("formTimes", () => {
  it("reads a timed event as local wall clock", () => {
    const times = formTimes({
      ...emptyForm({ calendarId: "primary" }),
      allDay: false,
      startDate: "2026-08-07",
      startTime: "09:00",
      endDate: "2026-08-07",
      endTime: "10:00",
    });
    expect(isFormError(times)).toBe(false);
    if (!isFormError(times)) {
      expect(times.start).toBe(NINE);
      expect(times.end - times.start).toBe(60 * MINUTE);
    }
  });

  it("turns an all-day range into an exclusive UTC end, the way Google wants", () => {
    const times = formTimes({
      ...emptyForm({ calendarId: "primary" }),
      allDay: true,
      startDate: "2026-08-07",
      endDate: "2026-08-08",
    });
    if (isFormError(times)) throw new Error(times.error);
    expect(times.start).toBe(Date.UTC(2026, 7, 7));
    // Two days shown, so the exclusive end is the 9th.
    expect(times.end).toBe(Date.UTC(2026, 7, 9));
  });

  it("refuses an end before the start", () => {
    const times = formTimes({
      ...emptyForm({ calendarId: "primary" }),
      startDate: "2026-08-07",
      startTime: "10:00",
      endDate: "2026-08-07",
      endTime: "09:00",
    });
    expect(isFormError(times)).toBe(true);
  });
});

describe("formFromEvent", () => {
  it("fills every field from the event", () => {
    const form = formFromEvent(event);
    expect(form.title).toBe("Standup");
    expect(form.startDate).toBe("2026-08-07");
    expect(form.startTime).toBe("09:00");
    expect(form.endTime).toBe("09:30");
    expect(form.location).toBe("Room 2");
    expect(form.attendees).toBe("Ada <ada@example.com>");
  });

  it("leaves the title blank rather than typing the placeholder into the field", () => {
    expect(formFromEvent({ ...event, title: "(no title)" }).title).toBe("");
  });

  it("shows an all-day event's last covered day, not its exclusive end", () => {
    const allDay: CalendarEvent = {
      ...event,
      allDay: true,
      start: Date.UTC(2026, 7, 7),
      end: Date.UTC(2026, 7, 9),
    };
    const form = formFromEvent(allDay);
    expect(form.startDate).toBe("2026-08-07");
    expect(form.endDate).toBe("2026-08-08");
  });

  it("survives a one-day all-day event without going backwards", () => {
    const oneDay: CalendarEvent = {
      ...event,
      allDay: true,
      start: Date.UTC(2026, 7, 7),
      end: Date.UTC(2026, 7, 8),
    };
    expect(formFromEvent(oneDay).endDate).toBe("2026-08-07");
  });
});

describe("formPatch — only what changed", () => {
  const base = formFromEvent(event);

  it("is undefined when nothing moved", () => {
    expect(formPatch(event, base)).toBeUndefined();
    expect(isDirty(event, base)).toBe(false);
  });

  it("names only the title when only the title changed", () => {
    expect(formPatch(event, { ...base, title: "Renamed" })).toEqual({ title: "Renamed" });
  });

  it("sends start, end and all-day together for any time change", () => {
    const patch = formPatch(event, { ...base, endTime: "10:00" });
    expect(patch).toEqual({
      startTs: NINE,
      endTs: NINE + 60 * MINUTE,
      isAllDay: false,
    });
  });

  it("sends the whole trio when converting to all-day", () => {
    const patch = formPatch(event, { ...base, allDay: true, startDate: "2026-08-07" });
    expect(patch?.isAllDay).toBe(true);
    expect(patch?.startTs).toBe(Date.UTC(2026, 7, 7));
    expect(patch?.endTs).toBe(Date.UTC(2026, 7, 8));
  });

  it("clears a text field with an empty string rather than omitting it", () => {
    expect(formPatch(event, { ...base, location: "" })).toEqual({ location: "" });
  });

  it("ignores a guest list that was only reordered", () => {
    expect(
      formPatch(event, { ...base, attendees: "ADA@example.com" }),
    ).toBeUndefined();
  });

  it("notices a guest being added", () => {
    const patch = formPatch(event, { ...base, attendees: "ada@example.com, bob@example.com" });
    expect(patch?.attendees?.map((p) => p.email)).toEqual(["ada@example.com", "bob@example.com"]);
  });

  it("carries a recurrence choice", () => {
    expect(formPatch(event, { ...base, recurrence: "weekly" })?.recurrence).toEqual([
      "RRULE:FREQ=WEEKLY;BYDAY=FR",
    ]);
  });

  it("carries a reminder choice, including none at all", () => {
    expect(formPatch(event, { ...base, reminderMinutes: 10 })?.reminderMinutes).toEqual([10]);
  });

  it("refuses to build a patch from a form that does not describe a time", () => {
    expect(formPatch(event, { ...base, startDate: "" })).toBeUndefined();
  });

  it("counts a calendar change as dirty even though it is a different command", () => {
    expect(isDirty(event, { ...base, calendarId: "work" })).toBe(true);
  });
});

describe("rulesFor", () => {
  it("anchors a weekly rule to the event's own weekday", () => {
    // 2026-08-07 is a Friday.
    expect(rulesFor("weekly", NINE)).toEqual(["RRULE:FREQ=WEEKLY;BYDAY=FR"]);
  });

  it("spells out the working week", () => {
    expect(rulesFor("weekdays", NINE)).toEqual(["RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"]);
  });

  it("says nothing at all for a one-off", () => {
    expect(rulesFor("none", NINE)).toEqual([]);
  });
});

describe("drafts", () => {
  it("builds a create draft out of a form", () => {
    const form: EventForm = {
      ...emptyForm({ calendarId: "primary", start: NINE }),
      title: "  New thing  ",
      attendees: "ada@x.com",
    };
    const draft = formDraft(form);
    if ("error" in draft) throw new Error(draft.error);
    expect(draft.title).toBe("New thing");
    expect(draft.startTs).toBe(NINE);
    expect(draft.attendees).toHaveLength(1);
    expect(draft.recurrence).toEqual([]);
  });

  it("reports the reason rather than a half-built draft", () => {
    const draft = formDraft({ ...emptyForm({ calendarId: "primary" }), startDate: "" });
    expect("error" in draft).toBe(true);
  });

  it("marks a duplicate as a copy and keeps everything else", () => {
    const draft = duplicateDraft(event);
    expect(draft.title).toBe("Standup (copy)");
    expect(draft.startTs).toBe(event.start);
    expect(draft.location).toBe("Room 2");
  });

  it("pastes onto another day at the same time of day", () => {
    const target = new Date(2026, 7, 12, 16, 30).getTime();
    const draft = pasteDraft(event, target);
    expect(new Date(draft.startTs).getDate()).toBe(12);
    expect(new Date(draft.startTs).getHours()).toBe(9);
    expect(draft.endTs - draft.startTs).toBe(event.end - event.start);
  });

  it("pastes an all-day event onto the target date", () => {
    const allDay: CalendarEvent = {
      ...event,
      allDay: true,
      start: Date.UTC(2026, 7, 7),
      end: Date.UTC(2026, 7, 8),
    };
    const draft = pasteDraft(allDay, new Date(2026, 7, 12, 16, 30).getTime());
    expect(draft.startTs).toBe(Date.UTC(2026, 7, 12));
    expect(draft.endTs).toBe(Date.UTC(2026, 7, 13));
  });
});

describe("nextSlot", () => {
  it("rounds up to the next clean half hour", () => {
    const at = new Date(2026, 7, 7, 9, 1).getTime();
    expect(nextSlot(at) - nextSlot(at) % DAY).toBeTypeOf("number");
    expect(new Date(nextSlot(at)).getMinutes() % 30).toBe(0);
    expect(nextSlot(at)).toBeGreaterThan(at);
  });
});

describe("looksRecurring", () => {
  const weekly: CalendarEvent[] = [0, 7, 14].map((offset, i) => ({
    ...event,
    id: 10 + i,
    start: NINE + offset * DAY,
    end: NINE + offset * DAY + 30 * MINUTE,
  }));

  it("spots the same meeting on another day", () => {
    expect(looksRecurring(weekly[1], weekly)).toBe(true);
  });

  it("does not call a one-off a series", () => {
    expect(looksRecurring(event, [event, { ...event, id: 2, title: "Something else" }])).toBe(
      false,
    );
  });

  it("ignores a same-named event of a different length", () => {
    const longer = { ...event, id: 3, start: NINE + DAY, end: NINE + DAY + 60 * MINUTE };
    expect(looksRecurring(event, [event, longer])).toBe(false);
  });

  it("ignores the same title on another calendar", () => {
    const elsewhere = { ...event, id: 4, calendarId: "work", start: NINE + DAY, end: NINE + DAY + 30 * MINUTE };
    expect(looksRecurring(event, [event, elsewhere])).toBe(false);
  });

  it("does not count two copies on the same day as a repeat", () => {
    const sameDay = { ...event, id: 5, start: NINE + 60 * MINUTE, end: NINE + 90 * MINUTE };
    expect(looksRecurring(event, [event, sameDay])).toBe(false);
  });

  it("says no for an untitled event rather than matching every other blank", () => {
    const blank = { ...event, id: 6, title: "" };
    const otherBlank = { ...blank, id: 7, start: NINE + DAY, end: NINE + DAY + 30 * MINUTE };
    expect(looksRecurring(blank, [blank, otherBlank])).toBe(false);
  });
});
