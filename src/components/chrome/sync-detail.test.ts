/**
 * The keyboard's way into the sync detail.
 *
 * The standing rule is that nothing here is mouse-only, and the status bar is
 * the hardest place to honour it: Tab in the main window belongs to the mail
 * keymap, so a button in the footer is reachable with a pointer and with
 * nothing else. `prefs/palette.ts` solved exactly this for "Accounts…" by
 * putting the route in ⌘K, and this follows it.
 *
 * `window` is stubbed rather than mocked with jsdom, as in that file: the only
 * thing under test is which event goes out.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { PaletteContext } from "@/lib/palette/resolver";
import { SYNC_DETAIL_EVENT, syncDetailResolver } from "./sync-detail";

let dispatched: CustomEvent[] = [];
const NO_WINDOW = Symbol("no window");
let saved: unknown = NO_WINDOW;

beforeEach(() => {
  dispatched = [];
  saved = "window" in globalThis ? globalThis.window : NO_WINDOW;
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: {
      dispatchEvent(event: CustomEvent) {
        dispatched.push(event);
        return true;
      },
    },
  });
});

afterEach(() => {
  if (saved === NO_WINDOW) delete (globalThis as { window?: unknown }).window;
  else (globalThis as { window?: unknown }).window = saved;
});

function context(query: string): PaletteContext {
  return {
    query,
    threads: [],
    events: [],
    people: [],
    mailboxes: [],
    commands: [],
    actions: {
      openThread: () => {},
      openEvent: () => {},
      openMailbox: () => {},
      runCommand: () => {},
      composeTo: () => {},
    },
  };
}

function find(query: string) {
  return syncDetailResolver.resolve(context(query)).find((r) => r.title === "Sync status…");
}

describe("the sync status entry", () => {
  it("answers to the words a stalled mailbox is described with", () => {
    // Not ">sign in again" — that phrase belongs to the Accounts entry, which
    // is where the sign-in actually happens. See `prefs/palette.test.ts`.
    for (const query of [">sync status", ">sync failed", ">retry", ">error", ">stuck"]) {
      expect(find(query), query).toBeDefined();
    }
  });

  it("opens the detail rather than navigating anywhere", () => {
    find(">sync status")!.run();

    expect(dispatched).toHaveLength(1);
    expect(dispatched[0]!.type).toBe(SYNC_DETAIL_EVENT);
  });

  it("stays out of the way of an ordinary mail search", () => {
    // These rows sit above the mail being searched for, so a scattered fuzzy
    // hit does not earn the space.
    expect(syncDetailResolver.resolve(context(""))).toHaveLength(0);
    expect(syncDetailResolver.resolve(context("invoice"))).toHaveLength(0);
  });
});
