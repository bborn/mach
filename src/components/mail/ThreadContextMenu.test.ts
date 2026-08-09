/**
 * The context menu, tested where it is decided: `buildItems`.
 *
 * Everything this menu promises is a property of that one function — that no
 * item exists without a live binding behind it, that the shortcut printed
 * beside an item is the binding's own, that choosing an item calls the
 * keyboard's handler rather than a copy of it, and that the target is the
 * selection when there is one. None of that needs a DOM to check, and checking
 * it here is what stops the menu quietly growing an entry that does nothing.
 */

import { describe, expect, it, vi } from "vitest";
import type { Thread, ThreadId } from "@/types";
import type { KeyBinding } from "@/lib/keymap";
import { buildItems, type Item } from "./ThreadContextMenu";

function thread(over: Partial<Thread> = {}): Thread {
  return {
    id: 1,
    accountId: 1,
    subject: "Q3 roadmap — final pass",
    snippet: "I moved the Meta scope review ahead of the reporting rewrite.",
    participants: [{ name: "Priya Raghunathan", email: "priya@example.com" }],
    timestamp: Date.UTC(2026, 7, 9, 10, 5),
    unread: false,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
    ...over,
  };
}

/** The bindings `MailMode` and `ComposerDock` register, as the registry has them. */
function registry(over: Partial<Record<string, boolean>> = {}): KeyBinding[] {
  const all: KeyBinding[] = [
    { keys: "r", group: "Write", description: "Reply", handler: () => {} },
    { keys: "a", group: "Write", description: "Reply all", handler: () => {} },
    { keys: "f", group: "Write", description: "Forward", handler: () => {} },
    { keys: "s", group: "Actions", description: "Star", handler: () => {} },
    { keys: "b", group: "Actions", description: "Snooze", handler: () => {} },
    { keys: "e", group: "Actions", description: "Archive", handler: () => {} },
    { keys: "#", group: "Actions", description: "Trash", handler: () => {} },
  ];
  return all.filter((b) => over[b.description!] !== false);
}

function labels(items: Item[]): string[] {
  return items.filter((i) => i.kind === "item").map((i) => (i as { label: string }).label);
}

function byLabel(items: Item[], label: string) {
  const found = items.find((i) => i.kind === "item" && i.label === label);
  return found as Extract<Item, { kind: "item" }> | undefined;
}

const map = (...threads: Thread[]) => new Map<ThreadId, Thread>(threads.map((t) => [t.id, t]));

describe("buildItems", () => {
  it("offers writing only when the target is the conversation on screen", () => {
    const t = thread();
    const onCursor = buildItems(registry(), [t.id], t.id, map(t));
    const offCursor = buildItems(registry(), [t.id], 99, map(t));

    expect(labels(onCursor).slice(0, 3)).toEqual(["Reply", "Reply all", "Forward"]);
    expect(labels(offCursor)).not.toContain("Reply");
    expect(labels(offCursor)).not.toContain("Forward");
  });

  it("acts on the whole selection, and drops what only makes sense for one", () => {
    const a = thread({ id: 1 });
    const b = thread({ id: 2 });
    const items = buildItems(registry(), [a.id, b.id], a.id, map(a, b));

    expect(labels(items)).toEqual(["Star", "Snooze", "Archive", "Trash"]);
    expect(labels(items)).not.toContain("Search by sender");
  });

  it("omits an item whose binding is not live rather than offering a dead one", () => {
    const t = thread();
    const items = buildItems(registry({ Archive: false }), [t.id], t.id, map(t));

    expect(labels(items)).not.toContain("Archive");
    expect(labels(items)).toContain("Trash");
  });

  it("prints the binding's own keys, so the menu cannot drift from the keymap", () => {
    const t = thread();
    const moved: KeyBinding[] = [
      { keys: "mod+shift+e", group: "Actions", description: "Archive", handler: () => {} },
    ];
    const items = buildItems(moved, [t.id], t.id, map(t));

    expect(byLabel(items, "Archive")?.shortcut).toBe("mod+shift+e");
  });

  it("runs the keyboard's handler, not a second implementation", () => {
    const t = thread();
    const archive = vi.fn();
    const items = buildItems(
      [{ keys: "e", group: "Actions", description: "Archive", handler: archive }],
      [t.id],
      t.id,
      map(t),
    );

    byLabel(items, "Archive")?.run();
    expect(archive).toHaveBeenCalledTimes(1);
  });

  it("says Unstar only when every target is already starred", () => {
    const plain = thread({ id: 1 });
    const starred = thread({ id: 2, starred: true });

    expect(labels(buildItems(registry(), [starred.id], 0, map(starred)))).toContain("Unstar");
    expect(labels(buildItems(registry(), [plain.id], 0, map(plain)))).toContain("Star");
    // Mixed follows `starSelected`: a set that is not all starred gets starred.
    expect(
      labels(buildItems(registry(), [plain.id, starred.id], 0, map(plain, starred))),
    ).toContain("Star");
  });

  it("searches by the sender of the one conversation it can name", () => {
    const t = thread();
    const items = buildItems(registry(), [t.id], t.id, map(t));
    expect(labels(items)).toContain("Search by sender");

    // A sender the store has a name for and no address — Gmail does return them.
    const anonymous = thread({ id: 3, participants: [{ name: "Nobody", email: "" }] });
    expect(labels(buildItems(registry(), [anonymous.id], anonymous.id, map(anonymous)))).not.toContain(
      "Search by sender",
    );
  });

  it("never rules off nothing", () => {
    const t = thread();
    for (const items of [
      buildItems(registry(), [t.id], t.id, map(t)),
      buildItems(registry({ Reply: false, "Reply all": false, Forward: false }), [t.id], t.id, map(t)),
      buildItems(registry({ Star: false, Snooze: false }), [t.id], 0, map(t)),
    ]) {
      expect(items[0]?.kind).toBe("item");
      expect(items[items.length - 1]?.kind).toBe("item");
      for (let i = 1; i < items.length; i++) {
        expect(items[i]?.kind === "separator" && items[i - 1]?.kind === "separator").toBe(false);
      }
    }
  });

  /*
   * A selection can outlive the rows in it for a moment — `threads-changed`
   * fires constantly during a sync and the list is refetched underneath. The
   * commands still address those ids, so the items stay; what goes is anything
   * that had to read the row to be true.
   */
  it("keeps the commands for a row it can no longer read, and only those", () => {
    const items = buildItems(registry(), [404], 404, map());
    expect(labels(items)).toEqual(["Star", "Snooze", "Archive", "Trash"]);
    expect(labels(items)).not.toContain("Unstar");
    expect(labels(items)).not.toContain("Search by sender");
  });
});
