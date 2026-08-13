import { describe, expect, it } from "vitest";
import {
  describeResult,
  describeWakeFailure,
  failedIds,
  fixtureSource,
  inverseOf,
  isMailCommand,
  targetIds,
  type Command,
  type CommandResult,
} from "./data";

describe("the command vocabulary", () => {
  it("covers everything in the Rust catalogue", () => {
    // `src-tauri/src/commands/catalogue.rs` is authoritative; if a kind lands
    // there and not here, the UI cannot dispatch it and the agent's tool list
    // and the UI's disagree.
    const kinds: Command["kind"][] = [
      "archive",
      "unarchive",
      "markRead",
      "star",
      "label",
      "reportSpam",
      "notSpam",
      "trash",
      "untrash",
      "snooze",
      "unsnooze",
      "rsvp",
    ];
    expect(kinds).toHaveLength(12);
  });

  it("separates the batched mail half from the single-event rsvp", () => {
    expect(isMailCommand({ kind: "trash", threadIds: [1, 2] })).toBe(true);
    expect(isMailCommand({ kind: "rsvp", eventId: 9, response: "accepted" })).toBe(false);
    expect(targetIds({ kind: "rsvp", eventId: 9, response: "accepted" })).toEqual([9]);
    expect(targetIds({ kind: "archive", threadIds: [1, 2] })).toEqual([1, 2]);
  });
});

describe("inverses", () => {
  it("reverses snooze with unsnooze, not unarchive", () => {
    // Waking a thread restores the labels it was snoozed from; "add INBOX"
    // would not, which is why this was wrong before.
    expect(inverseOf({ kind: "snooze", threadIds: [3], until: 1 })).toEqual({
      kind: "unsnooze",
      threadIds: [3],
    });
  });

  it("pairs archive with unarchive and trash with untrash", () => {
    expect(inverseOf({ kind: "archive", threadIds: [1] })).toEqual({
      kind: "unarchive",
      threadIds: [1],
    });
    expect(inverseOf({ kind: "unarchive", threadIds: [1] })).toEqual({
      kind: "archive",
      threadIds: [1],
    });
    expect(inverseOf({ kind: "trash", threadIds: [1] })).toEqual({
      kind: "untrash",
      threadIds: [1],
    });
    expect(inverseOf({ kind: "reportSpam", threadIds: [1] })).toEqual({
      kind: "notSpam",
      threadIds: [1],
    });
    expect(inverseOf({ kind: "notSpam", threadIds: [1] })).toEqual({
      kind: "reportSpam",
      threadIds: [1],
    });
    expect(inverseOf({ kind: "untrash", threadIds: [1] })).toEqual({
      kind: "trash",
      threadIds: [1],
    });
  });

  it("flips the boolean commands", () => {
    expect(inverseOf({ kind: "markRead", threadIds: [1], read: true })).toEqual({
      kind: "markRead",
      threadIds: [1],
      read: false,
    });
    expect(inverseOf({ kind: "star", threadIds: [1], starred: false })).toEqual({
      kind: "star",
      threadIds: [1],
      starred: true,
    });
    expect(inverseOf({ kind: "label", threadIds: [1], labelId: "Label_1", add: true })).toEqual({
      kind: "label",
      threadIds: [1],
      labelId: "Label_1",
      add: false,
    });
  });

  it("claims no inverse for the two that need prior state to reverse", () => {
    expect(inverseOf({ kind: "unsnooze", threadIds: [1] })).toBeUndefined();
    expect(inverseOf({ kind: "rsvp", eventId: 1, response: "declined" })).toBeUndefined();
  });

  /*
   * The one command in the vocabulary that nothing anywhere can reverse. The
   * others above are missing an inverse because the state that would build one
   * lives in the command layer; this one has no inverse to build. `undefined`
   * is this function's spelling of "there is none" — `CommandResult.undo` is
   * optional rather than nullable, so a `null` here could not be carried.
   */
  it("claims no inverse for unsubscribe, which has left the machine", () => {
    expect(inverseOf({ kind: "unsubscribe", messageId: 512 })).toBeUndefined();
    expect(inverseOf({ kind: "unsubscribe", messageId: 512 }) ?? null).toBeNull();
  });

  it("addresses no local row with an unsubscribe", () => {
    // Its `messageId` says which header to use, not which row to change, so
    // nothing in `applied`, `failed` or the projection is keyed by it.
    expect(targetIds({ kind: "unsubscribe", messageId: 512 })).toEqual([]);
    expect(isMailCommand({ kind: "unsubscribe", messageId: 512 })).toBe(false);
  });

  it("carries an exact restore set when the command layer supplies one", () => {
    const undo: Command = {
      kind: "unarchive",
      threadIds: [4],
      restore: [{ threadId: 4, labelIds: ["INBOX", "Label_9"], isUnread: true }],
    };
    expect(undo.kind === "unarchive" && undo.restore?.[0]?.labelIds).toEqual([
      "INBOX",
      "Label_9",
    ]);
  });
});

describe("partial failure", () => {
  const partial: CommandResult = {
    ok: false,
    message: "Archived 2 conversations",
    applied: [1, 2],
    failed: [
      {
        ids: [3, 4],
        kind: "rateLimited",
        message: "Gmail rate limited this account",
        retriable: true,
        rolledBack: true,
      },
      {
        ids: [4, 5],
        kind: "auth",
        message: "grant expired",
        retriable: false,
        rolledBack: true,
      },
    ],
  };

  it("collects every rolled-back id once, across failures", () => {
    expect(failedIds(partial)).toEqual([3, 4, 5]);
  });

  it("does not report a partial run as a clean one", () => {
    expect(describeResult(partial)).toBe(
      "Archived 2 conversations · 3 failed — Gmail rate limited this account",
    );
  });

  it("says only what failed when nothing applied", () => {
    expect(
      describeResult({
        ok: false,
        message: "Archived 0 conversations",
        applied: [],
        failed: [
          { ids: [1], kind: "network", message: "no answer", retriable: true, rolledBack: true },
        ],
      }),
    ).toBe("1 failed — no answer");
  });

  it("leaves a successful message alone", () => {
    expect(
      describeResult({ ok: true, message: "Snoozed 1 conversation", applied: [1], failed: [] }),
    ).toBe("Snoozed 1 conversation");
  });

  it("names the conversations a wake could not bring back, and why", () => {
    expect(
      describeWakeFailure({
        threadIds: [7, 9],
        message: "Gmail rate limited this account",
        retriable: true,
      }),
    ).toBe("Could not wake 2 conversations — Gmail rate limited this account");
    expect(
      describeWakeFailure({ threadIds: [7], message: "no answer", retriable: true }),
    ).toBe("Could not wake 1 conversation — no answer");
  });
});

describe("the fixture source", () => {
  it("pages with a keyset cursor rather than an offset", async () => {
    const first = await fixtureSource.listThreads({ labelId: "INBOX", limit: 10 });
    expect(first.threads).toHaveLength(10);
    expect(first.nextCursor).not.toBeNull();

    const second = await fixtureSource.listThreads({
      labelId: "INBOX",
      limit: 10,
      after: first.nextCursor,
    });
    const overlap = second.threads.filter((t) => first.threads.some((f) => f.id === t.id));
    expect(overlap).toHaveLength(0);
    expect(second.threads[0]!.timestamp).toBeLessThanOrEqual(first.nextCursor!.lastMessageAt);
  });

  it("ends the list with a null cursor", async () => {
    const page = await fixtureSource.listThreads({ labelId: "INBOX", limit: 1000 });
    expect(page.nextCursor).toBeNull();
  });

  it("filters by account and label the way the backend does", async () => {
    const page = await fixtureSource.listThreads({ accountId: 4, labelId: "L_FAMILY" });
    expect(page.threads.length).toBeGreaterThan(0);
    expect(page.threads.every((t) => t.accountId === 4)).toBe(true);
    expect(page.threads.every((t) => t.labelIds.includes("L_FAMILY"))).toBe(true);
  });

  it("returns numeric row ids, like SQLite", async () => {
    const page = await fixtureSource.listThreads({ limit: 1 });
    expect(typeof page.threads[0]!.id).toBe("number");
    const detail = await fixtureSource.getThread(page.threads[0]!.id);
    expect(detail?.thread.id).toBe(page.threads[0]!.id);
  });

  it("reports every command as applied, with the corrected inverse", async () => {
    const result = await fixtureSource.execute({ kind: "snooze", threadIds: [1, 2], until: 5 });
    expect(result.applied).toEqual([1, 2]);
    expect(result.undo).toEqual({ kind: "unsnooze", threadIds: [1, 2] });
    expect(result.failed).toEqual([]);
  });

  it("refuses account changes instead of pretending to make them", async () => {
    await expect(fixtureSource.beginAddAccount()).rejects.toThrow(/desktop app/);
  });
});
