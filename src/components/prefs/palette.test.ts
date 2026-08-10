/**
 * The keyboard's way into Preferences.
 *
 * Tab in the main window belongs to the mail keymap — it moves between the rail
 * and the list — so the status bar's "One account needs signing in again" is
 * not reachable by tabbing to it. ⌘K is, and the standing rule is that nothing
 * is mouse-only, so the same destination has to be sayable: the entry has to be
 * findable under the words somebody would use for a broken account, and it has
 * to land on the accounts section rather than on whichever one was read last.
 *
 * `window` is stubbed rather than mocked with jsdom: the only thing under test
 * is which event goes out with what on it.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { PaletteContext } from "@/lib/palette/resolver";
import { PREFERENCES_EVENT, preferencesResolver } from "./palette";

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

function find(query: string, title = "Accounts…") {
  return preferencesResolver.resolve(context(query)).find((result) => result.title === title);
}

describe("the accounts entry", () => {
  it("answers to the words a broken account is described with", () => {
    for (const query of [">accounts", ">sign in again", ">authorize", ">reauthorize"]) {
      expect(find(query), query).toBeDefined();
    }
  });

  it("opens Preferences on the accounts section, not on the last one read", () => {
    find(">accounts")!.run();

    expect(dispatched).toHaveLength(1);
    expect(dispatched[0]!.type).toBe(PREFERENCES_EVENT);
    expect(dispatched[0]!.detail).toEqual({ section: "accounts" });
  });

  it("leaves plain ⌘, with no opinion about where to land", () => {
    find(">preferences", "Preferences…")!.run();
    expect(dispatched[0]!.detail).toEqual({ section: undefined });
  });
});
