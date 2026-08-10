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
    needsReauthorization: false,
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
    expect(progress.errors).toEqual([
      {
        email: "alex@example.com",
        message: "401",
        needsReauthorization: false,
        lastSuccessAt: null,
      },
    ]);
  });

  it("names a dead Keychain entry as its own kind of trouble", () => {
    const progress = syncProgress(
      status([account({ phase: "done", lastSuccessAt: 1 })], {
        lastPassFinishedAt: 2,
        needsReauthorization: ["alex@lumen.example"],
      }),
    );
    // No remote text, because nothing was ever sent: the credential is missing
    // rather than refused.
    expect(progress.errors).toEqual([
      {
        email: "alex@lumen.example",
        message: "Not signed in",
        needsReauthorization: true,
        lastSuccessAt: null,
      },
    ]);
    expect(progress.label).toBe("One account needs signing in again");
    // Same sentence as a sync error, different next action: syncing again never
    // produces a refresh token. The status bar reads this to decide whether its
    // button means "Sync now" or "open Preferences → Accounts".
    expect(progress.reauthorize).toEqual(["alex@lumen.example"]);
  });

  /**
   * The failure the owner hit. He changed one account's Google password, every
   * refresh for it came back `invalid_grant`, and all he was told was "Sync
   * failed" — no account, no reason, nothing to press.
   */
  it("carries a refused credential with the account, the reason and the recovery", () => {
    const progress = syncProgress(
      status(
        [
          account({
            accountId: 1,
            email: "bruno@clickfunnels.example",
            phase: "failed",
            lastError:
              "google refused the stored credential: Google refused the stored credential: " +
              "invalid_grant (Token has been expired or revoked.)",
            needsReauthorization: true,
            lastSuccessAt: 1_700_000_000_000,
          }),
          account({ accountId: 2, email: "bruno@example.com", phase: "done", lastSuccessAt: 2 }),
        ],
        {
          lastPassFinishedAt: 3,
          // The startup Keychain check never sees this one: a revoked token is
          // still *present*, so only the sync loop can discover it.
          needsReauthorization: [],
        },
      ),
    );

    expect(progress.errors).toHaveLength(1);
    const failure = progress.errors[0]!;
    expect(failure.email).toBe("bruno@clickfunnels.example");
    expect(failure.needsReauthorization).toBe(true);
    // Google's words, verbatim. They are the only thing that says which of the
    // several ways a credential dies this one was.
    expect(failure.message).toContain("invalid_grant");
    expect(failure.message).toContain("Token has been expired or revoked.");
    expect(failure.lastSuccessAt).toBe(1_700_000_000_000);
    expect(progress.reauthorize).toEqual(["bruno@clickfunnels.example"]);
    expect(progress.label).toBe("One account needs signing in again");
  });

  it("does not ask for a sign-in when the failure is one another pass could clear", () => {
    const progress = syncProgress(
      status(
        [
          account({
            email: "bruno@example.com",
            phase: "failed",
            lastError: "google rate limited (429): rate limit exceeded",
            needsReauthorization: false,
            lastSuccessAt: 9,
          }),
        ],
        { lastPassFinishedAt: 10 },
      ),
    );
    expect(progress.reauthorize).toEqual([]);
    expect(progress.errors[0]!.needsReauthorization).toBe(false);
    expect(progress.errors[0]!.message).toContain("429");
    expect(progress.label).toBe("Sync failed");
  });

  it("stops asking for a sign-in the moment the address leaves the status", () => {
    // What a completed "Sign in again" looks like from here. Rust clears the
    // address in `mark_reauthorized` and emits the sync status from the same
    // command, so the label and the status bar both go without a restart.
    const progress = syncProgress(
      status([account({ phase: "done", lastSuccessAt: 3 })], {
        lastPassFinishedAt: 4,
        needsReauthorization: [],
      }),
    );
    expect(progress.reauthorize).toEqual([]);
    expect(progress.errors).toEqual([]);
    expect(progress.label).toBe("Up to date");
  });

  it("keeps a sync error and a dead credential apart", () => {
    const progress = syncProgress(
      status(
        [
          account({ email: "alex@lumen.example", phase: "done", lastError: "429" }),
          account({ email: "bruno@example.com", phase: "done", lastSuccessAt: 1 }),
        ],
        { lastPassFinishedAt: 2, needsReauthorization: ["bruno@example.com"] },
      ),
    );
    expect(progress.reauthorize).toEqual(["bruno@example.com"]);
    expect(progress.errors.map((e) => e.email)).toEqual([
      "alex@lumen.example",
      "bruno@example.com",
    ]);
    expect(progress.errors.map((e) => e.needsReauthorization)).toEqual([false, true]);
    // Two accounts are not syncing, and only one of them is retryable, so the
    // rail counts them and the detail says which is which. "Sync failed" alone
    // would under-report by one.
    expect(progress.label).toBe("Sync failed on 2 accounts");
  });

  it("says up to date once a pass has finished with nothing running", () => {
    expect(
      syncProgress(status([account({ phase: "done", lastSuccessAt: 1 })], { lastPassFinishedAt: 2 }))
        .label,
    ).toBe("Up to date");
  });
});
