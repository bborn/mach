/**
 * Typing a date, and typing a time.
 *
 * `<input type="date">` is the reason the event modal looked like a web form:
 * WebKit draws it as three spin-boxes with a stepper, in the platform's own
 * metrics, and nothing in a stylesheet can reach inside. Replacing it with a
 * text box is only an improvement if the text box is *more* forgiving than the
 * thing it replaced, so this module is the forgiving half.
 *
 * The contract is deliberately narrow. The form still stores `yyyy-mm-dd` and
 * `HH:mm` strings — exactly what `calendar-edit.ts` has always converted to
 * timestamps, and exactly what the native inputs used to produce — so nothing
 * downstream of the field knows the control changed. This is presentation and
 * parsing, nothing else.
 *
 * # What it accepts
 *
 *   dates   2026-08-05 · 8/5 · 8/5/26 · 08.05.2026 · 5 · aug 5 · next tuesday
 *           tomorrow · in 3 weeks
 *   times   9 · 9:30 · 930 · 9a · 9:30 pm · 21:30 · noon · midnight
 *
 * Anything the explicit patterns miss goes to `chrono-node`, which the app
 * already depends on for natural-language event entry (`calendar-nlp.ts`). Two
 * conventions are shared with that module on purpose, because a person should
 * not have to know which field they are typing into:
 *
 *  1. **Bare hours of 1–6 mean the afternoon.** Nobody schedules a standup at
 *     01:30. `9` is nine in the morning; `1` is one in the afternoon.
 *  2. **A bare weekday means the next one.** `friday` is the Friday ahead.
 *
 * A numeric `8/5` is the exception to (2): it takes the year already in the
 * field rather than rolling forward, because someone typing digits is naming a
 * date, not asking for the next occurrence of one.
 */

import * as chrono from "chrono-node";
import { clockTime, fullDate } from "./time";

const ISO = /^(\d{4})-(\d{1,2})-(\d{1,2})$/;
const NUMERIC = /^(\d{1,2})[/.-](\d{1,2})(?:[/.-](\d{2}|\d{4}))?$/;
const DAY_ONLY = /^(\d{1,2})$/;

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

/** `yyyy-mm-dd` for a local Date. */
export function dateValue(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/** A `yyyy-mm-dd` string as a local Date at midnight, or `null` if it is not one. */
export function dateFromValue(value: string): Date | null {
  const match = value.match(ISO);
  if (!match) return null;
  const [, y, m, d] = match;
  const date = new Date(Number(y), Number(m) - 1, Number(d));
  if (
    date.getFullYear() !== Number(y) ||
    date.getMonth() !== Number(m) - 1 ||
    date.getDate() !== Number(d)
  ) {
    return null;
  }
  return date;
}

/** "Wed, Aug 5, 2026" — what the field shows when it is not being typed in. */
export function formatDateField(value: string): string {
  const date = dateFromValue(value);
  return date ? fullDate(date.getTime()) : value;
}

/** "12:30 PM" — likewise, and it parses back to the same `HH:mm`. */
export function formatTimeField(value: string): string {
  const match = value.match(/^(\d{1,2}):(\d{2})$/);
  if (!match) return value;
  const hour = Number(match[1]);
  const minute = Number(match[2]);
  if (hour > 23 || minute > 59) return value;
  const date = new Date(2000, 0, 1, hour, minute);
  return clockTime(date);
}

export interface ParseDateOptions {
  /** Now, for "tomorrow" and for the default year. Defaults to `Date.now()`. */
  now?: number;
  /** The value already in the field: supplies the year and month for `8/5`, `5`. */
  current?: string;
}

/**
 * A `yyyy-mm-dd` out of whatever was typed, or `null` if it is not a date.
 *
 * `null` is a real answer, not a failure to try: the caller keeps the text on
 * screen and marks the field invalid rather than silently snapping it to
 * something the user did not ask for.
 */
export function parseDateField(text: string, options: ParseDateOptions = {}): string | null {
  const trimmed = text.trim();
  if (!trimmed) return null;

  const now = options.now ?? Date.now();
  const anchor = (options.current ? dateFromValue(options.current) : null) ?? new Date(now);

  const iso = dateFromValue(trimmed);
  if (iso) return dateValue(iso);

  const numeric = trimmed.match(NUMERIC);
  if (numeric) {
    const month = Number(numeric[1]);
    const day = Number(numeric[2]);
    const year = numeric[3] === undefined ? anchor.getFullYear() : expandYear(numeric[3]);
    return checked(year, month, day);
  }

  const dayOnly = trimmed.match(DAY_ONLY);
  if (dayOnly) {
    return checked(anchor.getFullYear(), anchor.getMonth() + 1, Number(dayOnly[1]));
  }

  const parsed = chrono.parse(trimmed, new Date(now), { forwardDate: true })[0];
  if (!parsed) return null;
  return dateValue(parsed.start.date());
}

/** `HH:mm` out of whatever was typed, or `null`. */
export function parseTimeField(text: string): string | null {
  const trimmed = text.trim().toLowerCase().replace(/\./g, "");
  if (!trimmed) return null;
  if (trimmed === "noon" || trimmed === "midday") return "12:00";
  if (trimmed === "midnight") return "00:00";

  const meridiem = trimmed.match(/^(\d{1,2})(?::?(\d{2}))?\s*(am?|pm?)$/);
  if (meridiem) {
    let hour = Number(meridiem[1]);
    const minute = Number(meridiem[2] ?? "0");
    if (hour > 12 || minute > 59) return null;
    const pm = meridiem[3].startsWith("p");
    if (hour === 12) hour = pm ? 12 : 0;
    else if (pm) hour += 12;
    return `${pad(hour)}:${pad(minute)}`;
  }

  const clock = trimmed.match(/^(\d{1,2}):(\d{2})$/);
  if (clock) {
    const minute = Number(clock[2]);
    if (minute > 59) return null;
    const hour = afternoonish(Number(clock[1]));
    return hour === null ? null : `${pad(hour)}:${pad(minute)}`;
  }

  const bare = trimmed.match(/^(\d{1,2})$/);
  if (bare) {
    const hour = afternoonish(Number(bare[1]));
    return hour === null ? null : `${pad(hour)}:00`;
  }

  // "930", "1430" — the way a keyboard actually types a time when it is in a
  // hurry, and the one shape no date library recognises.
  const digits = trimmed.match(/^(\d{1,2})(\d{2})$/);
  if (digits) {
    const hour = Number(digits[1]);
    const minute = Number(digits[2]);
    if (hour > 23 || minute > 59) return null;
    return `${pad(hour)}:${pad(minute)}`;
  }

  const parsed = chrono.parse(trimmed, undefined, { forwardDate: true })[0];
  if (!parsed || !parsed.start.isCertain("hour")) return null;
  const date = parsed.start.date();
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** The half-hour ladder the time popover offers, as `HH:mm`. */
export function timeChoices(stepMinutes = 30): string[] {
  const out: string[] = [];
  for (let minutes = 0; minutes < 24 * 60; minutes += stepMinutes) {
    out.push(`${pad(Math.floor(minutes / 60))}:${pad(minutes % 60)}`);
  }
  return out;
}

/** `yyyy-mm-dd`, `n` months along, clamped (Jan 31 + 1 month is Feb 28). */
export function addMonthsToValue(value: string, n: number): string {
  const date = dateFromValue(value) ?? new Date();
  const day = date.getDate();
  const moved = new Date(date.getFullYear(), date.getMonth(), 1);
  moved.setMonth(moved.getMonth() + n);
  const last = new Date(moved.getFullYear(), moved.getMonth() + 1, 0).getDate();
  moved.setDate(Math.min(day, last));
  return dateValue(moved);
}

/** `yyyy-mm-dd`, `n` days along. */
export function addDaysToValue(value: string, n: number): string {
  const date = dateFromValue(value) ?? new Date();
  date.setDate(date.getDate() + n);
  return dateValue(date);
}

function expandYear(digits: string): number {
  const n = Number(digits);
  return digits.length === 2 ? 2000 + n : n;
}

/** `null` unless the three numbers name a real day. */
function checked(year: number, month: number, day: number): string | null {
  if (month < 1 || month > 12 || day < 1 || day > 31) return null;
  const date = new Date(year, month - 1, day);
  if (date.getMonth() !== month - 1 || date.getDate() !== day) return null;
  return dateValue(date);
}

/** The 1–6 rule, shared with `calendar-nlp`. `null` for an impossible hour. */
function afternoonish(hour: number): number | null {
  if (hour > 23) return null;
  return hour >= 1 && hour <= 6 ? hour + 12 : hour;
}
