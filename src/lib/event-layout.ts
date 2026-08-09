/**
 * Side-by-side layout for overlapping calendar events.
 *
 * The thing every other client gets wrong: two events at the same time get
 * stacked, and one of them becomes invisible. Here they get their own column.
 *
 * The algorithm, in three passes:
 *
 *   1. Sort by start ascending, then by end descending (longer events first,
 *      so the one that frames the cluster owns column 0), then by id for a
 *      stable result on ties.
 *   2. Cut the sorted list into *clusters*: a run of events where each event
 *      starts before the running maximum end. Two events in different clusters
 *      can never overlap, so each cluster is laid out independently and a lone
 *      event always gets the full width.
 *   3. Inside a cluster, assign each event the leftmost column whose last
 *      event has already finished (greedy interval-graph colouring). Then
 *      widen each event rightwards across free neighbouring columns so a wide
 *      event is not needlessly narrow.
 *
 * Overlap is strict — `a.start < b.end && b.start < a.end`. Back-to-back
 * events (10:00–11:00 and 11:00–12:00) do not overlap, and a zero-length
 * event only overlaps something it sits strictly inside.
 */

import { clusterPlan } from "./calendar-geometry";

export interface LayoutInput {
  /** Any stable identity: local row ids are numbers, tests use strings. */
  id: string | number;
  start: number;
  end: number;
}

export interface LaidOutEvent<T extends LayoutInput = LayoutInput> {
  event: T;
  /** Zero-based column index within the cluster. */
  column: number;
  /** How many columns this event spans. */
  span: number;
  /** Total columns in this event's cluster — the denominator for width. */
  columns: number;
}

export function overlaps(a: LayoutInput, b: LayoutInput): boolean {
  return a.start < b.end && b.start < a.end;
}

export function layoutEvents<T extends LayoutInput>(events: readonly T[]): LaidOutEvent<T>[] {
  if (events.length === 0) return [];

  const sorted = [...events].sort(
    (a, b) => a.start - b.start || b.end - a.end || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
  );

  const out: LaidOutEvent<T>[] = [];
  let cluster: T[] = [];
  let clusterEnd = -Infinity;

  for (const event of sorted) {
    // A start at or after every end so far cannot overlap anything in the
    // cluster, and neither can anything after it — close the cluster.
    if (cluster.length > 0 && event.start >= clusterEnd) {
      out.push(...layoutCluster(cluster));
      cluster = [];
      clusterEnd = -Infinity;
    }
    cluster.push(event);
    clusterEnd = Math.max(clusterEnd, event.end);
  }
  if (cluster.length > 0) out.push(...layoutCluster(cluster));

  return out;
}

function layoutCluster<T extends LayoutInput>(cluster: readonly T[]): LaidOutEvent<T>[] {
  // Pass 2 — greedy column assignment. `columnEnds[c]` is when column c freed up.
  const columnEnds: number[] = [];
  const assigned = cluster.map((event) => {
    let column = columnEnds.findIndex((end) => end <= event.start);
    if (column === -1) {
      column = columnEnds.length;
      columnEnds.push(event.end);
    } else {
      columnEnds[column] = Math.max(columnEnds[column], event.end);
    }
    return { event, column };
  });

  const columns = columnEnds.length;

  // Pass 3 — widen rightwards while the neighbouring column is clear.
  return assigned.map(({ event, column }, index) => {
    let span = 1;
    for (let c = column + 1; c < columns; c++) {
      const blocked = assigned.some(
        (other, otherIndex) =>
          other.column === c && otherIndex !== index && overlaps(other.event, event),
      );
      if (blocked) break;
      span++;
    }
    return { event, column, span, columns };
  });
}

/**
 * Where a laid-out event actually sits, with a hairline gutter so adjacent
 * blocks read as separate cards rather than one striped rectangle.
 *
 * Two shapes, chosen by `clusterPlan` — see the long note in
 * `calendar-geometry.ts` for why a deep cluster stops dividing:
 *
 *   **divide**   the columns share the width. Percentages, so this is correct
 *                before anything has been measured and at any window size.
 *   **cascade**  every block starts at its own offset and runs to the right
 *                edge of the cluster, drawn over the one before it. Pixels,
 *                because the offset is a legibility floor rather than a
 *                fraction: 18px stays 18px whatever the column is doing.
 *
 * `clusterWidth` is the measured width of the day column's block area. Zero
 * means not measured yet, and divides — which is what the first frame wants.
 */
export function columnGeometry(
  laid: LaidOutEvent,
  clusterWidth = 0,
): { left: string; width: string } {
  const plan = clusterPlan(laid.columns, clusterWidth);

  if (plan.mode === "cascade") {
    const left = Math.round(laid.column * plan.step);
    return { left: `${left + 1}px`, width: `calc(100% - ${left + 2}px)` };
  }

  const unit = 100 / laid.columns;
  return {
    left: `calc(${laid.column * unit}% + 1px)`,
    width: `calc(${laid.span * unit}% - 2px)`,
  };
}
