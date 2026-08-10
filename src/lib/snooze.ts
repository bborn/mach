/**
 * When a snooze wakes.
 *
 * `b` used to be a single hardcoded instant — `Date.now() + DAY`, twenty-four
 * hours from whenever the key was pressed, with nothing on screen saying so.
 * That is not what snooze means in any mail client anybody has used: the whole
 * point of the verb is choosing *when*, and a fixed offset from the moment of
 * the keystroke lands at a different time of day every time you press it.
 *
 * So this module is the arithmetic behind a picker, and it is deliberately
 * separate from the picker: resolving "this weekend" is date maths with a
 * dozen edge cases (a Saturday, a Sunday, the last day of a month, a DST
 * boundary) and none of it needs a DOM to be checked. `calendar-geometry.ts`
 * is the same split for the same reason.
 *
 * # The instants
 *
 * Gmail's, because the standing rule of this app is not to invent a vocabulary
 * the user has to learn twice:
 *
 *   Later today    three hours from now, rounded up to the hour
 *   Tomorrow       tomorrow at 8am
 *   This weekend   the coming Saturday at 8am
 *   Next week      the coming Monday at 8am
 *   Pick a date…   whatever the user types, through `chrono-node`
 *
 * Two of them are conditional, and both conditions are the same idea — an
 * option that resolves to a time that has already gone, or to a moment inside
 * the period it names, is not a choice, it is a trap:
 *
 *  * **Later today** disappears once three hours from now is tomorrow. At
 *    half past ten at night it would say "Later today" and mean 1:30am, which
 *    is neither later nor today.
 *  * **This weekend** disappears on Saturday and Sunday. You are in it.
 *
 * # Why 8am and not 9

 * It is when Gmail wakes a snoozed thread, and the number is not the
 * interesting part — the *consistency* is. Every option that lands on a future
 * day lands at the same hour of it, so "tomorrow" and "next week" are the same
 * promise at different distances rather than two different guesses.
 */

import * as chrono from "chrono-node";
import { pmShift } from "./calendar-nlp";
import { HOUR, addDays, clockTime, isSameDay, monthShort, startOfDay, weekdayShort } from "./time";

/** The hour a thread comes back on any day that is not today. */
export const SNOOZE_MORNING_HOUR = 8;

/** How far "Later today" reaches, before rounding up to the hour. */
export const LATER_TODAY_HOURS = 3;

export type SnoozeOptionId =
  | "later-today"
  | "tomorrow"
  | "this-weekend"
  | "next-week"
  | "custom";

export interface SnoozeOption {
  id: SnoozeOptionId;
  /** The name of the choice. A label, not a sentence. */
  label: string;
  /**
   * The instant it resolves to, or `null` for the one that has to be typed.
   *
   * Every option carries this so the picker can print it next to the name. A
   * menu of relative phrases with no times under them makes the user do the
   * arithmetic this module exists to do, and get it wrong on a Friday.
   */
  at: number | null;
}

/**
 * The choices, in the order they are offered.
 *
 * Order is by distance, which is also the order the number keys run in — and
 * because the unavailable options are dropped rather than disabled, the number
 * beside a choice is its position in *this* list, not a fixed id. A greyed-out
 * row you cannot pick is a row you have to read and then skip past every time.
 *
 * # No instant is offered twice
 *
 * Two of these names collide with a third on particular days, and the collision
 * is total — not "nearly the same", the identical millisecond:
 *
 *  * On a **Friday**, "Tomorrow" and "This weekend" are both Saturday 8am.
 *  * On a **Sunday**, "Tomorrow" and "Next week" are both Monday 8am, because
 *    the week starts on Monday (`WEEK_STARTS_ON`) and tomorrow begins it.
 *
 * Offering both is offering a choice that is not one: whichever the user picks,
 * the thread comes back at the same moment, so the second row can only ever
 * cost them a decision. The nearer name wins, because it is the more concrete
 * of the two — "Tomorrow" tells you when; "This weekend" tells you when *if*
 * you already know what day it is.
 *
 * This is a filter over the resolved instants rather than two more weekday
 * conditions, because the condition it is really expressing is the one worth
 * enforcing: distinct rows resolve to distinct times.
 */
export function snoozeOptions(now: number): SnoozeOption[] {
  const out: SnoozeOption[] = [];

  const later = laterToday(now);
  if (later !== null) out.push({ id: "later-today", label: "Later today", at: later });

  out.push({ id: "tomorrow", label: "Tomorrow", at: atHour(addDays(now, 1), SNOOZE_MORNING_HOUR) });

  const weekend = thisWeekend(now);
  if (weekend !== null) out.push({ id: "this-weekend", label: "This weekend", at: weekend });

  out.push({ id: "next-week", label: "Next week", at: nextWeek(now) });

  const seen = new Set<number>();
  const distinct = out.filter((option) => {
    if (option.at === null || seen.has(option.at)) return false;
    seen.add(option.at);
    return true;
  });

  distinct.push({ id: "custom", label: "Pick a date & time", at: null });
  return distinct;
}

/**
 * Three hours out, rounded up to a whole hour — or `null` once that is
 * tomorrow.
 *
 * Rounding up rather than to the nearest hour is what keeps the option honest
 * about its own name: from 2:50pm the nearest hour is 3pm, ten minutes away,
 * which is not "later today" by any reading.
 */
export function laterToday(now: number): number | null {
  const raw = now + LATER_TODAY_HOURS * HOUR;
  const at = Math.ceil(raw / HOUR) * HOUR;
  return isSameDay(at, now) ? at : null;
}

/** The coming Saturday at 8am, or `null` if it is already the weekend. */
export function thisWeekend(now: number): number | null {
  const day = new Date(now).getDay();
  if (day === 0 || day === 6) return null;
  return nextWeekdayAt(now, 6, SNOOZE_MORNING_HOUR);
}

/** The coming Monday at 8am. From a Monday that means the next one. */
export function nextWeek(now: number): number {
  return nextWeekdayAt(now, 1, SNOOZE_MORNING_HOUR);
}

/**
 * The next `weekday` strictly after today, at `hour`.
 *
 * Strictly after *today*, not after *now*: "next week" pressed at 6am on a
 * Monday means the Monday ahead, not two hours' time. The unit a weekday name
 * picks out is the day, so that is the unit the comparison uses.
 */
export function nextWeekdayAt(now: number, weekday: number, hour: number): number {
  const shift = ((weekday - new Date(now).getDay() + 7) % 7) || 7;
  return atHour(addDays(now, shift), hour);
}

function atHour(day: Date | number, hour: number): number {
  const d = startOfDay(day);
  d.setHours(hour);
  return d.getTime();
}

/**
 * What the user typed, as an instant — or `null` when it is not one yet.
 *
 * `chrono-node` does the parsing, as it does for natural-language event entry
 * (`calendar-nlp.ts`) and for the date and time fields (`datetime-field.ts`).
 * Two conventions come with it, and both are shared with those modules on
 * purpose, because nobody should have to remember which box they are typing
 * into:
 *
 *  1. A bare weekday means the next one — `forwardDate`.
 *  2. A bare hour of 1–6 means the afternoon — `pmShift`. "tuesday at 3"
 *     is three in the afternoon.
 *
 * One convention is this module's own: a date with no time in it wakes at
 * {@link SNOOZE_MORNING_HOUR}, the same hour every other option lands on.
 * Chrono would put "next tuesday" at midnight, which is a wake time nobody has
 * ever wanted from a mail client.
 *
 * An instant already in the past is `null` rather than a value. A snooze is a
 * promise about the future; the honest answer to "yesterday 9am" is that the
 * field does not have a date yet, and the picker says so rather than silently
 * waking the thread on the next sweep.
 */
export function parseSnoozeAt(text: string, now: number): number | null {
  const trimmed = text.trim();
  if (!trimmed) return null;

  const parsed = chrono.parse(trimmed, new Date(now), { forwardDate: true })[0];
  if (!parsed) return null;

  const timed = parsed.start.isCertain("hour");
  const at = timed
    ? parsed.start.date().getTime() + pmShift(parsed.start)
    : atHour(parsed.start.date(), SNOOZE_MORNING_HOUR);

  return at > now ? at : null;
}

/**
 * The instant, phrased for a row in the picker: "Today, 5:00 PM".
 *
 * It ages out the same way `listTime` does — clock, weekday, date — because a
 * full date beside "Tomorrow" is noise and a bare weekday beside something
 * five weeks out is a riddle.
 */
export function snoozeWhen(at: number, now: number): string {
  const time = clockTime(at);
  if (isSameDay(at, now)) return `Today, ${time}`;
  if (isSameDay(at, addDays(now, 1))) return `Tomorrow, ${time}`;

  const days = Math.round((startOfDay(at).getTime() - startOfDay(now).getTime()) / (24 * HOUR));
  if (days > 1 && days < 7) return `${weekdayShort(at)}, ${time}`;

  const d = new Date(at);
  const year = d.getFullYear() === new Date(now).getFullYear() ? "" : ` ${d.getFullYear()}`;
  return `${weekdayShort(at)}, ${monthShort(at)} ${d.getDate()}${year}, ${time}`;
}

/** "Snoozed 3 conversations until Tomorrow, 8:00 AM" — the undo stack's label. */
export function snoozeLabel(count: number, at: number, now: number): string {
  const what = count === 1 ? "conversation" : "conversations";
  return `Snoozed ${count} ${what} until ${snoozeWhen(at, now)}`;
}

/** Wrapping cursor movement, the same loop the palette's ↑/↓ run. */
export function moveSnoozeCursor(cursor: number, delta: number, count: number): number {
  if (count === 0) return 0;
  return (cursor + delta + count) % count;
}
