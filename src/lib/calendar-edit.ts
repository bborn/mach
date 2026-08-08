/**
 * The event editor's state machine, with no React in it.
 *
 * The modal is a form over a `CalendarEvent`. Three things have to be true and
 * none of them are obvious from the component:
 *
 *  1. **The form is strings, the command is numbers.** Date and time fields hold
 *     `yyyy-mm-dd` and `HH:mm` because that is what `<input type="date">` and
 *     `type="time"` speak. Turning those into epoch millis — in the *local*
 *     zone, and at UTC midnight for all-day, which is how the store pins them —
 *     is a conversion with edge cases, so it lives here and is tested here.
 *  2. **Only what changed is sent.** `events.patch` is a genuine partial update.
 *     Sending the whole event on every save would clobber anything Google
 *     changed underneath us (an attendee's RSVP, most obviously) and would make
 *     every undo a full-event rewrite instead of a one-field flip.
 *  3. **A time change is atomic.** Start, end and all-day always travel
 *     together, because switching between timed and all-day changes the shape
 *     of both ends and Google rejects a half-converted pair.
 */

import type { CalendarEvent, CalendarId, Participant } from "@/types";
import type { EventDraft, EventPatch } from "./data";
import { DEFAULT_EVENT_MINUTES } from "./calendar-geometry";
import { DAY, MINUTE, startOfDay } from "./time";

/** The recurrence rules the modal offers. Anything else is shown, not edited. */
export type RecurrenceChoice = "none" | "daily" | "weekdays" | "weekly" | "monthly" | "yearly";

export const RECURRENCE_CHOICES: { id: RecurrenceChoice; label: string }[] = [
  { id: "none", label: "Does not repeat" },
  { id: "daily", label: "Every day" },
  { id: "weekdays", label: "Every weekday" },
  { id: "weekly", label: "Every week" },
  { id: "monthly", label: "Every month" },
  { id: "yearly", label: "Every year" },
];

/** Reminder offsets, in minutes before the start. */
export const REMINDER_CHOICES: { minutes: number | null; label: string }[] = [
  { minutes: null, label: "Calendar default" },
  { minutes: 0, label: "At start" },
  { minutes: 5, label: "5 minutes before" },
  { minutes: 10, label: "10 minutes before" },
  { minutes: 30, label: "30 minutes before" },
  { minutes: 60, label: "1 hour before" },
  { minutes: 1440, label: "1 day before" },
];

export interface EventForm {
  title: string;
  allDay: boolean;
  /** `yyyy-mm-dd`. */
  startDate: string;
  /** `HH:mm`, ignored when `allDay`. */
  startTime: string;
  endDate: string;
  endTime: string;
  location: string;
  description: string;
  /** One address per line or comma-separated; parsed on save. */
  attendees: string;
  calendarId: CalendarId;
  recurrence: RecurrenceChoice;
  /** `null` means "leave the calendar's default reminder alone". */
  reminderMinutes: number | null;
}

/* -------------------------------------------------------------------------- */
/* String ⇄ timestamp                                                          */
/* -------------------------------------------------------------------------- */

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

/** `yyyy-mm-dd` in the local zone. */
export function dateField(ts: number): string {
  const d = new Date(ts);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** `yyyy-mm-dd` read in UTC — how all-day rows are pinned in the store. */
export function utcDateField(ts: number): string {
  const d = new Date(ts);
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`;
}

export function timeField(ts: number): string {
  const d = new Date(ts);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** Local midnight-relative timestamp for a `yyyy-mm-dd` + `HH:mm` pair. */
export function timestampFrom(date: string, time: string): number | null {
  const day = date.match(/^(\d{4})-(\d{2})-(\d{2})$/);
  const clock = time.match(/^(\d{1,2}):(\d{2})$/);
  if (!day || !clock) return null;
  const [, y, m, d] = day;
  const [, hh, mm] = clock;
  const value = new Date(Number(y), Number(m) - 1, Number(d), Number(hh), Number(mm), 0, 0);
  return Number.isNaN(value.getTime()) ? null : value.getTime();
}

/** UTC midnight for a `yyyy-mm-dd` — the all-day convention the store uses. */
export function utcMidnightFrom(date: string): number | null {
  const day = date.match(/^(\d{4})-(\d{2})-(\d{2})$/);
  if (!day) return null;
  const [, y, m, d] = day;
  return Date.UTC(Number(y), Number(m) - 1, Number(d));
}

/* -------------------------------------------------------------------------- */
/* Attendees                                                                   */
/* -------------------------------------------------------------------------- */

/**
 * Addresses out of whatever was typed.
 *
 * Accepts commas, semicolons and newlines, and `Name <addr>`, because people
 * paste guest lists out of other calendars and out of mail. Anything with no
 * `@` in it is dropped rather than sent — Google would refuse it anyway, and
 * refusing here means the error names the line rather than the request.
 */
export function parseAttendees(input: string): Participant[] {
  const out: Participant[] = [];
  const seen = new Set<string>();
  for (const chunk of input.split(/[,;\n]+/)) {
    const raw = chunk.trim();
    if (!raw) continue;
    const angled = raw.match(/^(.*)<([^>]+)>$/);
    const email = (angled ? angled[2] : raw).trim();
    const name = angled ? angled[1].trim().replace(/^["']|["']$/g, "") : "";
    if (!email.includes("@")) continue;
    const key = email.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ name: name || email, email });
  }
  return out;
}

export function attendeesField(people: readonly Participant[]): string {
  return people
    .map((p) => (p.name && p.name !== p.email ? `${p.name} <${p.email}>` : p.email))
    .join(", ");
}

function sameAttendees(a: readonly Participant[], b: readonly Participant[]): boolean {
  if (a.length !== b.length) return false;
  const key = (p: Participant) => p.email.toLowerCase();
  const left = [...a].map(key).sort();
  const right = [...b].map(key).sort();
  return left.every((value, i) => value === right[i]);
}

/* -------------------------------------------------------------------------- */
/* Recurrence                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * RRULE lines for a choice.
 *
 * Weekly repeats are anchored to the event's own weekday, which is what every
 * calendar means by "every week" and what Google would otherwise infer from the
 * start anyway — saying it explicitly keeps the rule readable in Google's UI.
 */
export function rulesFor(choice: RecurrenceChoice, start: number): string[] {
  switch (choice) {
    case "none":
      return [];
    case "daily":
      return ["RRULE:FREQ=DAILY"];
    case "weekdays":
      return ["RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"];
    case "weekly":
      return [`RRULE:FREQ=WEEKLY;BYDAY=${BYDAY[new Date(start).getDay()]}`];
    case "monthly":
      return ["RRULE:FREQ=MONTHLY"];
    case "yearly":
      return ["RRULE:FREQ=YEARLY"];
  }
}

const BYDAY = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];

/* -------------------------------------------------------------------------- */
/* Form ⇄ event                                                                */
/* -------------------------------------------------------------------------- */

export function formFromEvent(event: CalendarEvent): EventForm {
  return {
    title: event.title === "(no title)" ? "" : event.title,
    allDay: event.allDay,
    startDate: event.allDay ? utcDateField(event.start) : dateField(event.start),
    startTime: timeField(event.start),
    // Google's all-day end is exclusive; the field shows the last day the event
    // actually covers, which is the one a human would name.
    endDate: event.allDay
      ? utcDateField(Math.max(event.start, event.end - DAY))
      : dateField(event.end),
    endTime: timeField(event.end),
    location: event.location ?? "",
    description: event.description ?? "",
    attendees: attendeesField(event.attendees),
    calendarId: event.calendarId,
    // Recurrence is not stored locally, so an existing event's rule cannot be
    // read back. The modal says as much rather than showing "does not repeat"
    // over a weekly meeting — see `isRecurring` in the component.
    recurrence: "none",
    reminderMinutes: null,
  };
}

/** A blank form for a new event in a given slot. */
export function emptyForm(options: {
  start?: number;
  end?: number;
  calendarId: CalendarId;
  allDay?: boolean;
}): EventForm {
  const start = options.start ?? nextSlot(Date.now());
  const end = options.end ?? start + DEFAULT_EVENT_MINUTES * MINUTE;
  const allDay = options.allDay ?? false;
  return {
    title: "",
    allDay,
    startDate: allDay ? utcDateField(start) : dateField(start),
    startTime: timeField(start),
    endDate: allDay ? utcDateField(start) : dateField(end),
    endTime: timeField(end),
    location: "",
    description: "",
    attendees: "",
    calendarId: options.calendarId,
    recurrence: "none",
    reminderMinutes: null,
  };
}

/** The next clean half hour — where a keyboard-created event lands. */
export function nextSlot(now: number): number {
  const step = DEFAULT_EVENT_MINUTES * MINUTE;
  return Math.ceil(now / step) * step;
}

export interface FormTimes {
  start: number;
  end: number;
  allDay: boolean;
}

/**
 * The instants a form describes, or the reason it describes none.
 *
 * All-day events are UTC-midnight to exclusive-UTC-midnight, matching what the
 * sync layer writes for a `date`-only start; timed events are local wall clock.
 */
export function formTimes(form: EventForm): FormTimes | { error: string } {
  if (form.allDay) {
    const start = utcMidnightFrom(form.startDate);
    const last = utcMidnightFrom(form.endDate || form.startDate);
    if (start === null || last === null) return { error: "That date is not a date" };
    if (last < start) return { error: "It cannot end before it starts" };
    return { start, end: last + DAY, allDay: true };
  }
  const start = timestampFrom(form.startDate, form.startTime);
  const end = timestampFrom(form.endDate || form.startDate, form.endTime);
  if (start === null || end === null) return { error: "That date or time is not valid" };
  if (end < start) return { error: "It cannot end before it starts" };
  return { start, end, allDay: false };
}

export function isFormError(value: FormTimes | { error: string }): value is { error: string } {
  return "error" in value;
}

/**
 * What actually changed, as a patch.
 *
 * `undefined` means "nothing to save" — the caller uses that to skip the round
 * trip entirely rather than sending an empty body and calling it a success.
 * `calendarId` is deliberately *not* in here: moving between calendars is a
 * different command with a different inverse, and the caller dispatches it
 * separately.
 */
export function formPatch(event: CalendarEvent, form: EventForm): EventPatch | undefined {
  const times = formTimes(form);
  if (isFormError(times)) return undefined;

  const patch: EventPatch = {};
  const title = form.title.trim();
  if (title !== (event.title === "(no title)" ? "" : event.title)) patch.title = title;
  if (form.location.trim() !== (event.location ?? "")) patch.location = form.location.trim();
  if (form.description !== (event.description ?? "")) patch.description = form.description;

  const attendees = parseAttendees(form.attendees);
  if (!sameAttendees(attendees, event.attendees)) patch.attendees = attendees;

  // Start, end and all-day move as one — a half-converted pair is a body
  // Google refuses.
  if (
    times.start !== event.start ||
    times.end !== event.end ||
    times.allDay !== event.allDay
  ) {
    patch.startTs = times.start;
    patch.endTs = times.end;
    patch.isAllDay = times.allDay;
  }

  if (form.recurrence !== "none") patch.recurrence = rulesFor(form.recurrence, times.start);
  if (form.reminderMinutes !== null) patch.reminderMinutes = [form.reminderMinutes];

  return Object.keys(patch).length > 0 ? patch : undefined;
}

/** The draft a form describes, for a create. */
export function formDraft(form: EventForm): EventDraft | { error: string } {
  const times = formTimes(form);
  if (isFormError(times)) return times;
  return {
    title: form.title.trim(),
    description: form.description || undefined,
    location: form.location.trim() || undefined,
    startTs: times.start,
    endTs: times.end,
    isAllDay: times.allDay,
    attendees: parseAttendees(form.attendees),
    recurrence: rulesFor(form.recurrence, times.start),
    reminderMinutes: form.reminderMinutes === null ? undefined : [form.reminderMinutes],
  };
}

/** The draft that duplicates an event — same everything, new row. */
export function duplicateDraft(event: CalendarEvent): EventDraft {
  return {
    title: event.title === "(no title)" ? "" : `${event.title} (copy)`,
    description: event.description,
    location: event.location,
    startTs: event.start,
    endTs: event.end,
    isAllDay: event.allDay,
    attendees: event.attendees,
    recurrence: [],
  };
}

/**
 * The draft that pastes a copied event into a day.
 *
 * The time of day survives, the date does not — pasting is "this meeting, but
 * on that day", which is what every calendar means by it.
 */
export function pasteDraft(event: CalendarEvent, targetDay: number): EventDraft {
  const duration = event.end - event.start;
  const start = event.allDay
    ? Date.UTC(
        new Date(targetDay).getFullYear(),
        new Date(targetDay).getMonth(),
        new Date(targetDay).getDate(),
      )
    : startOfDay(targetDay).getTime() + (event.start - startOfDay(event.start).getTime());
  return {
    title: event.title === "(no title)" ? "" : event.title,
    description: event.description,
    location: event.location,
    startTs: start,
    endTs: start + duration,
    isAllDay: event.allDay,
    attendees: event.attendees,
    recurrence: [],
  };
}

/* -------------------------------------------------------------------------- */
/* Is this one of a series?                                                    */
/* -------------------------------------------------------------------------- */

/**
 * Whether an event looks like an occurrence of a repeating series.
 *
 * The honest version of this would read `recurringEventId`, which the store
 * has and `list_events` returns — but `mapEvent` in `src/lib/ipc.ts` (another
 * unit's file) does not carry it onto `CalendarEvent`. So it is inferred:
 * another event, on the same calendar, with the same title and the same
 * duration, on a different day.
 *
 * **Biased towards saying no**, deliberately. A false negative means the user
 * is not asked "this one or all of them?" and the edit applies to this
 * occurrence — which is the safe answer, and the one Google applies when you
 * press Delete without choosing. A false positive would ask a pointless
 * question about a one-off. Only the first costs correctness, and it costs
 * nothing.
 */
export function looksRecurring(
  event: CalendarEvent,
  others: readonly CalendarEvent[],
): boolean {
  const duration = event.end - event.start;
  const day = startOfDay(event.start).getTime();
  const title = event.title.trim().toLowerCase();
  if (!title) return false;
  return others.some(
    (other) =>
      other.id !== event.id &&
      other.calendarId === event.calendarId &&
      other.accountId === event.accountId &&
      other.end - other.start === duration &&
      other.title.trim().toLowerCase() === title &&
      startOfDay(other.start).getTime() !== day,
  );
}

/** Has the form drifted from the event it was opened on? */
export function isDirty(event: CalendarEvent, form: EventForm): boolean {
  return formPatch(event, form) !== undefined || form.calendarId !== event.calendarId;
}
