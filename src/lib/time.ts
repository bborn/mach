/** Date arithmetic and the app's formatting vocabulary. No dependencies. */

export const MINUTE = 60_000;
export const HOUR = 3_600_000;
export const DAY = 86_400_000;

/** Monday. The week grid is a work week first. */
export const WEEK_STARTS_ON = 1;

export function startOfDay(input: Date | number): Date {
  const d = new Date(input);
  d.setHours(0, 0, 0, 0);
  return d;
}

export function endOfDay(input: Date | number): Date {
  const d = startOfDay(input);
  d.setDate(d.getDate() + 1);
  return d;
}

export function addDays(input: Date | number, n: number): Date {
  const d = new Date(input);
  d.setDate(d.getDate() + n);
  return d;
}

export function addMonths(input: Date | number, n: number): Date {
  const d = new Date(input);
  const day = d.getDate();
  d.setDate(1);
  d.setMonth(d.getMonth() + n);
  // Clamp: Jan 31 + 1 month is Feb 28, not Mar 3.
  d.setDate(Math.min(day, daysInMonth(d)));
  return d;
}

export function daysInMonth(input: Date | number): number {
  const d = new Date(input);
  return new Date(d.getFullYear(), d.getMonth() + 1, 0).getDate();
}

export function startOfWeek(input: Date | number, weekStartsOn = WEEK_STARTS_ON): Date {
  const d = startOfDay(input);
  const shift = (d.getDay() - weekStartsOn + 7) % 7;
  d.setDate(d.getDate() - shift);
  return d;
}

export function startOfMonth(input: Date | number): Date {
  const d = startOfDay(input);
  d.setDate(1);
  return d;
}

export function isSameDay(a: Date | number, b: Date | number): boolean {
  const x = new Date(a);
  const y = new Date(b);
  return (
    x.getFullYear() === y.getFullYear() &&
    x.getMonth() === y.getMonth() &&
    x.getDate() === y.getDate()
  );
}

export function isToday(input: Date | number): boolean {
  return isSameDay(input, Date.now());
}

/** The seven (or one, or forty-two) days a calendar view renders. */
export function daysOfWeek(anchor: Date | number): Date[] {
  const first = startOfWeek(anchor);
  return Array.from({ length: 7 }, (_, i) => addDays(first, i));
}

/** Six full weeks, so the month grid never reflows between months. */
export function daysOfMonthGrid(anchor: Date | number): Date[] {
  const first = startOfWeek(startOfMonth(anchor));
  return Array.from({ length: 42 }, (_, i) => addDays(first, i));
}

const DOW = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_LONG = [
  "January", "February", "March", "April", "May", "June",
  "July", "August", "September", "October", "November", "December",
];

export function weekdayShort(input: Date | number): string {
  return DOW[new Date(input).getDay()];
}

export function monthShort(input: Date | number): string {
  return MONTHS[new Date(input).getMonth()];
}

export function monthYear(input: Date | number): string {
  const d = new Date(input);
  return `${MONTHS_LONG[d.getMonth()]} ${d.getFullYear()}`;
}

function pad(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

/** "9:41 AM" — used in the reading pane and event detail. */
export function clockTime(input: Date | number): string {
  const d = new Date(input);
  const h = d.getHours();
  const suffix = h < 12 ? "AM" : "PM";
  const h12 = h % 12 === 0 ? 12 : h % 12;
  return `${h12}:${pad(d.getMinutes())} ${suffix}`;
}

/** "9a" / "9:30a" — the compact form the grid axis and event chips use. */
export function shortTime(input: Date | number): string {
  const d = new Date(input);
  const h = d.getHours();
  const m = d.getMinutes();
  const suffix = h < 12 ? "a" : "p";
  const h12 = h % 12 === 0 ? 12 : h % 12;
  return m === 0 ? `${h12}${suffix}` : `${h12}:${pad(m)}${suffix}`;
}

export function timeRangeLabel(start: number, end: number): string {
  return `${shortTime(start)} – ${shortTime(end)}`;
}

/**
 * The name of the day `ts` falls on, or `null` when that day is today.
 *
 * One vocabulary, four widths: "Yesterday", then a weekday for the rest of the
 * week behind us, then "Aug 10" while the year still goes without saying, then
 * "8/10/25". Each step is the shortest phrase that is still unambiguous at that
 * distance — a weekday five weeks back names four different days, and a year on
 * a message from Tuesday is dead weight.
 *
 * Today returns `null` rather than a string, which is what lets the two callers
 * disagree about what to do with it. The thread list draws nothing else, so it
 * falls back to the clock; a message row keeps the clock either way and only
 * prefixes the day when there is one.
 */
export function dayLabel(ts: number, now: number = Date.now()): string | null {
  const d = new Date(ts);
  if (isSameDay(d, now)) return null;

  const midnight = startOfDay(now).getTime();
  if (ts >= midnight - DAY) return "Yesterday";
  if (ts >= midnight - 6 * DAY) return weekdayShort(d);
  if (d.getFullYear() === new Date(now).getFullYear()) {
    return `${monthShort(d)} ${d.getDate()}`;
  }
  return `${d.getMonth() + 1}/${d.getDate()}/${String(d.getFullYear()).slice(2)}`;
}

/**
 * Thread-list time. Ages out from clock, to weekday, to date, to short date —
 * so a fixed-width column always says the most useful thing it can.
 */
export function listTime(ts: number, now: number = Date.now()): string {
  return dayLabel(ts, now) ?? clockTime(ts);
}

/**
 * Per-message time: "7:08 AM" today, "Yesterday 7:08 AM" and "Tue 7:08 AM"
 * before that.
 *
 * A message row used to draw the clock alone. In a thread that spans days —
 * which is most threads worth reopening — that is not a shortened answer, it is
 * no answer: five rows reading 7:08 AM, 7:35 AM, 10:34 AM, 12:05 PM, 12:14 PM
 * could be one busy morning or five months, and the row order reads the same
 * either way.
 *
 * The day goes in front and the clock stays flush against the right edge of the
 * column, so the clocks still line up down the thread even though the prefixes
 * are four different widths; that is what the `tabular-nums` on the column was
 * bought for. Today stays a bare clock, both because the date of the
 * conversation you are in is noise and because the widest label — "Yesterday",
 * at eighteen characters with a 12-hour clock behind it — is the one the column
 * has to be sized for, and today's rows are the ones that would pay for it most
 * often.
 */
export function messageTime(ts: number, now: number = Date.now()): string {
  const day = dayLabel(ts, now);
  const clock = clockTime(ts);
  return day ? `${day} ${clock}` : clock;
}

export function fullDate(ts: number): string {
  const d = new Date(ts);
  return `${weekdayShort(d)}, ${monthShort(d)} ${d.getDate()}, ${d.getFullYear()}`;
}

/** Fractional hours since the start of `day` — the week grid's y coordinate. */
export function hoursFromDayStart(ts: number, day: Date | number): number {
  return (ts - startOfDay(day).getTime()) / HOUR;
}
