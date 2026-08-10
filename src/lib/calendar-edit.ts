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

import type {
  CalendarAccessRole,
  CalendarEvent,
  CalendarId,
  Participant,
} from "@/types";
import type { EventDraft, EventPatch } from "./data";
import { DEFAULT_EVENT_MINUTES } from "./calendar-geometry";
import { DAY, MINUTE, startOfDay } from "./time";

/**
 * The recurrence rules the modal offers, plus `custom`.
 *
 * `custom` is never in the picker — it is what an event whose rule Mach did not
 * write turns into. Somebody's `FREQ=WEEKLY;INTERVAL=3;UNTIL=…` is a rule this
 * form cannot express, and the two wrong answers are both worse than admitting
 * it: showing "does not repeat" makes the next save silently delete the series,
 * and showing "every week" makes it silently retime one. So the rule is carried
 * through untouched, described in words, and only replaced if the user picks
 * something else.
 *
 * `series` is the same admission one step further along, and it is the state
 * this form was lying about. Google is asked for events with `singleEvents=true`
 * and answers with *occurrences*; the RRULE lives on the series master, which
 * that expansion never returns. So a weekly standup synced from Google arrives
 * with `recurringEventId` set and `recurrence` empty — the store knows the event
 * repeats and does not know how — and an empty rule list was being read as "no
 * rule", which the picker rendered as **Does not repeat** over a meeting that
 * Google's own popover described as "Weekly on weekdays".
 *
 * Fetching the master would answer it exactly, at one `events.get` per series
 * against a UI that is not allowed to wait on the network, on a path this unit
 * does not own. The rule is also worth almost nothing here: it cannot be edited
 * from this form without addressing the whole series anyway. So the honest,
 * free answer is to stop claiming the opposite of the truth — `series` says
 * "this repeats, the rule is Google's" and, exactly like `custom`, carries
 * nothing into a save.
 */
export type RecurrenceChoice =
  | "none"
  | "daily"
  | "weekdays"
  | "weekly"
  | "monthly"
  | "yearly"
  | "custom"
  | "series";

export const RECURRENCE_CHOICES: { id: RecurrenceChoice; label: string }[] = [
  { id: "none", label: "Does not repeat" },
  { id: "daily", label: "Every day" },
  { id: "weekdays", label: "Every weekday" },
  { id: "weekly", label: "Every week" },
  { id: "monthly", label: "Every month" },
  { id: "yearly", label: "Every year" },
];

/**
 * What the alert picker offers.
 *
 * `minutes` is a list rather than a number because Google's three states need
 * three shapes, and collapsing them loses one: `null` follows the calendar's
 * default, `[]` means no alert at all, and anything else is those offsets. An
 * event that quietly stops alerting is a bug you find by missing a meeting, so
 * "default" and "none" are offered as separate rows rather than merged.
 *
 * `id` exists because a `<select>` holds a string, not a list.
 */
export interface ReminderChoice {
  id: string;
  label: string;
  minutes: number[] | null;
}

export const REMINDER_CHOICES: ReminderChoice[] = [
  { id: "default", label: "Calendar default", minutes: null },
  { id: "none", label: "No alert", minutes: [] },
  { id: "0", label: "At start", minutes: [0] },
  { id: "5", label: "5 minutes before", minutes: [5] },
  { id: "10", label: "10 minutes before", minutes: [10] },
  { id: "30", label: "30 minutes before", minutes: [30] },
  { id: "60", label: "1 hour before", minutes: [60] },
  { id: "1440", label: "1 day before", minutes: [1440] },
];

/** The picker row a form's current setting corresponds to. */
export function reminderChoiceId(minutes: number[] | null): string {
  if (minutes === null) return "default";
  if (minutes.length === 0) return "none";
  return REMINDER_CHOICES.find((c) => sameNumbers(c.minutes, minutes))?.id ?? "custom";
}

export function reminderChoiceById(id: string): ReminderChoice | undefined {
  return REMINDER_CHOICES.find((choice) => choice.id === id);
}

/** "45 minutes before", or "20 minutes and 1 day before" for a set of them. */
export function describeReminders(minutes: number[] | null): string {
  if (minutes === null) return "Calendar default";
  if (minutes.length === 0) return "No alert";
  // "at start" is already a whole phrase; the others are quantities that need
  // the word "before" after them, and gluing it onto the first would read
  // "at start before".
  const parts = minutes.map((m) => (m === 0 ? "at start" : `${describeOffset(m)} before`));
  const last = parts.pop()!;
  const sentence = parts.length === 0 ? last : `${parts.join(", ")} and ${last}`;
  return sentence.charAt(0).toUpperCase() + sentence.slice(1);
}

function describeOffset(minutes: number): string {
  if (minutes % 1440 === 0) return plural(minutes / 1440, "day");
  if (minutes % 60 === 0) return plural(minutes / 60, "hour");
  return plural(minutes, "minute");
}

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function sameNumbers(a: number[] | null, b: number[] | null): boolean {
  if (a === null || b === null) return a === b;
  return a.length === b.length && a.every((value, i) => value === b[i]);
}

/**
 * The offsets an event is currently set to alert at, in the form's vocabulary.
 *
 * `null` for "the calendar's default", which is also what an event the store
 * has never been told about reads as — the two are indistinguishable from here,
 * and both mean "leave it alone", so nothing is lost by conflating them.
 */
export function reminderMinutesOf(event: CalendarEvent): number[] | null {
  const reminders = event.reminders;
  if (!reminders || reminders.useDefault) return null;
  return reminders.overrides.map((r) => r.minutes);
}

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
  /**
   * The rule the form stands for while `recurrence` is `custom`.
   *
   * Ignored for every other choice, which derives its lines from the start
   * date. It exists so that opening a series Mach did not author, changing its
   * title and saving does not rewrite how it repeats.
   */
  recurrenceRules: string[];
  /** `null` = the calendar's default; `[]` = no alert; otherwise the offsets. */
  reminderMinutes: number[] | null;
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
    // A rule this form did not author has no derivation; `rulesOf` carries the
    // original lines through instead, and that is the only correct answer here.
    case "custom":
      return [];
    // And a rule this form has never *seen* has nothing to carry through. An
    // empty list is right for the same reason it is wrong for `none`: it is
    // compared against what the event already has, which is also empty, so the
    // save says nothing about how the event repeats.
    case "series":
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

/**
 * The lines a form actually means — the derived ones, or the carried-through
 * custom rule.
 */
export function rulesOf(form: EventForm, start: number): string[] {
  return form.recurrence === "custom" || form.recurrence === "series"
    ? form.recurrenceRules
    : rulesFor(form.recurrence, start);
}

/**
 * Which picker row an event stands on, rule or no rule.
 *
 * The distinction `choiceFromRules` cannot make on its own: an empty rule list
 * means "no rule is known here", and whether that is "does not repeat" or "we
 * were not told" is answered by `recurringEventId`, which Google sets on every
 * occurrence it expands out of a series.
 */
export function choiceForEvent(event: CalendarEvent): RecurrenceChoice {
  const rules = event.recurrence ?? [];
  if (rules.length === 0 && (event.recurringEventId ?? "").length > 0) return "series";
  return choiceFromRules(rules, event.start);
}

/**
 * Which picker row a stored rule corresponds to, if any.
 *
 * Deliberately strict: only the exact lines `rulesFor` emits map back to a
 * choice. A rule that differs by so much as an `INTERVAL` is `custom`, because
 * the cost of being wrong is asymmetric — treating an unknown rule as custom
 * costs a slightly vague label, and treating it as "every week" silently
 * rewrites somebody's series on the next unrelated save.
 *
 * The weekday check is anchored to the event's own start for the same reason
 * `rulesFor` anchors it: `FREQ=WEEKLY;BYDAY=TU` is "every week" for a Tuesday
 * meeting and something stranger for a Thursday one.
 */
export function choiceFromRules(rules: readonly string[], start: number): RecurrenceChoice {
  if (rules.length === 0) return "none";
  if (rules.length > 1) return "custom";
  const rule = rules[0].trim().toUpperCase();
  for (const choice of RECURRENCE_CHOICES) {
    if (choice.id === "none") continue;
    const [derived] = rulesFor(choice.id, start);
    if (derived && derived.toUpperCase() === rule) return choice.id;
  }
  // `FREQ=WEEKLY` with no BYDAY is what Google stores when the rule was made
  // somewhere that leaves the weekday implicit. Same meaning, different words.
  if (rule === "RRULE:FREQ=WEEKLY") return "weekly";
  return "custom";
}

/** A human sentence for a rule, for the cases the picker cannot name. */
export function describeRules(rules: readonly string[]): string {
  if (rules.length === 0) return "Does not repeat";
  const rule = rules[0].replace(/^RRULE:/i, "");
  const freq = /FREQ=([A-Z]+)/i.exec(rule)?.[1]?.toUpperCase();
  const interval = Number(/INTERVAL=(\d+)/i.exec(rule)?.[1] ?? 1);
  const unit = { DAILY: "day", WEEKLY: "week", MONTHLY: "month", YEARLY: "year" }[freq ?? ""];
  if (!unit) return `Custom rule (${rule})`;
  const every = interval === 1 ? `Every ${unit}` : `Every ${interval} ${unit}s`;
  const days = /BYDAY=([A-Z,]+)/i.exec(rule)?.[1];
  const until = /UNTIL=(\d{8})/i.exec(rule)?.[1];
  const count = /COUNT=(\d+)/i.exec(rule)?.[1];
  return [
    days ? `${every} on ${days.split(",").map(dayName).join(", ")}` : every,
    until ? `until ${until.slice(0, 4)}-${until.slice(4, 6)}-${until.slice(6, 8)}` : null,
    count ? `${count} times` : null,
    rules.length > 1 ? "with exceptions" : null,
  ]
    .filter(Boolean)
    .join(", ");
}

const DAY_NAMES: Record<string, string> = {
  SU: "Sunday",
  MO: "Monday",
  TU: "Tuesday",
  WE: "Wednesday",
  TH: "Thursday",
  FR: "Friday",
  SA: "Saturday",
};

function dayName(code: string): string {
  return DAY_NAMES[code.trim().toUpperCase()] ?? code;
}

function sameRules(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((line, i) => line === b[i]);
}

/* -------------------------------------------------------------------------- */
/* May this even be edited?                                                    */
/* -------------------------------------------------------------------------- */

/**
 * Whether Google would accept a write to this event from this app.
 *
 * Google refuses an `events.patch` from anyone who is not the organizer unless
 * the organizer ticked "guests can modify". Offering Save on an invitation you
 * merely received is therefore offering a button whose only possible outcome is
 * an error, which is a worse experience than not offering it — the user learns
 * the rule from a red status line instead of from the interface.
 *
 * **Silence is permission.** `organizerSelf === undefined` means the seam never
 * said: fixture data, a row written before the store had the column, an event
 * created in this session that has not synced back. Reading that as "not yours"
 * would take the editor away from every event on first launch after the upgrade
 * and hand it back a minute later, which looks like a bug. So the affordance
 * only goes away on a positive `false`.
 *
 * `accountEmails` is the last resort for the same reason: an event whose
 * organizer is one of the owner's own addresses is his, whatever `self` says
 * about the particular copy in front of us.
 *
 * **The calendar can veto, and only the calendar can veto.** `accessRole` is
 * Google's answer to a different question — not "is this event yours" but "may
 * you write to this calendar at all" — and a `reader` subscription refuses every
 * write regardless of who organized what. A subscribed holiday calendar is the
 * clearest case: every event on it is organized by Google, `guestsCanModify` is
 * meaningless, and offering an editor produces a 403 a round trip later.
 *
 * It is checked first and it can only ever say *no*, which is the shape that
 * keeps this one decision rather than two. `owner` and `writer` do not grant an
 * edit on a stranger's invitation; they simply decline to remove one. And the
 * same silence rule applies once more: `undefined` is a calendar whose metadata
 * has never been fetched — every calendar, on the first launch after migration 6
 * — so it means "not told", never "denied".
 */
export function canEditEvent(
  event: CalendarEvent,
  accountEmails: readonly string[] = [],
  accessRole?: CalendarAccessRole,
): boolean {
  if (accessRole === "reader" || accessRole === "freeBusyReader") return false;
  if (event.organizerSelf === undefined) return true;
  if (event.organizerSelf) return true;
  if (event.guestsCanModify) return true;
  const organizer = event.organizer?.email?.toLowerCase();
  if (!organizer) return true;
  return accountEmails.some((email) => email.toLowerCase() === organizer);
}

/* -------------------------------------------------------------------------- */
/* Form ⇄ event                                                                */
/* -------------------------------------------------------------------------- */

/**
 * The clock an all-day event's time fields show.
 *
 * An all-day row is pinned to *UTC* midnight, so reading a wall clock off it
 * gives 19:00 or 20:00 anywhere west of Greenwich — the previous evening, in
 * local time, of a day the event does not even cover. Those fields are hidden
 * while "All day" is ticked, so nobody sees the nonsense; the moment the tick
 * comes off they become the event's real time, and unticking used to produce a
 * zero-length meeting at seven in the evening that saved without complaint.
 *
 * A working hour is what every calendar offers instead, and it is a guess the
 * user can see and change before saving.
 */
const ALL_DAY_START = "09:00";
const ALL_DAY_END = "10:00";

export function formFromEvent(event: CalendarEvent): EventForm {
  const rules = event.recurrence ?? [];
  return {
    title: event.title === "(no title)" ? "" : event.title,
    allDay: event.allDay,
    startDate: event.allDay ? utcDateField(event.start) : dateField(event.start),
    startTime: event.allDay ? ALL_DAY_START : timeField(event.start),
    // Google's all-day end is exclusive; the field shows the last day the event
    // actually covers, which is the one a human would name.
    endDate: event.allDay
      ? utcDateField(Math.max(event.start, event.end - DAY))
      : dateField(event.end),
    endTime: event.allDay ? ALL_DAY_END : timeField(event.end),
    location: event.location ?? "",
    description: event.description ?? "",
    attendees: attendeesField(event.attendees),
    calendarId: event.calendarId,
    // The rule and the alerts are now read back rather than assumed. They were
    // write-only for a long time, and the modal papered over it by showing
    // "does not repeat" and "calendar default" on top of a weekly meeting with
    // a fifteen-minute alert — so an unrelated title edit re-sent both defaults
    // and quietly rewrote the event.
    //
    // `choiceForEvent`, not `choiceFromRules`: a synced series has no rule here
    // and "no rule" is not "does not repeat".
    recurrence: choiceForEvent(event),
    recurrenceRules: [...rules],
    reminderMinutes: reminderMinutesOf(event),
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
    startTime: allDay ? ALL_DAY_START : timeField(start),
    endDate: allDay ? utcDateField(start) : dateField(end),
    endTime: allDay ? ALL_DAY_END : timeField(end),
    location: "",
    description: "",
    attendees: "",
    calendarId: options.calendarId,
    recurrence: "none",
    recurrenceRules: [],
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
    if (start === null || last === null) return { error: "Not a date" };
    if (last < start) return { error: "Ends before it starts" };
    return { start, end: last + DAY, allDay: true };
  }
  const start = timestampFrom(form.startDate, form.startTime);
  const end = timestampFrom(form.endDate || form.startDate, form.endTime);
  if (start === null || end === null) return { error: "Not a valid date or time" };
  if (end < start) return { error: "Ends before it starts" };
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

  // Both of these used to be "send it if it is not the default", which sent the
  // rule on every save of a recurring event and could never clear one. Now that
  // the store reads them back they are diffed like every other field: named
  // only when they actually moved.
  const rules = rulesOf(form, times.start);
  if (!sameRules(rules, event.recurrence ?? [])) patch.recurrence = rules;

  const reminders = reminderMinutesOf(event);
  if (form.reminderMinutes !== null && !sameNumbers(form.reminderMinutes, reminders)) {
    patch.reminderMinutes = form.reminderMinutes;
  }

  return Object.keys(patch).length > 0 ? patch : undefined;
}

/**
 * Whether a save has to address the whole series whatever the user picks.
 *
 * How an event repeats belongs to the series master; `events.patch` on an
 * expanded occurrence refuses a `recurrence` key outright. The command layer
 * rejects it with a sentence, but the better place to notice is here, before
 * the user is offered a "this one" button that cannot work.
 */
export function requiresSeriesScope(patch: EventPatch): boolean {
  return patch.recurrence !== undefined;
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
    recurrence: rulesOf(form, times.start),
    reminderMinutes: form.reminderMinutes ?? undefined,
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
 * The draft that copies an event to times that have already been chosen — by an
 * alt-drag on the grid, or by a paste that worked out where the day starts.
 *
 * The title is carried across unchanged. `duplicateDraft` marks its copy
 * "(copy)" because it lands exactly on top of the original and there would
 * otherwise be nothing to tell the two apart; a copy that arrives somewhere
 * else is already told apart by the thing that put it there.
 */
export function copyDraft(
  event: CalendarEvent,
  at: { start: number; end: number },
): EventDraft {
  return {
    title: event.title === "(no title)" ? "" : event.title,
    description: event.description,
    location: event.location,
    startTs: at.start,
    endTs: at.end,
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
  return copyDraft(event, { start, end: start + duration });
}

/* -------------------------------------------------------------------------- */
/* Is this one of a series?                                                    */
/* -------------------------------------------------------------------------- */

/**
 * Whether an event is an occurrence of a repeating series.
 *
 * `recurringEventId` answers this exactly, and it is now carried onto
 * `CalendarEvent` by `mapEvent` — so when it is present, that is the answer and
 * nothing is inferred. A row that has one *is* an occurrence; a row from a
 * backend that returned the field and left it empty is a one-off.
 *
 * The inference below survives for the two cases that have no such field:
 * fixture data, and a row created in this session that has not been synced back
 * yet. It looks for another event on the same calendar with the same title and
 * duration on a different day.
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
  if (event.recurringEventId !== undefined) return event.recurringEventId.length > 0;
  // A window where *anything* carries the field is a window from a backend that
  // reports it, so a row without one is genuinely a one-off — guessing on top of
  // that would only ever produce false positives.
  if (others.some((other) => other.recurringEventId !== undefined)) return false;

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
