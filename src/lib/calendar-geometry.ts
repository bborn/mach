/**
 * The measured geometry of the week grid.
 *
 * Every number here was read off Google Calendar's live DOM and is recorded in
 * `docs/calendar-ux-brief.md` §1 and §5. They are not taste; they are the
 * reference implementation's actual values, and the whole point of the brief is
 * that we copy them rather than invent our own.
 *
 * Nothing in this file knows about React. The grid renders it, the tests pin it.
 */

import { HOUR, MINUTE } from "./time";

/** 48px/hour makes a 30-minute meeting exactly 24px — one 15px line + padding. */
export const HOUR_HEIGHT = 48;

/**
 * The whole trick from §1: a 15-minute event is an 11px block whose 15px text
 * line deliberately overflows it. 17px (what this used to be) renders a
 * 15-minute event 40% taller than its duration, so it reads the same size as a
 * 21-minute one and the grid lies about its own geometry.
 */
export const MIN_BLOCK_HEIGHT = 11;

/** 1px of air between vertically adjacent blocks, so they read as two cards. */
export const BLOCK_GAP = 1;

/** Time gutter: 56px wide, label right-aligned with an 8px inset. */
export const TIME_GUTTER = 56;
export const TIME_GUTTER_INSET = 8;

/** Google leaves this strip clear so there is always somewhere to drag-create. */
export const BLOCK_RIGHT_GUTTER = 13;

export const BLOCK_RADIUS = 6;

/** All-day chips: 22px tall on a 24px pitch, at most three rows before "+N". */
export const ALL_DAY_CHIP_HEIGHT = 22;
export const ALL_DAY_ROW_PITCH = 24;
export const ALL_DAY_MAX_ROWS = 3;

/** Below this a column is hopeless whatever you put in it (§2). */
export const MIN_COLUMN_WIDTH = 40;

/**
 * Named layers instead of Google's magic 5 / 507.
 *
 * The selected block sits above its neighbours because its halo is drawn
 * *outside* it: at `Z_EVENT` the block starting at the next quarter hour paints
 * over the bottom band of the mark, and the cursor comes out looking chewed.
 * Below the hover layer, though — an expanded block is deliberately covering
 * its cluster and should keep doing so.
 */
export const Z_EVENT = 5;
export const Z_EVENT_SELECTED = 8;
export const Z_EVENT_HOVER = 10;
export const Z_NOW = 20;

/** Drag-create snaps to the quarter hour; a bare click makes 30 minutes. */
export const SNAP_MINUTES = 15;
export const DEFAULT_EVENT_MINUTES = 30;

/** Hover-to-expand: cluster size that earns it, and the delay before it fires. */
export const EXPAND_CLUSTER_MIN = 3;
export const EXPAND_DELAY_MS = 150;

export function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), Math.max(min, max));
}

/** Pixels from the top of a day column for a timestamp inside that day. */
export function offsetForTime(ts: number, dayStart: number): number {
  return ((ts - dayStart) / HOUR) * HOUR_HEIGHT;
}

/** The inverse: the timestamp a y offset inside a day column points at. */
export function timeForOffset(offset: number, dayStart: number): number {
  return dayStart + (offset / HOUR_HEIGHT) * HOUR;
}

/** Round a timestamp to the nearest snap step (15 minutes by default). */
export function snapTime(ts: number, minutes: number = SNAP_MINUTES): number {
  const step = minutes * MINUTE;
  return Math.round(ts / step) * step;
}

/** Round down instead — what a drag's leading edge wants. */
export function snapTimeDown(ts: number, minutes: number = SNAP_MINUTES): number {
  const step = minutes * MINUTE;
  return Math.floor(ts / step) * step;
}

/**
 * Block height for a duration. `max(durationPx - 1, 11)`: the 1px is the gap
 * between stacked blocks, the 11px floor is the short-event rule above.
 */
export function blockHeight(durationMs: number): number {
  const exact = (durationMs / HOUR) * HOUR_HEIGHT;
  return Math.max(exact - BLOCK_GAP, MIN_BLOCK_HEIGHT);
}

/**
 * Where the grid opens.
 *
 * A fixed 07:00 is right at 09:00 and useless at 16:00. Anchoring now a quarter
 * of the way down keeps the hours you are about to act on in the largest part
 * of the viewport, while still showing what just happened. The lower clamp
 * stops an early morning opening on a wall of empty night.
 */
export function nowScrollTop(
  now: number,
  dayStart: number,
  viewportHeight: number,
  /**
   * The earliest hour worth showing, in hours since midnight.
   *
   * 6.5 was the measured default — half an hour of air above a seven o'clock
   * start. It is a parameter so the working-hours preference can move it: on a
   * day that starts at ten, opening the grid on half past six wastes a third of
   * the viewport on hours nothing is ever in.
   */
  floorHours = 6.5,
): number {
  const nowOffset = offsetForTime(now, dayStart);
  const floor = floorHours * HOUR_HEIGHT;
  const ceiling = 24 * HOUR_HEIGHT - viewportHeight;
  return clamp(nowOffset - 0.25 * viewportHeight, floor, ceiling);
}

/* -------------------------------------------------------------------------- */
/* Progressive degradation (§5)                                                */
/* -------------------------------------------------------------------------- */

/**
 * Each tier drops exactly one thing. In order:
 *
 *   full    ≥48px  ≥60min  title (may wrap) · time on its own line · location
 *   twoLine 34–47  45min   title (1 line, ellipsised) · time on its own line
 *   oneLine 24–33  30min   "Standup, 11am" — one line, comma joined
 *   sliver  11–23  ≤15min  the same line at 11px, overflowing the block
 */
export type BlockTier = "full" | "twoLine" | "oneLine" | "sliver";

export interface BlockPlan {
  tier: BlockTier;
  /** 12px normal, 11px compressed. Two sizes in the grid, and only two. */
  fontPx: 11 | 12;
  /** The text line's height. Constant, which is why a sliver can overflow. */
  lineHeightPx: 15;
  /** Title and time on one comma-joined line. */
  inlineTime: boolean;
  /** Title may run to a second line. Never true below 48px. */
  wrapTitle: boolean;
  showLocation: boolean;
  /** The text is allowed to spill outside the block's bounds. */
  overflow: boolean;
}

export function blockPlan(
  height: number,
  options: { hasLocation?: boolean } = {},
): BlockPlan {
  const tier = blockTier(height);
  return {
    tier,
    fontPx: tier === "sliver" ? 11 : 12,
    lineHeightPx: 15,
    inlineTime: tier === "oneLine" || tier === "sliver",
    wrapTitle: tier === "full",
    showLocation: tier === "full" && options.hasLocation === true,
    overflow: tier === "sliver",
  };
}

/**
 * The brief's thresholds are *duration* heights — a 60-minute event is 48px, a
 * 30-minute one is 24px. What gets rendered is one pixel shorter than that,
 * because `blockHeight` has already taken the stacking gap out. Comparing a
 * rendered height against the brief's raw numbers therefore demotes every event
 * by exactly one tier: the most common meeting length there is, 30 minutes,
 * came out as an 11px sliver, and an hour-long event lost its location line.
 *
 * So the thresholds carry the gap too, and the ladder lines up with §5 again.
 */
export function blockTier(height: number): BlockTier {
  if (height >= 48 - BLOCK_GAP) return "full";
  if (height >= 34 - BLOCK_GAP) return "twoLine";
  if (height >= 24 - BLOCK_GAP) return "oneLine";
  return "sliver";
}

/**
 * How many columns a cluster may actually show before slivers stop being
 * readable, and how many events that hides. Below 40px per column, cap the
 * columns and spend the last one on a `+N` chip rather than rendering a 12px
 * sliver of a title.
 */
/**
 * Row packing for the all-day strip.
 *
 * All-day events span days, so they pack into rows the way timed events pack
 * into columns: longest first, each bar taking the topmost row where nothing
 * already overlaps its day range. A five-day trip is then one bar across five
 * columns rather than the same title repeated five times.
 */
export interface RowItem {
  /** Index of the first visible day the bar covers. */
  startIndex: number;
  /** How many day columns it covers, at least 1. */
  span: number;
}

export function packRows<T extends RowItem>(items: readonly T[]): (T & { row: number })[] {
  const ordered = [...items].sort((a, b) => a.startIndex - b.startIndex || b.span - a.span);
  const rows: { start: number; end: number }[][] = [];
  return ordered.map((item) => {
    const range = { start: item.startIndex, end: item.startIndex + Math.max(item.span, 1) };
    let row = rows.findIndex((occupied) =>
      occupied.every((other) => other.end <= range.start || other.start >= range.end),
    );
    if (row === -1) {
      row = rows.length;
      rows.push([]);
    }
    rows[row].push(range);
    return { ...item, row };
  });
}

export function visibleColumns(columns: number, columnWidth: number): number {
  if (columns <= 1 || columnWidth <= 0) return Math.max(columns, 1);
  const fits = Math.floor(columnWidth / MIN_COLUMN_WIDTH);
  if (fits >= columns) return columns;
  return Math.max(1, fits);
}
