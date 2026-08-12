/**
 * ⌘K's route to a forced sync.
 *
 * The keyboard has ⇧⌘R and the menu bar has File ▸ Sync Now, but the palette is
 * the surface somebody reaches for when they do not know the key — so the entry
 * has to be findable by the words a person would actually type, and it has to
 * show that a pass is running rather than looking inert while one is.
 */

import { describe, expect, it } from "vitest";

import { fuzzyScore } from "@/lib/palette/score";
import { commandsWith } from "./CommandPalette";

function syncEntry(syncing = false) {
  const entry = commandsWith(syncing).find((command) => command.id === "sync-now");
  expect(entry, "the palette must offer a forced sync").toBeDefined();
  return entry!;
}

describe("the Sync now command", () => {
  it("is titled for what it does and carries its shortcut", () => {
    const entry = syncEntry();
    expect(entry.title).toBe("Sync now");
    expect(entry.hint).toBe("⇧⌘R");
  });

  it("is found by the words somebody would type for it", () => {
    const entry = syncEntry();
    const haystack = `${entry.title} ${entry.keywords ?? ""}`;
    for (const query of ["sync", "refresh", "fetch", "calendar", "force", "update"]) {
      expect(fuzzyScore(haystack, query), `"${query}" should find it`).toBeGreaterThan(0);
    }
  });

  it("reads as busy while a pass is running", () => {
    expect(syncEntry(true).hint).toBe("Syncing");
    // Still listed, and still titled the same. Removing it would leave the
    // palette answering "no matches" for the thing that is happening.
    expect(syncEntry(true).title).toBe("Sync now");
  });

  it("leaves every other command alone", () => {
    const idle = commandsWith(false);
    const busy = commandsWith(true);
    expect(busy.length).toBe(idle.length);
    expect(busy.filter((c) => c.id !== "sync-now")).toEqual(
      idle.filter((c) => c.id !== "sync-now"),
    );
  });
});
