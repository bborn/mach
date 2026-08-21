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

import { addDays, startOfDay, HOUR, MINUTE } from "./time";

/**
 * 64px/hour makes a 30-minute meeting 32px, which is a thing you can hit.
 *
 * It was 48, on the arithmetic that a 30-minute meeting is then exactly 24px —
 * one 15px line plus padding, the smallest honest size that still reads. It
 * does read; it is just small under a pointer, and a half-hour meeting is the
 * most common length there is. A block's height is its duration, so the only
 * lever that makes half-hours taller is the hour itself.
 *
 * The cost is the day: a 785px grid showed 16.4 hours and now shows 12.3.
 * Every threshold below scales with this rather than restating 48, so the
 * ladder in §5 still lines up if it moves again.
 */
export const HOUR_HEIGHT = 64;

/**
 * A floor for events shorter than a quarter hour, and no higher than that.
 *
 * At 64px/hour a 15-minute event is exactly this tall, so the floor binds only
 * below it. It was briefly 26, to make quarter-hours easier to hit, and that
 * was wrong for a reason worth writing down: a floor taller than the slot the
 * event occupies makes *consecutive* short events overlap, and the later one
 * paints over the earlier. Two back-to-back fifteen-minute meetings covered
 * each other by ten pixels.
 *
 * So height stays honest and the hit skirt does the reaching — a 15px block is
 * a 31px target. That is the division of labour: the grid says how long
 * something is, and the pointer is answered somewhere slightly larger.
 */
export const MIN_BLOCK_HEIGHT = 15;

/** 1px of air between vertically adjacent blocks, so they read as two cards. */
export const BLOCK_GAP = 1;

/**
 * The shortest a block's *pointer target* is allowed to be.
 *
 * The rule above is about honesty of scale, and it stays. But the drawing and
 * the hit area are two different questions, and only the first one owes the
 * grid its geometry. A 30-minute meeting is 23px of colour and a 15-minute one
 * is 11px, and at 11px the thing you are aiming at is thinner than the pointer's
 * own hotspot. Missing it does not do nothing, either — the press lands on empty
 * grid, which is drag-to-create, so a 2px miss answers "open my standup" with a
 * new untitled event and a text field.
 *
 * So a short block carries a transparent skirt above and below its painted body,
 * bringing the target to 32px: about a 40-minute event, which is the smallest
 * block nobody complains about. Nothing moves and nothing is drawn — the block
 * keeps its true top, its true bottom and its true height.
 *
 * The skirt is only ever allowed to claim grid that is otherwise empty. Every
 * painted block sits at `Z_EVENT` or above and every skirt at `Z_EVENT_HIT`
 * below it, so a skirt reaching down into its neighbour passes *under* that
 * neighbour's body: the 09:00 block can never take a click meant for the 09:30
 * one. Where two skirts overlap each other — a gap shorter than the two of them
 * — the later block's wins, being painted after it.
 */
export const MIN_HIT_HEIGHT = 32;

/**
 * And the furthest it may reach to get there.
 *
 * Without a ceiling the rule bites back: an 11px sliver would claim 11px above
 * and below itself, and the strip of grid just under a quarter-hour meeting is
 * where you press to drag-create the thing that follows it. Answering that with
 * the meeting's own modal is the same defect as the one being fixed, pointing
 * the other way.
 *
 * Eight is what a 15-minute block can take before that trade turns: 27px of
 * target, two and a half times what it had, and ten minutes of grid rather than
 * fifteen. A 30-minute block wants five and never meets this at all.
 *
 * So the promise the grid makes is bounded: nothing answers the pointer more
 * than 8px from where it is drawn.
 */
export const MAX_HIT_OVERHANG = 8;

/** How far a block's hit area overhangs its painted body, top and bottom. */
export function hitSkirt(height: number): number {
  const wanted = Math.ceil((MIN_HIT_HEIGHT - height) / 2);
  return clamp(wanted, 0, MAX_HIT_OVERHANG);
}

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

/* -------------------------------------------------------------------------- */
/* Overlapping clusters                                                        */
/* -------------------------------------------------------------------------- */

/**
 * Three concurrent events used to render as `Intervi staf…`, `OKR…` and
 * `Wareh cost…`.
 *
 * A week column at 1440px is 168px wide, 155px of it usable. Divided three ways
 * that is 51px per block, which after the block's own inset leaves about four
 * characters. Five ways it is 31px. Even division is right for two events and
 * wrong for five, and the numbers below are where it changes.
 *
 * The two references answer it differently:
 *
 *   - **Google Calendar** keeps the first words legible by dropping everything
 *     that is not the title once a block gets narrow. The time goes first,
 *     because a block's position in the grid already says when it is.
 *   - **Fantastical** stops dividing past a certain depth and *cascades*: each
 *     event is offset and drawn over the one before, so one of them is readable
 *     in full rather than all of them being unreadable.
 *
 * Mach takes both, at different thresholds. Under `NARROW_BLOCK_WIDTH` a block
 * spends its whole self on the title (Google). Under `READABLE_COLUMN_WIDTH`,
 * and only from three events up, the cluster cascades (Fantastical).
 *
 * A cascaded block runs from its own offset to the right edge of the cluster,
 * so its title is laid out at full width and then covered. Selecting it lifts it
 * above its neighbours (`Z_EVENT_SELECTED` over `Z_EVENT`) and the whole title
 * is already there, with no reflow and no geometry change; arrowing through a
 * five-deep cluster reads every one of them in turn. Every event stays on the
 * grid throughout, keeping its colour, its true top and bottom, and its own
 * click target.
 */

/** Below this an evenly-divided column cannot hold a title. */
export const READABLE_COLUMN_WIDTH = 76;

/**
 * Below this a block shows its title and nothing else.
 *
 * 88px less the block's 4px insets is 80px of text, which is about thirteen
 * characters at 12px. Above that a line spent on the time is a line the title
 * did not need; below it, it is the line the title needed most.
 *
 * It sits under the 97px a full-width column has at a 1040px window, so
 * narrowing the window leaves the times alone and only crowding removes them.
 */
export const NARROW_BLOCK_WIDTH = 88;

/** What the cascade aims to leave the block on top. */
export const CASCADE_TOP_WIDTH = 76;

/**
 * The narrowest strip a cascaded block may reveal.
 *
 * 18px is under two characters, so the strip at this size says *another event
 * is here, on this calendar, running this long* and offers a target for the
 * pointer and the arrow keys. Below it the cluster stops adding columns and
 * spends the last one on a `+N` chip.
 */
export const CASCADE_STEP_MIN = 18;

/** A block narrower than this is a colour rather than a label (§2). */
export const MIN_COLUMN_WIDTH = 40;

export type ClusterMode = "divide" | "cascade";

export interface ClusterPlan {
  mode: ClusterMode;
  /** Pixels between one cascaded block's left edge and the next. 0 when dividing. */
  step: number;
}

/**
 * Divide or cascade, and by how much.
 *
 * Two events always divide. Side-by-side halves is the idiom every calendar
 * uses, and 48px each at a narrow window still shows a word apiece: worse than a
 * cascade for one of the two, better for the other.
 */
export function clusterPlan(columns: number, clusterWidth: number): ClusterPlan {
  if (columns <= 2 || clusterWidth <= 0) return { mode: "divide", step: 0 };
  const share = clusterWidth / columns;
  if (share >= READABLE_COLUMN_WIDTH) return { mode: "divide", step: 0 };
  // `clamp` prefers its minimum when the range inverts, which is what a cluster
  // narrower than the top block's target needs: every step is the floor, and the
  // block on top takes whatever is left rather than the width it asked for.
  const step = clamp(
    (clusterWidth - CASCADE_TOP_WIDTH) / (columns - 1),
    CASCADE_STEP_MIN,
    share,
  );
  return { mode: "cascade", step };
}

/**
 * Named layers instead of Google's magic 5 / 507.
 *
 * The selected block sits above its neighbours because its halo is drawn
 * *outside* it: at `Z_EVENT` the block starting at the next quarter hour paints
 * over the bottom band of the mark, and the cursor comes out looking chewed.
 * Below the hover layer, though — an expanded block is deliberately covering
 * its cluster and should keep doing so.
 */
/**
 * A block's hit skirt, under every painted block including its own.
 *
 * It has to be above 0 rather than absent: the working-hours wash is an
 * unpositioned sibling, and a `z-index: auto` positioned element would still
 * paint over it, but a stated layer keeps the two orderings from depending on
 * which one happens to render first.
 */
export const Z_EVENT_HIT = 1;
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
  /**
   * The secondary line — the time, and the location under it.
   *
   * It used to be `fontPx` as well, so a block's title and its time were the
   * same size and the block read as two equal lines. At 12 over 11 the title
   * reads as the label and the time as the footnote, and the block costs no
   * more height, because 11px sits on the same 15px line.
   */
  timeFontPx: 11;
  /** The text line's height. Constant, which is why a sliver can overflow. */
  lineHeightPx: 15;
  /** Title and time on one comma-joined line. */
  inlineTime: boolean;
  /**
   * Whether the time is drawn at all.
   *
   * False on a narrow block, which is Google's rule. The time costs a whole
   * line in a stacked block and about 30px on an inline one, and the grid is
   * already a picture of when the event is. It stays in the tooltip and in the
   * modal.
   */
  showTime: boolean;
  /** Title may run to a second line. Never true below 48px. */
  wrapTitle: boolean;
  /** How many lines the title may take. Three only when the time has gone. */
  titleLines: 1 | 2 | 3;
  showLocation: boolean;
  /** The text is allowed to spill outside the block's bounds. */
  overflow: boolean;
}

export function blockPlan(
  height: number,
  options: { hasLocation?: boolean; width?: number } = {},
): BlockPlan {
  const tier = blockTier(height);
  // Width is optional so a caller that only cares about the height ladder — the
  // tests, and anything reasoning about duration alone — gets the roomy plan.
  const narrow = options.width !== undefined && options.width < NARROW_BLOCK_WIDTH;
  const titleLines = tier === "full" ? (narrow ? 3 : 2) : 1;
  return {
    tier,
    fontPx: tier === "sliver" ? 11 : 12,
    timeFontPx: 11,
    lineHeightPx: 15,
    inlineTime: tier === "oneLine" || tier === "sliver",
    showTime: !narrow,
    wrapTitle: titleLines > 1,
    titleLines,
    showLocation: tier === "full" && !narrow && options.hasLocation === true,
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
  // Fractions of the hour rather than the pixel counts §5 was written with, so
  // the ladder follows `HOUR_HEIGHT` instead of having to be restated whenever
  // it moves: an hour, three quarters, a half.
  if (height >= HOUR_HEIGHT - BLOCK_GAP) return "full";
  if (height >= HOUR_HEIGHT * 0.708 - BLOCK_GAP) return "twoLine";
  if (height >= HOUR_HEIGHT * 0.5 - BLOCK_GAP) return "oneLine";
  return "sliver";
}

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

/**
 * The local midnights of the first inclusive day and the exclusive end day an
 * event occupies on a date grid.
 *
 * All-day events are pinned to UTC midnight of each calendar date, so we read
 * the UTC date and re-express it as local midnight — otherwise a trip that
 * Google shows as Tue–Thu starts Monday evening here, west of Greenwich.
 *
 * Timed events use local start-of-day. An end exactly on midnight does not
 * occupy that day (the timed equivalent of Google's exclusive all-day end).
 */
export function eventGridRange(event: {
  start: number;
  end: number;
  allDay: boolean;
}): { start: number; end: number } {
  if (event.allDay) {
    const start = localMidnightOfUtcDate(event.start);
    let end = localMidnightOfUtcDate(Math.max(event.end, event.start + 1));
    if (end <= start) end = addDays(start, 1).getTime();
    return { start, end };
  }
  const start = startOfDay(event.start).getTime();
  const last = startOfDay(Math.max(event.end, event.start + 1) - 1).getTime();
  return { start, end: addDays(last, 1).getTime() };
}

/** Local midnight of the UTC calendar date `ts` falls on. */
function localMidnightOfUtcDate(ts: number): number {
  const d = new Date(ts);
  return new Date(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate()).getTime();
}

/**
 * Where `range` sits on a run of local day columns, or `null` if it misses
 * them all. `dayStarts` is each column's local midnight, in order.
 */
export function clipToDays(
  range: { start: number; end: number },
  dayStarts: readonly number[],
): { startIndex: number; span: number } | null {
  let startIndex = -1;
  let endIndex = -1;
  for (let i = 0; i < dayStarts.length; i++) {
    const dayStart = dayStarts[i];
    const dayEnd =
      i + 1 < dayStarts.length ? dayStarts[i + 1] : addDays(dayStart, 1).getTime();
    if (range.start < dayEnd && range.end > dayStart) {
      if (startIndex === -1) startIndex = i;
      endIndex = i + 1;
    }
  }
  if (startIndex === -1) return null;
  return { startIndex, span: endIndex - startIndex };
}

/**
 * How many columns a cluster may show before the last one becomes a `+N` chip.
 *
 * This used to be `floor(width / 40)`, how many 40px columns fit side by side.
 * That let three 51px columns through, which is where the unreadable cluster
 * came from, and it refused a fourth event that a cascade has room for. Now
 * that a deep cluster cascades, the question it asks is how many 18px strips
 * fit beside a block wide enough to read: seven at 1440px, against three.
 *
 * Two is never capped. Half of a very narrow column is still an event you can
 * see and click.
 */
export function visibleColumns(columns: number, columnWidth: number): number {
  if (columns <= 2 || columnWidth <= 0) return Math.max(columns, 1);
  const fits = 1 + Math.floor((columnWidth - MIN_COLUMN_WIDTH) / CASCADE_STEP_MIN);
  return clamp(fits, 1, columns);
}
