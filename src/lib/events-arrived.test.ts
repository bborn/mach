/**
 * The calendar's push channel.
 *
 * Filed as "why are my latest bruno@influencekit.com cal events not showing",
 * and a minute later "huh now it's showing". The events were in SQLite the
 * whole time — calendar passes run on the same 30-second poll as mail and are
 * incremental against Google's `syncToken`. What was missing was anything
 * telling the grid to re-read: it refetches on the 30-day window and on local
 * writes, and a sync is neither. "Now it's showing" was a window boundary.
 */

import { describe, expect, it } from "vitest";
import type { AccountSyncStatus, SyncStatus } from "@/types";
import { eventsArrived, NO_EVENT_COUNTS } from "./events-arrived";

function account(accountId: number, eventsWritten: number): AccountSyncStatus {
  return {
    accountId,
    email: `a${accountId}@example.com`,
    phase: "idle",
    backfillTotal: 0,
    backfillDone: 0,
    messagesWritten: 0,
    eventsWritten,
    lastError: null,
    needsReauthorization: false,
    lastSuccessAt: null,
    updatedAt: 0,
  } as AccountSyncStatus;
}

function status(...accounts: AccountSyncStatus[]): Pick<SyncStatus, "accounts"> {
  return { accounts };
}

describe("eventsArrived", () => {
  /*
   * The first update describes a pass that may have finished before the window
   * existed, and the fetch on mount already read whatever it wrote. Refetching
   * on it would be a wasted query on every launch.
   */
  it("says nothing on the first update, however many it reports", () => {
    const first = eventsArrived(status(account(1, 12)), NO_EVENT_COUNTS);
    expect(first.arrived).toBe(false);
    expect(first.counts.get(1)).toBe(12);
  });

  it("reports a rise, which is a pass that brought something", () => {
    const { counts } = eventsArrived(status(account(1, 0)), NO_EVENT_COUNTS);
    expect(eventsArrived(status(account(1, 3)), counts).arrived).toBe(true);
  });

  it("says nothing when a pass brought nothing", () => {
    const { counts } = eventsArrived(status(account(1, 4)), NO_EVENT_COUNTS);
    expect(eventsArrived(status(account(1, 4)), counts).arrived).toBe(false);
  });

  /*
   * `sync::status` zeroes the counter at the start of every pass, so a quiet
   * pass after a busy one reads 3 → 0. A fall is a new pass beginning, not
   * events disappearing, and refetching on it would double every refresh.
   */
  it("says nothing on the reset at the start of the next pass", () => {
    const { counts } = eventsArrived(status(account(1, 3)), NO_EVENT_COUNTS);
    const quiet = eventsArrived(status(account(1, 0)), counts);
    expect(quiet.arrived).toBe(false);
    // And the fall is remembered, so the *next* rise off zero still counts.
    expect(eventsArrived(status(account(1, 1)), quiet.counts).arrived).toBe(true);
  });

  /*
   * Per account, not per total. Two accounts syncing in one pass — one writing
   * an event, one having just been reset — sum to no change at all, and the
   * grid would have gone on showing yesterday.
   */
  it("sees one account's events under another account's reset", () => {
    const { counts } = eventsArrived(status(account(1, 0), account(2, 5)), NO_EVENT_COUNTS);
    expect(eventsArrived(status(account(1, 5), account(2, 0)), counts).arrived).toBe(true);
  });

  it("handles an account appearing, which is one being added", () => {
    const { counts } = eventsArrived(status(account(1, 2)), NO_EVENT_COUNTS);
    expect(eventsArrived(status(account(1, 2), account(2, 9)), counts).arrived).toBe(true);
  });
});
