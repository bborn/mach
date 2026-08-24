/**
 * Whether a sync status update means the calendar grid has to re-read.
 *
 * # The bug this exists for
 *
 * Reported as "why are my latest bruno@influencekit.com cal events not
 * showing", and then, a minute later, "huh now it's showing". Sync was not the
 * problem: calendar passes run on the same poll as mail — every 30 seconds on
 * his machine — and they are incremental against Google's `syncToken`, so an
 * event created on a phone lands in SQLite within one pass.
 *
 * The grid was the problem. `useMach` fetches events on
 * `[windowKey, reloadKey, eventsKey]`, and none of those move when a *sync*
 * writes rows: `windowKey` changes only when you navigate out of the loaded
 * 30-day window, and `eventsKey` is bumped by local calendar writes — dragging
 * an event, creating one, answering an invitation. Mail has a push channel for
 * exactly this (`onThreadsChanged`); the calendar had none, so a new event sat
 * in the database until something else made the view refetch. Crossing a window
 * boundary is what "now it's showing" was.
 *
 * # Why the counter is not monotonic
 *
 * `sync::status` resets `events_written` to zero at the start of every pass, so
 * a pass that writes three events reads `0 → 3` and the next quiet pass reads
 * `3 → 0`. Only the rise means anything. Comparing totals across the whole
 * status would also work until two accounts sync in the same pass, so this is
 * per account: five accounts each writing one event is five rises, and a fall
 * anywhere is a new pass beginning rather than events disappearing.
 */

import type { SyncStatus } from "@/types";

/** What was last seen, per account. Opaque; hand it back on the next call. */
export type EventCounts = ReadonlyMap<number, number>;

export const NO_EVENT_COUNTS: EventCounts = new Map();

export interface EventsArrived {
  /** True when some account wrote events it had not written a moment ago. */
  arrived: boolean;
  /** Pass this back next time. */
  counts: EventCounts;
}

/**
 * Fold one status update into the running counts.
 *
 * Never reports `arrived` on the very first update, whatever it says. The first
 * status a window receives describes a pass that may have finished before the
 * view existed, and the fetch on mount has already read everything it wrote.
 */
export function eventsArrived(
  status: Pick<SyncStatus, "accounts">,
  previous: EventCounts,
  seenBefore = previous.size > 0,
): EventsArrived {
  const counts = new Map<number, number>();
  let arrived = false;
  for (const account of status.accounts) {
    const written = account.eventsWritten;
    counts.set(account.accountId, written);
    if (!seenBefore) continue;
    if (written > (previous.get(account.accountId) ?? 0)) arrived = true;
  }
  return { arrived, counts };
}
