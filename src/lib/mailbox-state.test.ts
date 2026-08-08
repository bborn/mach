import { describe, expect, it } from "vitest";
import type { AccountSyncStatus, SyncStatus } from "@/types";
import { mailboxState, syncProgress, type MailboxInput } from "./mailbox-state";

function account(overrides: Partial<AccountSyncStatus> = {}): AccountSyncStatus {
  return {
    accountId: 1,
    email: "alex@example.com",
    phase: "idle",
    backfillTotal: 0,
    backfillDone: 0,
    messagesWritten: 0,
    eventsWritten: 0,
    lastError: null,
    lastSuccessAt: null,
    updatedAt: 0,
    ...overrides,
  };
}

function status(accounts: AccountSyncStatus[], overrides: Partial<SyncStatus> = {}): SyncStatus {
  return {
    running: true,
    accounts,
    lastPassStartedAt: 1,
    lastPassFinishedAt: null,
    configured: true,
    configurationError: null,
    needsReauthorization: [],
    ...overrides,
  };
}

const base: MailboxInput = {
  booted: true,
  error: null,
  accountCount: 1,
  threadCount: 0,
  sync: status([account({ lastSuccessAt: 10, phase: "done" })], { lastPassFinishedAt: 20 }),
};

describe("the four empty states", () => {
  it("is loading only until the first round trip settles", () => {
    expect(mailboxState({ ...base, booted: false })).toEqual({ kind: "loading" });
  });

  it("reports missing credentials even before boot finishes", () => {
    // Waiting cannot produce an OAuth client, so this outranks the spinner.
    const state = mailboxState({
      ...base,
      booted: false,
      error: { kind: "notConfigured", message: "missing config: MACH_GOOGLE_CLIENT_ID" },
    });
    expect(state).toEqual({
      kind: "notConfigured",
      message: "missing config: MACH_GOOGLE_CLIENT_ID",
    });
  });

  it("believes the backend's own configured flag over anything else", () => {
    // `sync_status` answers this directly; there is no need to read error prose.
    expect(
      mailboxState({
        ...base,
        threadCount: 20,
        sync: status([], {
          configured: false,
          configurationError: "MACH_GOOGLE_CLIENT_ID is not set",
        }),
      }),
    ).toEqual({ kind: "notConfigured", message: "MACH_GOOGLE_CLIENT_ID is not set" });
  });

  it("offers to add an account when there are none", () => {
    expect(mailboxState({ ...base, accountCount: 0, sync: status([]) })).toEqual({
      kind: "noAccounts",
    });
  });

  it("shows sync progress while the first backfill is filling an empty list", () => {
    const state = mailboxState({
      ...base,
      sync: status([account({ phase: "backfill", backfillDone: 120, backfillTotal: 61_204 })]),
    });
    expect(state.kind).toBe("syncing");
    expect(state.kind === "syncing" && state.progress.fraction).toBeCloseTo(120 / 61_204);
  });

  it("counts an account that has never finished a pass as syncing, not as empty", () => {
    expect(mailboxState({ ...base, sync: status([account({ phase: "idle" })]) }).kind).toBe(
      "syncing",
    );
  });

  it("says empty only when the store is synced and this mailbox really is", () => {
    expect(mailboxState(base)).toEqual({ kind: "empty" });
  });

  it("shows the list as soon as there is anything in it, sync or no sync", () => {
    const state = mailboxState({
      ...base,
      threadCount: 12,
      sync: status([account({ phase: "backfill", backfillTotal: 100 })]),
    });
    expect(state).toEqual({ kind: "ready" });
  });
});

describe("errors", () => {
  it("surfaces a sync failure rather than claiming an empty inbox", () => {
    const state = mailboxState({
      ...base,
      sync: status([account({ phase: "failed", lastError: "401 from Gmail" })], {
        lastPassFinishedAt: 5,
      }),
    });
    expect(state).toEqual({ kind: "error", message: "401 from Gmail" });
  });

  it("does not blame a narrowed query for a sync failure", () => {
    const state = mailboxState({
      ...base,
      filtered: true,
      sync: status([account({ phase: "failed", lastError: "401 from Gmail" })], {
        lastPassFinishedAt: 5,
      }),
    });
    expect(state).toEqual({ kind: "empty" });
  });

  it("reports a backend failure from the boot round trip", () => {
    expect(
      mailboxState({ ...base, error: { kind: "backend", message: "database is locked" } }),
    ).toEqual({ kind: "error", message: "database is locked" });
  });
});

describe("sync progress", () => {
  it("is inactive and unlabelled with nothing to report", () => {
    expect(syncProgress(null)).toMatchObject({ active: false, fraction: null, label: "Idle" });
  });

  it("aggregates the backfill across accounts", () => {
    const progress = syncProgress(
      status([
        account({ accountId: 1, phase: "backfill", backfillDone: 100, backfillTotal: 400 }),
        account({ accountId: 2, phase: "backfill", backfillDone: 100, backfillTotal: 600 }),
      ]),
    );
    expect(progress).toMatchObject({ active: true, done: 200, total: 1000 });
    expect(progress.fraction).toBeCloseTo(0.2);
    expect(progress.label).toBe("Backfilling 2 accounts — 200 of 1,000");
  });

  it("names the account and the count for a single backfill", () => {
    expect(
      syncProgress(
        status([account({ phase: "backfill", backfillDone: 12_480, backfillTotal: 61_204 })]),
      ).label,
    ).toBe("Backfilling alex@example.com — 12,480 of 61,204");
  });

  it("has no fraction before the backfill has enumerated its queue", () => {
    const progress = syncProgress(status([account({ phase: "labels" })]));
    expect(progress.fraction).toBeNull();
    expect(progress.label).toBe("Reading labels alex@example.com");
  });

  it("lets the loudest phase name the pass when accounts are at different stages", () => {
    expect(
      syncProgress(
        status([
          account({ accountId: 1, phase: "calendar" }),
          account({ accountId: 2, phase: "backfill", backfillDone: 1, backfillTotal: 2 }),
        ]),
      ).label,
    ).toBe("Backfilling 2 accounts — 1 of 2");
  });

  it("carries per-account errors without hiding the healthy accounts", () => {
    const progress = syncProgress(
      status([
        account({ accountId: 1, phase: "failed", lastError: "401" }),
        account({ accountId: 2, phase: "backfill", backfillTotal: 10, backfillDone: 5 }),
      ]),
    );
    expect(progress.active).toBe(true);
    expect(progress.errors).toEqual([{ email: "alex@example.com", message: "401" }]);
  });

  it("names a dead Keychain entry as its own kind of trouble", () => {
    const progress = syncProgress(
      status([account({ phase: "done", lastSuccessAt: 1 })], {
        lastPassFinishedAt: 2,
        needsReauthorization: ["alex@lumen.example"],
      }),
    );
    expect(progress.errors).toEqual([
      { email: "alex@lumen.example", message: "alex@lumen.example needs signing in again" },
    ]);
    expect(progress.label).toBe("One account needs signing in again");
  });

  it("says up to date once a pass has finished with nothing running", () => {
    expect(
      syncProgress(status([account({ phase: "done", lastSuccessAt: 1 })], { lastPassFinishedAt: 2 }))
        .label,
    ).toBe("Up to date");
  });
});
