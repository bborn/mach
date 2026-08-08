/**
 * Merge-on-render for the same meeting arriving on several accounts (brief §7).
 *
 * With five Google accounts, an invitation that reaches two of them is stored
 * twice and rendered twice, side by side, halving the width of both. No
 * mainstream client fixes this natively — Google's own support threads end at
 * "turn off the duplicate calendar", and the best-known workaround is a browser
 * extension whose entire purpose is collapsing these. So: collapse them here.
 *
 * Identity, in order of confidence:
 *
 *   1. `iCalUID` + start. Google returns `iCalUID` on every event and it is
 *      stable across copies of the same meeting, which makes this exact.
 *   2. Fallback: start, end, all-day flag and normalised title all match.
 *
 * **The `iCalUID` half is dormant.** `CalendarEvent` does not carry the field
 * yet — the SQLite `events` table has no column for it and `list_events` does
 * not return one — so today every merge runs on the fallback. `iCalUidOf` reads
 * it structurally, so the moment the seam grows the field this becomes exact
 * with no change here.
 */

import type { AccountId, CalendarEvent, CalendarId } from "@/types";

export interface MergedEvent {
  /** The copy that is rendered, and whose calendar supplies the colour. */
  event: CalendarEvent;
  /** Every copy, the rendered one first. Length 1 when nothing merged. */
  copies: CalendarEvent[];
  calendarIds: CalendarId[];
  accountIds: AccountId[];
  /** True when this block stands for more than one stored event. */
  merged: boolean;
}

/** Google's cross-copy identifier, if the data seam has grown it yet. */
export function iCalUidOf(event: CalendarEvent): string | undefined {
  const value = (event as CalendarEvent & { iCalUID?: unknown }).iCalUID;
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

/** Case, punctuation-spacing and stray whitespace are not identity. */
export function normaliseTitle(title: string): string {
  return title
    .toLowerCase()
    .replace(/[‘’“”]/g, "'")
    .replace(/\s+/g, " ")
    .trim();
}

export function mergeKey(event: CalendarEvent): string {
  const uid = iCalUidOf(event);
  if (uid) return `uid:${uid}@${event.start}`;
  return `t:${event.start}:${event.end}:${event.allDay ? 1 : 0}:${normaliseTitle(event.title)}`;
}

/** Answered invitations win the colour: that is the account you replied from. */
const RESPONSE_RANK: Record<string, number> = {
  accepted: 0,
  tentative: 1,
  declined: 2,
};

function responseRank(event: CalendarEvent): number {
  if (!event.rsvp) return 3; // you own it outright — still better than unanswered
  return RESPONSE_RANK[event.rsvp] ?? 4; // needsAction
}

export interface MergeOptions {
  /** Sidebar order, used to break ties. Calendars not listed sort last. */
  order?: readonly CalendarId[];
  /** Off restores one block per stored event. Defaults on (§7). */
  enabled?: boolean;
}

export function mergeDuplicates(
  events: readonly CalendarEvent[],
  options: MergeOptions = {},
): MergedEvent[] {
  const enabled = options.enabled !== false;
  if (!enabled) {
    return events.map((event) => ({
      event,
      copies: [event],
      calendarIds: [event.calendarId],
      accountIds: [event.accountId],
      merged: false,
    }));
  }

  const order = new Map((options.order ?? []).map((id, index) => [id, index]));
  const rank = (event: CalendarEvent) => order.get(event.calendarId) ?? Number.MAX_SAFE_INTEGER;

  const groups = new Map<string, CalendarEvent[]>();
  for (const event of events) {
    const key = mergeKey(event);
    const bucket = groups.get(key);
    if (bucket) bucket.push(event);
    else groups.set(key, [event]);
  }

  const out: MergedEvent[] = [];
  for (const bucket of groups.values()) {
    const copies = [...bucket].sort(
      (a, b) => responseRank(a) - responseRank(b) || rank(a) - rank(b) || a.id - b.id,
    );
    const primary = copies[0];
    out.push({
      event: primary,
      copies,
      calendarIds: unique(copies.map((c) => c.calendarId)),
      accountIds: unique(copies.map((c) => c.accountId)),
      merged: copies.length > 1,
    });
  }

  // Groups are keyed, so the output order is arbitrary; the grid sorts by
  // start anyway, but a stable order keeps React keys and tests predictable.
  return out.sort((a, b) => a.event.start - b.event.start || a.event.id - b.event.id);
}

function unique<T>(values: T[]): T[] {
  return [...new Set(values)];
}
