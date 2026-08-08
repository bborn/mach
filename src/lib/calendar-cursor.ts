/**
 * Where the keyboard cursor goes next, and which events a typed query means.
 *
 * Two jobs, one file, because they are the same job seen twice: both answer
 * "which event does the user mean now" from the set currently on screen, and
 * both have to be right about ties, empty days and the edges of the window.
 * Neither touches React, so both are testable as arithmetic.
 *
 * # Why the arrows changed hands
 *
 * They used to move the *date window*: left was "last week". That is Google
 * Calendar's own binding and it was copied deliberately — but it also meant the
 * gesture every user reaches for first did the thing they were least likely to
 * want, and left the events themselves reachable only by Tab.
 *
 * So the range keeps the letters it already had — `n`/`j` forward, `p`/`k`
 * back, `t` for today, which is Google's own map minus the arrows — and the
 * arrows move between events. Nothing was invented: the letters are the ones
 * muscle memory already has, and the arrows now do the obvious thing.
 *
 * # The shape of arrow movement
 *
 * Up and down walk a day in time order, then fall through to the next day that
 * has anything on it. Left and right cross to the nearest day that has an
 * event and keep the time of day as close as they can, which is what makes a
 * week of 9am standups feel like a row you can run along.
 *
 * Walking off the end does not dead-end. It asks the caller to page the view
 * and land on the first (or last) event of the period it arrives in, so holding
 * an arrow crosses weeks the way holding it in a list crosses pages.
 */

import type { CalendarEvent, EventId } from "@/types";
import { startOfDay } from "./time";

/** What an arrow key resolved to. */
export type CursorMove =
  /** Select this event. */
  | { kind: "event"; id: EventId }
  /**
   * Nothing further in this direction inside the visible window. Move the
   * range by `delta` periods and select the event at the edge the cursor is
   * arriving from — `edge` says which.
   */
  | { kind: "page"; delta: 1 | -1; edge: "first" | "last" }
  /** Nothing to do: there are no events at all. */
  | { kind: "none" };

export type Arrow = "up" | "down" | "left" | "right";

/**
 * The calendar day an event belongs to, as a comparable `YYYYMMDD` integer.
 *
 * Two zones, on purpose. A timed event is read in the local zone, because that
 * is the column it is drawn in. An all-day event is pinned to *UTC* midnight by
 * the store, so reading a local day off one lands on the previous evening
 * anywhere west of Greenwich — a Saturday all-day event would answer "Friday"
 * in California and the cursor would step through it in the wrong column.
 */
export function dayKey(event: CalendarEvent): number {
  const d = new Date(event.start);
  return event.allDay
    ? d.getUTCFullYear() * 10000 + (d.getUTCMonth() + 1) * 100 + d.getUTCDate()
    : d.getFullYear() * 10000 + (d.getMonth() + 1) * 100 + d.getDate();
}

/** Minutes past midnight — the axis left/right tries to preserve. */
function timeOfDay(event: CalendarEvent): number {
  return event.allDay ? 0 : event.start - startOfDay(event.start).getTime();
}

/**
 * Reading order for the grid: by day, then by time within the day.
 *
 * Sorting by day *first* rather than by raw timestamp is what keeps a day's
 * events contiguous in the list even when an all-day row is in it — its
 * UTC-midnight start would otherwise sort it into the previous evening. With
 * this ordering the neighbour of an event is always either the next event in
 * its own column or the first event of the next occupied column, which is what
 * both Tab and the down arrow mean.
 */
export function inReadingOrder(events: readonly CalendarEvent[]): CalendarEvent[] {
  return [...events].sort(
    (a, b) => dayKey(a) - dayKey(b) || a.start - b.start || a.id - b.id,
  );
}

/**
 * One step along reading order — Tab, and the up/down arrows.
 *
 * They are deliberately the same movement. "The next event in this day, and
 * then the first one tomorrow" *is* reading order across a week grid, so
 * giving the arrows their own subtly different walk would only make the two
 * disagree in cases nobody could predict.
 */
export function stepCursor(
  events: readonly CalendarEvent[],
  currentId: EventId | null,
  delta: 1 | -1,
): CursorMove {
  const ordered = inReadingOrder(events);
  if (ordered.length === 0) return { kind: "none" };

  const index = ordered.findIndex((e) => e.id === currentId);
  if (index === -1) {
    return { kind: "event", id: (delta > 0 ? ordered[0] : ordered[ordered.length - 1]).id };
  }
  const next = index + delta;
  if (next < 0) return { kind: "page", delta: -1, edge: "last" };
  if (next >= ordered.length) return { kind: "page", delta: 1, edge: "first" };
  return { kind: "event", id: ordered[next].id };
}

/**
 * Where an arrow key moves the cursor.
 *
 * `events` is what is on screen — already filtered by calendar, by account and
 * by the declined toggle — because the cursor must only ever land on a block
 * the user can actually see.
 */
export function arrowCursor(
  events: readonly CalendarEvent[],
  currentId: EventId | null,
  arrow: Arrow,
): CursorMove {
  const ordered = inReadingOrder(events);
  if (ordered.length === 0) return { kind: "none" };

  const current = ordered.find((e) => e.id === currentId);
  if (!current) {
    // No cursor yet. Any arrow adopts one at the end of the grid the key points
    // away from, so the first press always moves *into* the week.
    const forwards = arrow === "down" || arrow === "right";
    return { kind: "event", id: (forwards ? ordered[0] : ordered[ordered.length - 1]).id };
  }

  if (arrow === "up" || arrow === "down") {
    return stepCursor(ordered, current.id, arrow === "down" ? 1 : -1);
  }
  return acrossDays(ordered, current, arrow === "right" ? 1 : -1);
}

/**
 * Left/right: the nearest event on another day, at the nearest time.
 *
 * "The nearest day that has anything", not "the next column" — an empty
 * Wednesday should cost one keypress, not three. Within that day the closest
 * start time wins, so running right along a week of 9am standups stays on the
 * standups even when the days around them are full of other things.
 */
function acrossDays(
  ordered: readonly CalendarEvent[],
  current: CalendarEvent,
  delta: 1 | -1,
): CursorMove {
  const day = dayKey(current);
  const candidates = ordered.filter((e) => (delta > 0 ? dayKey(e) > day : dayKey(e) < day));
  if (candidates.length === 0) {
    return delta > 0
      ? { kind: "page", delta: 1, edge: "first" }
      : { kind: "page", delta: -1, edge: "last" };
  }

  const days = candidates.map(dayKey);
  const targetDay = delta > 0 ? Math.min(...days) : Math.max(...days);
  const onTargetDay = candidates.filter((e) => dayKey(e) === targetDay);

  const wanted = timeOfDay(current);
  let best = onTargetDay[0];
  let bestGap = Math.abs(timeOfDay(best) - wanted);
  for (const candidate of onTargetDay.slice(1)) {
    const gap = Math.abs(timeOfDay(candidate) - wanted);
    // Strictly better only, so an exact tie keeps the earlier event — the one
    // higher up the column, and the one the eye is already level with.
    if (gap < bestGap) {
      best = candidate;
      bestGap = gap;
    }
  }
  return { kind: "event", id: best.id };
}

/* -------------------------------------------------------------------------- */
/* Type to select                                                              */
/* -------------------------------------------------------------------------- */

/**
 * Events matching a typed query, best first.
 *
 * Substring, not fuzzy. Fuzzy is the right call over thousands of files with
 * distinctive names; over a week of "Standup", "Design review" and "1:1 with
 * Ada" it mostly produces surprising second and third places, and the user is
 * choosing from a set they can already see. So a plain substring, ranked by
 * *where* it lands.
 *
 * Location is searched too, because "the one in Room 2" is a real way to
 * remember a meeting whose name you cannot.
 */
export function matchEvents(events: readonly CalendarEvent[], query: string): CalendarEvent[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [];

  const scored: { event: CalendarEvent; score: number }[] = [];
  for (const event of events) {
    const score = scoreEvent(event, needle);
    if (score > 0) scored.push({ event, score });
  }
  // Score first, then reading order. Two equally good matches are offered the
  // way they are drawn, so Tab walks down the week rather than around it.
  scored.sort(
    (a, b) =>
      b.score - a.score ||
      dayKey(a.event) - dayKey(b.event) ||
      a.event.start - b.event.start ||
      a.event.id - b.event.id,
  );
  return scored.map((row) => row.event);
}

function scoreEvent(event: CalendarEvent, needle: string): number {
  const title = event.title.toLowerCase();
  if (title.startsWith(needle)) return 3;
  // A match at a word boundary — "rev" finding "Design review" — is what a
  // person means far more often than one buried mid-word.
  if (new RegExp(`\\b${escapeRegExp(needle)}`).test(title)) return 2;
  if (title.includes(needle)) return 1;
  if ((event.location ?? "").toLowerCase().includes(needle)) return 1;
  return 0;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
