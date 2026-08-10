import { describe, expect, it } from "vitest";
import type { Thread } from "@/types";
import { reconcile } from "./useThreadStream";

function thread(id: number, over: Partial<Thread> = {}): Thread {
  return {
    id,
    accountId: 1,
    subject: `Conversation ${id}`,
    snippet: "snippet",
    participants: [{ name: "Someone", email: "someone@example.test" }],
    timestamp: 2_000 - id,
    unread: false,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
    ...over,
  };
}

/**
 * What a refetch costs when nothing changed.
 *
 * `threads-changed` fires on every command and on every batch a sync pass
 * writes. Each one refetches up to three hundred rows, and each row came back
 * as a freshly deserialised object equal to the one it replaced — so the list
 * re-rendered in full, twice a second, for the length of a backfill, to paint
 * exactly what was already on screen.
 */
describe("a refetched list", () => {
  it("is the very same array when nothing moved", () => {
    // Same reference means React bails out of the render entirely.
    const previous = [thread(1), thread(2)];
    const incoming = [thread(1), thread(2)];
    expect(reconcile(previous, incoming)).toBe(previous);
  });

  it("keeps the old object for every row that did not change", () => {
    const previous = [thread(1), thread(2)];
    const incoming = [thread(1), thread(2, { starred: true })];
    const next = reconcile(previous, incoming);
    expect(next).not.toBe(previous);
    expect(next[0]).toBe(previous[0]);
    expect(next[1]).toBe(incoming[1]);
  });

  it("takes the new row when a field the list paints changed", () => {
    const previous = [thread(1, { unread: true })];
    const next = reconcile(previous, [thread(1, { unread: false })]);
    expect(next[0]!.unread).toBe(false);
  });

  it("notices a label change, which the draft mark and the projection read", () => {
    const previous = [thread(1)];
    const next = reconcile(previous, [thread(1, { labelIds: ["INBOX", "DRAFT"] })]);
    expect(next[0]).not.toBe(previous[0]);
  });

  it("follows a row that moved up the list", () => {
    const previous = [thread(1), thread(2)];
    const next = reconcile(previous, [thread(2), thread(1)]);
    expect(next).not.toBe(previous);
    expect(next.map((t) => t.id)).toEqual([2, 1]);
    // Still the same objects, in a different order — a reorder is not a reason
    // to rebuild rows.
    expect(next[0]).toBe(previous[1]);
  });

  it("drops a row that left the mailbox", () => {
    const previous = [thread(1), thread(2)];
    const next = reconcile(previous, [thread(1)]);
    expect(next.map((t) => t.id)).toEqual([1]);
  });

  it("takes the incoming list whole when there was nothing before", () => {
    const incoming = [thread(1)];
    expect(reconcile([], incoming)).toBe(incoming);
  });
});
