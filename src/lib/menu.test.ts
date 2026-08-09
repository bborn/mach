import { beforeEach, describe, expect, it, vi } from "vitest";

import { createKeymap, tokenFromEvent } from "./keymap";
import { REPLAY_GUARD_MS, connectMenu, keyEventFromToken } from "./menu";

/**
 * A subscribe stub that hands back the emit function, so tests can fire menu
 * choices. `off()` genuinely detaches, because one of the tests below is about
 * exactly that.
 */
function channel() {
  let emit: ((token: string) => void) | null = null;
  const off = vi.fn(() => {
    emit = null;
  });
  const subscribe = async (handler: (token: string) => void) => {
    emit = handler;
    return off;
  };
  return { subscribe, off, fire: (token: string) => emit?.(token) };
}

/** A stand-in for `window`, so the bridge is testable without a DOM. */
function keySource() {
  const listeners = new Set<(event: KeyboardEvent) => void>();
  return {
    source: {
      addEventListener: (_t: "keydown", fn: (event: KeyboardEvent) => void) => {
        listeners.add(fn);
      },
      removeEventListener: (_t: "keydown", fn: (event: KeyboardEvent) => void) => {
        listeners.delete(fn);
      },
    },
    press(key: string, mods: Partial<Record<"metaKey" | "shiftKey" | "altKey" | "ctrlKey", boolean>> = {}) {
      const event = {
        key,
        metaKey: false,
        ctrlKey: false,
        altKey: false,
        shiftKey: false,
        ...mods,
      } as KeyboardEvent;
      for (const fn of listeners) fn(event);
    },
  };
}

describe("keyEventFromToken", () => {
  it("round-trips through the keymap's own tokeniser", () => {
    // The two must agree exactly, or a menu item fires the wrong binding —
    // this is the whole contract between shell.rs and the registry.
    for (const token of ["c", "?", "[", "meta+k", "meta+1", "meta+shift+z", "escape", "enter"]) {
      expect(tokenFromEvent(keyEventFromToken(token))).toBe(token);
    }
  });

  it("reports no typing target, so menu items work from inside the composer", () => {
    // Picking a menu item is unambiguous intent; the allowInInput gate is there
    // to stop stray letters, not deliberate choices.
    expect(keyEventFromToken("meta+,").target).toBeNull();
  });
});

describe("connectMenu", () => {
  let now = 1_000;

  beforeEach(() => {
    now = 1_000;
  });

  it("runs the binding the token names", () => {
    const keymap = createKeymap("meta");
    const handler = vi.fn();
    keymap.register({ keys: "mod+,", handler });

    const ch = channel();
    connectMenu(keymap, { subscribe: ch.subscribe, now: () => now, keys: null });
    ch.fire("meta+,");

    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("replays a sequence one token at a time", () => {
    const keymap = createKeymap("meta");
    const handler = vi.fn();
    keymap.register({ keys: "g i", handler });

    const ch = channel();
    connectMenu(keymap, { subscribe: ch.subscribe, now: () => now, keys: null });
    ch.fire("g i");

    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("drops a replay that lands right behind the identical keystroke", () => {
    // WKWebView does not reliably yield a key equivalent to the menu, so the
    // webview can handle ⌘Z *and* the menu can emit it. Without this guard the
    // stack would unwind twice for one press.
    const keymap = createKeymap("meta");
    const handler = vi.fn();
    keymap.register({ keys: "mod+z", handler });

    const ch = channel();
    const kb = keySource();
    connectMenu(keymap, { subscribe: ch.subscribe, now: () => now, keys: kb.source });

    kb.press("z", { metaKey: true });
    now += REPLAY_GUARD_MS - 1;
    ch.fire("meta+z");

    expect(handler).not.toHaveBeenCalled();
  });

  it("still replays once the guard window has passed", () => {
    const keymap = createKeymap("meta");
    const handler = vi.fn();
    keymap.register({ keys: "mod+z", handler });

    const ch = channel();
    const kb = keySource();
    connectMenu(keymap, { subscribe: ch.subscribe, now: () => now, keys: kb.source });

    kb.press("z", { metaKey: true });
    now += REPLAY_GUARD_MS + 1;
    ch.fire("meta+z");

    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("does not suppress a different token", () => {
    const keymap = createKeymap("meta");
    const prefs = vi.fn();
    keymap.register({ keys: "mod+,", handler: prefs });

    const ch = channel();
    const kb = keySource();
    connectMenu(keymap, { subscribe: ch.subscribe, now: () => now, keys: kb.source });

    kb.press("z", { metaKey: true });
    ch.fire("meta+,");

    expect(prefs).toHaveBeenCalledTimes(1);
  });

  /*
   * Every token in `shell.rs` is written the way bindings are written, with
   * "mod" — never "meta". The tests above all fired canonical tokens, which is
   * why an unnormalised replay went unnoticed: it only fails on the vocabulary
   * the menu actually speaks.
   */
  it.each(["mod+,", "mod+k", "mod+1", "mod+2"])(
    "replays %s, the vocabulary shell.rs emits",
    async (token) => {
      const keymap = createKeymap("meta");
      const handler = vi.fn();
      keymap.register({ keys: token, handler });

      const ch = channel();
      connectMenu(keymap, {
        subscribe: ch.subscribe,
        now: () => now,
        keys: null,
        mod: "meta",
      });
      await Promise.resolve();

      ch.fire(token);
      expect(handler).toHaveBeenCalledTimes(1);
    },
  );

  it("unsubscribes and stops listening when torn down", async () => {
    const keymap = createKeymap("meta");
    const handler = vi.fn();
    keymap.register({ keys: "mod+,", handler });

    const ch = channel();
    const disconnect = connectMenu(keymap, {
      subscribe: ch.subscribe,
      now: () => now,
      keys: null,
    });

    // The subscription resolves a microtask later; tearing down before it
    // lands must still detach, or a disposed bridge keeps firing bindings.
    await Promise.resolve();
    disconnect();

    expect(ch.off).toHaveBeenCalled();
    ch.fire("meta+,");
    expect(handler).not.toHaveBeenCalled();
  });
});
