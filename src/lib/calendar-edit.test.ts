import { describe, expect, it } from "vitest";
import type { CalendarEvent } from "@/types";
import {
  attendeesField,
  canEditEvent,
  choiceForEvent,
  choiceFromRules,
  describeReminders,
  describeRules,
  reminderChoiceId,
  reminderMinutesOf,
  requiresSeriesScope,
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

  it("offers a working hour on an all-day event, not the UTC-midnight artifact", () => {
    // Reading a wall clock off a UTC-pinned all-day row gives 19:00 or 20:00
    // west of Greenwich. Unticking "All day" then produced a zero-length event
    // that evening, and saved it without complaint.
    const allDay: CalendarEvent = {
      ...event,
      allDay: true,
      start: Date.UTC(2026, 7, 7),
      end: Date.UTC(2026, 7, 8),
    };
    const form = formFromEvent(allDay);
    expect(form.startTime).toBe("09:00");
    expect(form.endTime).toBe("10:00");

    const timed = formTimes({ ...form, allDay: false });
    expect(isFormError(timed)).toBe(false);
    if (isFormError(timed)) return;
    expect(timed.end - timed.start).toBe(60 * MINUTE);
    expect(new Date(timed.start).getHours()).toBe(9);
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
    expect(formPatch(event, { ...base, reminderMinutes: [10] })?.reminderMinutes).toEqual([10]);
    // "No alert" is a real setting and a different one from "the calendar's
    // default", so the empty list has to survive as an empty list.
    expect(formPatch(event, { ...base, reminderMinutes: [] })?.reminderMinutes).toEqual([]);
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

  it("believes recurringEventId over any inference", () => {
    const occurrence = { ...event, recurringEventId: "series-abc" };
    // Alone in the window, with nothing to infer from.
    expect(looksRecurring(occurrence, [occurrence])).toBe(true);
  });

  it("treats an empty recurringEventId as a one-off, not as absent", () => {
    const oneOff = { ...event, recurringEventId: "" };
    const twin = { ...oneOff, id: 8, start: NINE + DAY, end: NINE + DAY + 30 * MINUTE };
    expect(looksRecurring(oneOff, [oneOff, twin])).toBe(false);
  });

  it("stops guessing once anything in the window reports the field", () => {
    // A backend that answers with `recurringEventId` answers with it on every
    // row, so a row without one is genuinely not part of a series — inferring
    // one from a repeated title there would only ever be a false positive.
    const reporting = { ...event, id: 9, recurringEventId: "series-xyz", start: NINE + DAY };
    expect(looksRecurring(weekly[1], [...weekly, reporting])).toBe(false);
  });
});

/* -------------------------------------------------------------------------- */
/* The fields that used to be write-only                                       */
/* -------------------------------------------------------------------------- */

describe("recurrence read-back", () => {
  it("recognises each rule the picker itself emits", () => {
    for (const choice of ["daily", "weekdays", "weekly", "monthly", "yearly"] as const) {
      expect(choiceFromRules(rulesFor(choice, NINE), NINE)).toBe(choice);
    }
    expect(choiceFromRules([], NINE)).toBe("none");
  });

  it("calls anything it did not author custom rather than guessing", () => {
    // The asymmetry that matters: reading this as "every week" would silently
    // triple the frequency of somebody's series on the next unrelated save.
    expect(choiceFromRules(["RRULE:FREQ=WEEKLY;INTERVAL=3"], NINE)).toBe("custom");
    expect(choiceFromRules(["RRULE:FREQ=WEEKLY", "EXDATE:20260814T090000Z"], NINE)).toBe("custom");
  });

  it("treats a bare weekly rule as weekly — same meaning, fewer words", () => {
    expect(choiceFromRules(["RRULE:FREQ=WEEKLY"], NINE)).toBe("weekly");
  });

  it("anchors the weekly match to the event's own weekday", () => {
    // 2026-08-07 is a Friday. `BYDAY=TU` on a Friday event is not "every week".
    expect(choiceFromRules(["RRULE:FREQ=WEEKLY;BYDAY=TU"], NINE)).toBe("custom");
  });

  it("describes a rule it cannot name in words a human reads", () => {
    expect(describeRules(["RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE"])).toBe(
      "Every 2 weeks on Monday, Wednesday",
    );
    expect(describeRules(["RRULE:FREQ=DAILY;UNTIL=20261231"])).toBe("Every day, until 2026-12-31");
    expect(describeRules([])).toBe("Does not repeat");
  });

  it("opens a recurring event on its own rule rather than on “does not repeat”", () => {
    const weekly: CalendarEvent = {
      ...event,
      recurrence: ["RRULE:FREQ=WEEKLY;BYDAY=FR"],
      recurringEventId: "series",
    };
    const form = formFromEvent(weekly);
    expect(form.recurrence).toBe("weekly");
    // And re-saving without touching it sends nothing about recurrence, which
    // is the bug the old "send it unless it is none" rule could not avoid.
    expect(formPatch(weekly, form)?.recurrence).toBeUndefined();
  });

  it("carries a rule it cannot express straight back out again", () => {
    const odd: CalendarEvent = { ...event, recurrence: ["RRULE:FREQ=WEEKLY;INTERVAL=3"] };
    const form = formFromEvent(odd);
    expect(form.recurrence).toBe("custom");
    expect(formPatch(odd, { ...form, title: "Renamed" })?.recurrence).toBeUndefined();
    // Picking a real choice is the only way to replace it.
    expect(formPatch(odd, { ...form, recurrence: "daily" })?.recurrence).toEqual([
      "RRULE:FREQ=DAILY",
    ]);
  });

  it("clears a rule when the user says it does not repeat", () => {
    const weekly: CalendarEvent = { ...event, recurrence: ["RRULE:FREQ=WEEKLY;BYDAY=FR"] };
    const form = { ...formFromEvent(weekly), recurrence: "none" as const };
    expect(formPatch(weekly, form)?.recurrence).toEqual([]);
  });

  it("never says “does not repeat” about an occurrence Google expanded", () => {
    // The lie this fixes. `singleEvents=true` returns occurrences, an occurrence
    // carries no RRULE, so a synced weekly standup arrives with a series id and
    // an empty rule list — and an empty rule list was being read as "no rule",
    // which the picker rendered as "Does not repeat" over a meeting Google's own
    // popover described as "Weekly on weekdays".
    const occurrence: CalendarEvent = { ...event, recurringEventId: "series-abc" };
    const form = formFromEvent(occurrence);
    expect(form.recurrence).toBe("series");
    expect(choiceForEvent(occurrence)).toBe("series");
    // And it stays silent on save: nothing is known about the rule, so nothing
    // is claimed about it.
    expect(formPatch(occurrence, form)).toBeUndefined();
    expect(formPatch(occurrence, { ...form, title: "Renamed" })?.recurrence).toBeUndefined();
  });

  it("still says “does not repeat” about an event that genuinely does not", () => {
    expect(choiceForEvent(event)).toBe("none");
    expect(choiceForEvent({ ...event, recurringEventId: "" })).toBe("none");
  });

  it("prefers a rule it does have over the vaguer answer", () => {
    // A series Mach created, or one a sibling occurrence taught the store: the
    // rule is known, so it is named rather than described as "it repeats".
    const known: CalendarEvent = {
      ...event,
      recurringEventId: "series-abc",
      recurrence: ["RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"],
    };
    expect(choiceForEvent(known)).toBe("weekdays");
  });

  it("replaces an unknown rule only when the user picks one", () => {
    const occurrence: CalendarEvent = { ...event, recurringEventId: "series-abc" };
    const form = formFromEvent(occurrence);
    expect(formPatch(occurrence, { ...form, recurrence: "daily" })?.recurrence).toEqual([
      "RRULE:FREQ=DAILY",
    ]);
  });

  it("says when a save has to take the whole series with it", () => {
    expect(requiresSeriesScope({ recurrence: ["RRULE:FREQ=DAILY"] })).toBe(true);
    expect(requiresSeriesScope({ title: "Renamed" })).toBe(false);
  });
});

describe("reminder read-back", () => {
  const withReminder = (minutes: number[]): CalendarEvent => ({
    ...event,
    reminders: { useDefault: false, overrides: minutes.map((m) => ({ method: "popup", minutes: m })) },
  });

  it("keeps the calendar default and “no alert” as different things", () => {
    expect(reminderMinutesOf({ ...event, reminders: { useDefault: true, overrides: [] } })).toBeNull();
    expect(reminderMinutesOf(withReminder([]))).toEqual([]);
    expect(reminderChoiceId(null)).toBe("default");
    expect(reminderChoiceId([])).toBe("none");
  });

  it("reads an event's own alert into the picker", () => {
    expect(formFromEvent(withReminder([30])).reminderMinutes).toEqual([30]);
    expect(reminderChoiceId([30])).toBe("30");
  });

  it("labels an alert the picker has no row for", () => {
    expect(reminderChoiceId([45])).toBe("custom");
    expect(describeReminders([45])).toBe("45 minutes before");
    expect(describeReminders([20, 1440])).toBe("20 minutes before and 1 day before");
    expect(describeReminders([0])).toBe("At start");
    expect(describeReminders(null)).toBe("Calendar default");
  });

  it("sends nothing when the alert was not touched", () => {
    const source = withReminder([10]);
    expect(formPatch(source, formFromEvent(source))?.reminderMinutes).toBeUndefined();
  });

  it("cannot express a return to the calendar default, and does not pretend to", () => {
    // `EventPatch.reminderMinutes` has no "use the default" value, so choosing
    // it sends nothing rather than sending "no alert" and calling it default.
    const source = withReminder([10]);
    const form = { ...formFromEvent(source), reminderMinutes: null };
    expect(formPatch(source, form)?.reminderMinutes).toBeUndefined();
  });
});

describe("canEditEvent", () => {
  const mine = ["alex@example.com"];

  it("takes nothing away when the seam has not said who owns it", () => {
    // Fixture data, and every row written before the store had the column. A
    // guess here turns the editor off for every event on first launch.
    expect(canEditEvent(event, mine)).toBe(true);
    expect(canEditEvent({ ...event, organizer: { name: "X", email: "x@y.com" } }, mine)).toBe(true);
  });

  it("allows an event this account organizes", () => {
    expect(canEditEvent({ ...event, organizerSelf: true }, mine)).toBe(true);
  });

  it("refuses an invitation from someone else", () => {
    expect(
      canEditEvent(
        { ...event, organizerSelf: false, organizer: { name: "Chief", email: "chief@elsewhere.com" } },
        mine,
      ),
    ).toBe(false);
  });

  it("allows it anyway when the organizer let guests modify", () => {
    expect(
      canEditEvent(
        {
          ...event,
          organizerSelf: false,
          guestsCanModify: true,
          organizer: { name: "Chief", email: "chief@elsewhere.com" },
        },
        mine,
      ),
    ).toBe(true);
  });

  it("recognises the owner's other addresses, whatever this copy says", () => {
    expect(
      canEditEvent(
        { ...event, organizerSelf: false, organizer: { name: "Me", email: "ALEX@example.com" } },
        mine,
      ),
    ).toBe(true);
  });

  it("refuses a calendar this account may only read, whoever organized it", () => {
    // A subscribed holiday or team calendar. Every event on it is organized
    // elsewhere, `guestsCanModify` means nothing, and Google answers 403 — so
    // the editor should never have been offered.
    const ours = { ...event, organizerSelf: true };
    expect(canEditEvent(ours, mine, "reader")).toBe(false);
    expect(canEditEvent(ours, mine, "freeBusyReader")).toBe(false);
  });

  it("does not let a writable calendar hand back an event that is not ours", () => {
    // The calendar can only ever veto. `writer` says "you may write here", not
    // "you may rewrite a stranger's invitation", and conflating the two would
    // put Save back on every event in a shared calendar.
    const theirs = {
      ...event,
      organizerSelf: false,
      organizer: { name: "Chief", email: "chief@elsewhere.com" },
    };
    expect(canEditEvent(theirs, mine, "writer")).toBe(false);
    expect(canEditEvent(theirs, mine, "owner")).toBe(false);
  });

  it("treats an unfetched access role as silence, not as a refusal", () => {
    // Every calendar looks like this until its first metadata sweep lands.
    expect(canEditEvent({ ...event, organizerSelf: true }, mine, undefined)).toBe(true);
    expect(canEditEvent({ ...event, organizerSelf: true }, mine)).toBe(true);
  });
});
