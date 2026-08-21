/**
 * The arithmetic behind dragging on the week grid.
 *
 * Everything here is pure: pixels and timestamps in, timestamps out. The React
 * side owns the pointer events and the transforms; this owns the question
 * "given that the pointer moved 37px down and one column right, when is the
 * event now?" — which is the part that has edge cases and therefore the part
 * that gets tested.
 *
 * Three rules run through all of it:
 *
 *  1. **Snap on the way out, never on the way in.** A drag accumulates a raw
 *     pixel delta and snaps the *result*, so a slow 3px-at-a-time drag lands on
 *     the same time as one fast flick. Snapping each increment would make the
 *     event creep.
 *  2. **Day changes are calendar days, not 86,400,000ms.** `addDays` walks the
 *     local calendar, so dragging a 9am meeting across a DST boundary lands on
 *     9am, not 8am.
 *  3. **Duration is preserved by a move and only changed by a resize.** They
 *     are separate functions for that reason.
 */

import type { EventId } from "@/types";
import { DEFAULT_EVENT_MINUTES, HOUR_HEIGHT, SNAP_MINUTES, snapTime } from "./calendar-geometry";
import { DAY, HOUR, MINUTE, addDays, startOfDay } from "./time";

/** Nothing may be dragged shorter than this. One snap step. */
export const MIN_EVENT_MINUTES = SNAP_MINUTES;

/** How far the pointer must travel before a press becomes a drag. */
export const DRAG_THRESHOLD_PX = 3;

export type ResizeEdge = "start" | "end";
export type DragKind = "move" | "resize";

/** Where a drag began. Fixed at pointer-down and never recomputed. */
export interface DragOrigin {
  start: number;
  end: number;
  /** Local midnight of the column the event was grabbed in. */
  dayStart: number;
}

export interface DragOutcome {
  start: number;
  end: number;
}

/** What a finished drag asks the calendar to save. */
export interface EventMove {
  eventId: EventId;
  start: number;
  end: number;
  /** Alt was held: leave the original where it is and create a copy here. */
  copy: boolean;
  /** The source was all-day; the write must stay all-day. */
  allDay: boolean;
}

/** Pixels of vertical travel, as milliseconds. */
export function pixelsToMs(dy: number): number {
  return (dy / HOUR_HEIGHT) * HOUR;
}

/** The inverse — how far a duration reaches down the grid. */
export function msToPixels(ms: number): number {
  return (ms / HOUR) * HOUR_HEIGHT;
}

/**
 * Shift a timestamp by whole calendar days.
 *
 * Not `ts + n * DAY`: across a DST boundary that lands an hour out, and a
 * calendar that moves your 9am standup to 8am once a year is a calendar you
 * stop trusting.
 */
export function shiftDays(ts: number, days: number): number {
  if (days === 0) return ts;
  return addDays(ts, days).getTime();
}

/**
 * Which day column a client x falls in.
 *
 * Clamped to the visible range: dragging off the left edge of the week parks
 * the event on Monday rather than inventing a column that is not rendered.
 */
export function dayIndexAt(
  clientX: number,
  contentLeft: number,
  contentWidth: number,
  dayCount: number,
): number {
  if (dayCount <= 0 || contentWidth <= 0) return 0;
  const column = Math.floor(((clientX - contentLeft) / contentWidth) * dayCount);
  return Math.min(Math.max(column, 0), dayCount - 1);
}

/**
 * Where a moved event ends up.
 *
 * `dayDelta` is how many columns to the right the pointer travelled, already
 * resolved to whole days by the caller — the grid knows which columns are
 * rendered (weekends can be hidden) and this does not.
 */
export function moveResult(
  origin: DragOrigin,
  dy: number,
  dayDelta: number,
  options: { snapMinutes?: number } = {},
): DragOutcome {
  const duration = origin.end - origin.start;
  const snapped = snapTime(origin.start + pixelsToMs(dy), options.snapMinutes ?? SNAP_MINUTES);
  const start = shiftDays(snapped, dayDelta);
  return { start, end: start + duration };
}

/**
 * Where a resized event ends up.
 *
 * The dragged edge moves and the other one does not. The minimum duration is
 * enforced against the *fixed* edge, so pulling the top edge past the bottom
 * pins it one snap step above rather than inverting the event.
 */
export function resizeResult(
  origin: DragOrigin,
  edge: ResizeEdge,
  dy: number,
  options: { snapMinutes?: number; minMinutes?: number } = {},
): DragOutcome {
  const snap = options.snapMinutes ?? SNAP_MINUTES;
  const min = (options.minMinutes ?? MIN_EVENT_MINUTES) * MINUTE;

  if (edge === "end") {
    const end = snapTime(origin.end + pixelsToMs(dy), snap);
    return { start: origin.start, end: Math.max(end, origin.start + min) };
  }
  const start = snapTime(origin.start + pixelsToMs(dy), snap);
  return { start: Math.min(start, origin.end - min), end: origin.end };
}

/**
 * The block a drag on empty grid describes.
 *
 * `anchor` is where the pointer went down (already snapped down to the quarter
 * hour), `edge` is where it is now. Dragging upwards is the same gesture as
 * dragging downwards, so the two are sorted rather than rejected.
 */
export function createResult(
  anchor: number,
  edge: number | null,
  options: { snapMinutes?: number; defaultMinutes?: number } = {},
): DragOutcome {
  const snap = options.snapMinutes ?? SNAP_MINUTES;
  const fallback = (options.defaultMinutes ?? DEFAULT_EVENT_MINUTES) * MINUTE;
  if (edge === null) return { start: anchor, end: anchor + fallback };

  const snapped = snapTime(edge, snap);
  // A press with no meaningful travel is the common case — a plain click makes
  // a default-length event, so the grid never demands a drag.
  if (Math.abs(snapped - anchor) < snap * MINUTE) {
    return { start: anchor, end: anchor + fallback };
  }
  return {
    start: Math.min(anchor, snapped),
    end: Math.max(anchor, snapped),
  };
}

/**
 * Nudge an event by keyboard.
 *
 * Same outcomes as the pointer, reached without one: `move` slides both ends,
 * `resize` moves one. `units` is signed and counted in snap steps for time and
 * in calendar days for `axis: "day"`.
 */
export function nudge(
  origin: DragOrigin,
  action:
    | { kind: "move"; axis: "time"; steps: number }
    | { kind: "move"; axis: "day"; days: number }
    | { kind: "resize"; edge: ResizeEdge; steps: number },
  options: { snapMinutes?: number; minMinutes?: number } = {},
): DragOutcome {
  const snap = options.snapMinutes ?? SNAP_MINUTES;
  const min = (options.minMinutes ?? MIN_EVENT_MINUTES) * MINUTE;

  if (action.kind === "move") {
    if (action.axis === "day") {
      return {
        start: shiftDays(origin.start, action.days),
        end: shiftDays(origin.end, action.days),
      };
    }
    const delta = action.steps * snap * MINUTE;
    return { start: origin.start + delta, end: origin.end + delta };
  }

  const delta = action.steps * snap * MINUTE;
  if (action.edge === "end") {
    return { start: origin.start, end: Math.max(origin.end + delta, origin.start + min) };
  }
  return { start: Math.min(origin.start + delta, origin.end - min), end: origin.end };
}

/**
 * The label that follows the pointer: "9:15 – 10:00", or the date too when the
 * drag has crossed into another day.
 *
 * Live feedback is the whole reason a drag feels safe. Without it the user is
 * aiming at a 48px-per-hour grid and hoping.
 */
export function dragLabel(
  outcome: DragOutcome,
  options: { showDate?: boolean; allDay?: boolean } = {},
): string {
  if (options.allDay) return "All day";
  const time = `${clock(outcome.start)} – ${clock(outcome.end)}`;
  if (!options.showDate) return time;
  const d = new Date(outcome.start);
  return `${WEEKDAYS[d.getDay()]} ${d.getDate()} · ${time}`;
}

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

function clock(ts: number): string {
  const d = new Date(ts);
  const h = d.getHours();
  const m = d.getMinutes();
  const suffix = h < 12 ? "am" : "pm";
  const h12 = h % 12 === 0 ? 12 : h % 12;
  return m === 0 ? `${h12}${suffix}` : `${h12}:${String(m).padStart(2, "0")}${suffix}`;
}

/** True when a drag has left the day it started in. */
export function crossesDay(outcome: DragOutcome, dayStart: number): boolean {
  return startOfDay(outcome.start).getTime() !== dayStart;
}

/**
 * Is this drag making a copy rather than moving the original?
 *
 * Alt held during a move is the gesture every file manager and most calendars
 * use for "leave that one where it is". A *resize* with alt held is not: there
 * is no second event a stretched edge could describe, so the modifier is
 * ignored there rather than doing something surprising with it.
 *
 * The answer is recomputed from every event that carries a modifier state, not
 * latched at pointer-down: alt is a thing you reach for once the drag is
 * already under way and you can see where it is going.
 */
export function isCopyDrag(kind: DragKind, altKey: boolean): boolean {
  return kind === "move" && altKey;
}

/**
 * Shift an all-day instant by whole UTC calendar days.
 *
 * All-day rows are pinned to UTC midnight. `shiftDays` walks the *local*
 * calendar, so across a DST boundary a Tuesday holiday becomes 23:00 UTC and
 * the grid draws it on Monday. Adding to the UTC date keeps the day Google
 * named.
 */
export function shiftAllDay(ts: number, days: number): number {
  if (days === 0) return ts;
  const d = new Date(ts);
  return Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate() + days);
}

/**
 * Slide an event by whole days, preserving duration and all-day-ness.
 *
 * Month-grid drags and all-day week drags have no hour to aim at — only a
 * column — so they go through here rather than through `moveResult`.
 */
export function moveByDays(
  origin: { start: number; end: number; allDay: boolean },
  dayDelta: number,
): DragOutcome {
  if (dayDelta === 0) return { start: origin.start, end: origin.end };
  if (origin.allDay) {
    return {
      start: shiftAllDay(origin.start, dayDelta),
      end: shiftAllDay(origin.end, dayDelta),
    };
  }
  return {
    start: shiftDays(origin.start, dayDelta),
    end: shiftDays(origin.end, dayDelta),
  };
}

/**
 * Which cell of a row-major day grid the pointer is in.
 *
 * Clamped: dragging off the month parks on the nearest edge rather than
 * inventing a week that is not rendered.
 */
export function cellIndexAt(
  clientX: number,
  clientY: number,
  bounds: { left: number; top: number; width: number; height: number },
  columns: number,
  rows: number,
): number {
  if (columns <= 0 || rows <= 0 || bounds.width <= 0 || bounds.height <= 0) return 0;
  const col = Math.min(
    Math.max(Math.floor(((clientX - bounds.left) / bounds.width) * columns), 0),
    columns - 1,
  );
  const row = Math.min(
    Math.max(Math.floor(((clientY - bounds.top) / bounds.height) * rows), 0),
    rows - 1,
  );
  return row * columns + col;
}

/** Has the pointer moved far enough to count as a drag rather than a click? */
export function isDrag(dx: number, dy: number, threshold = DRAG_THRESHOLD_PX): boolean {
  return Math.abs(dx) >= threshold || Math.abs(dy) >= threshold;
}

/**
 * Keep an event inside the grid it is drawn on.
 *
 * A move is allowed to end after midnight — a 23:30 meeting really can run into
 * tomorrow — but it may not start before the first day rendered or after the
 * last, because there would be nowhere to draw it.
 */
export function clampToRange(
  outcome: DragOutcome,
  rangeStart: number,
  rangeEnd: number,
): DragOutcome {
  const duration = outcome.end - outcome.start;
  if (outcome.start < rangeStart) return { start: rangeStart, end: rangeStart + duration };
  if (outcome.start >= rangeEnd) {
    const start = rangeEnd - DAY;
    return { start, end: start + duration };
  }
  return outcome;
}
