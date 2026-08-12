/**
 * "Sync now": what it says when it comes back, and what a second press does.
 *
 * Both halves are here because both are the feature. A forced sync that never
 * reports is a spinner you have to interpret, and a forced sync you can fire
 * ten times is a button that looks broken while it works.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ForcedSync, SyncAccountOutcome } from "@/types";
import {
  beginForcedSync,
  endForcedSync,
  forcedSyncInFlight,
  forcedSyncMessage,
  resetForcedSync,
  subscribeForcedSync,
} from "./force-sync";

function account(over: Partial<SyncAccountOutcome> = {}): SyncAccountOutcome {
  return {
    accountId: 1,
    email: "bruno@example.com",
    messagesWritten: 0,
    eventsWritten: 0,
    error: null,
    needsReauthorization: false,
    cancelled: false,
    skipped: false,
    ...over,
  };
}

function outcome(accounts: SyncAccountOutcome[], started = true): ForcedSync {
  return { started, accounts };
}

beforeEach(resetForcedSync);

describe("what a forced sync says when it comes back", () => {
  it("confirms a clean pass, so a press is never silent", () => {
    expect(forcedSyncMessage(outcome([account({ messagesWritten: 3 })]))).toEqual({
      message: "Up to date",
      tone: "info",
    });
  });

  it("names the account and quotes Google when one fails", () => {
    const said = forcedSyncMessage(
      outcome([
        account(),
        account({
          accountId: 2,
          email: "bruno@work.example",
          error: "google rate limited (429): User-rate limit exceeded",
        }),
      ]),
    );
    expect(said.tone).toBe("error");
    // Both halves: which mailbox, and Google's own words. "Sync failed" on its
    // own is the report this feature exists to stop repeating.
    expect(said.message).toContain("bruno@work.example");
    expect(said.message).toContain("User-rate limit exceeded");
  });

  it("asks for a sign-in rather than a retry when the credential is dead", () => {
    const said = forcedSyncMessage(
      outcome([account({ error: "invalid_grant", needsReauthorization: true })]),
    );
    expect(said).toEqual({
      message: "bruno@example.com needs signing in again",
      tone: "error",
    });
    // No number of syncs produces a refresh token, so it must not read as
    // something another press could fix.
    expect(said.message).not.toContain("failed");
  });

  it("counts them when several fail", () => {
    const said = forcedSyncMessage(
      outcome([
        account({ error: "boom" }),
        account({ accountId: 2, email: "b@example.com", error: "bang" }),
      ]),
    );
    expect(said.message).toBe("Sync failed on 2 accounts");
  });

  it("says so when every failure is a dead credential", () => {
    const said = forcedSyncMessage(
      outcome([
        account({ error: "invalid_grant", needsReauthorization: true }),
        account({
          accountId: 2,
          email: "b@example.com",
          error: "invalid_grant",
          needsReauthorization: true,
        }),
      ]),
    );
    expect(said.message).toBe("2 accounts need signing in again");
  });

  it("does not claim to be up to date when the pass was already running", () => {
    const said = forcedSyncMessage(outcome([account({ skipped: true })], false));
    expect(said).toEqual({ message: "Already syncing", tone: "info" });
  });

  it("treats an account already in flight as neither success nor failure", () => {
    // One account synced, one was mid-pass. Nothing failed, so nothing is red.
    const said = forcedSyncMessage(
      outcome([account(), account({ accountId: 2, skipped: true })]),
    );
    expect(said).toEqual({ message: "Up to date", tone: "info" });
  });
});

describe("pressing it twice", () => {
  it("refuses the second request while the first is in flight", () => {
    expect(beginForcedSync("all")).toBe(true);
    expect(beginForcedSync("all")).toBe(false);
    expect(forcedSyncInFlight("all")).toBe(true);

    endForcedSync("all");
    expect(forcedSyncInFlight("all")).toBe(false);
    expect(beginForcedSync("all")).toBe(true);
  });

  it("keeps one account's retry apart from the whole-mailbox press", () => {
    // Retrying the one account Google refused must not be blocked by, or block,
    // a sync of everything — the engine syncs whatever is free and reports the
    // rest as already in flight.
    expect(beginForcedSync(7)).toBe(true);
    expect(beginForcedSync("all")).toBe(true);
    expect(beginForcedSync(7)).toBe(false);

    endForcedSync(7);
    expect(forcedSyncInFlight(7)).toBe(false);
    expect(forcedSyncInFlight()).toBe(true);
  });

  it("tells the window when the state changes, both ways", () => {
    const changed = vi.fn();
    const off = subscribeForcedSync(changed);

    beginForcedSync("all");
    expect(changed).toHaveBeenCalledTimes(1);
    // A refused claim changed nothing, so nothing re-renders.
    beginForcedSync("all");
    expect(changed).toHaveBeenCalledTimes(1);
    endForcedSync("all");
    expect(changed).toHaveBeenCalledTimes(2);

    off();
    beginForcedSync("all");
    expect(changed).toHaveBeenCalledTimes(2);
  });
});
