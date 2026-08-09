/**
 * The macOS menu bar, replayed through the keymap.
 *
 * `src-tauri/src/shell.rs` builds the menu with a *keymap token* as each item's
 * id — `"mod+,"`, `"c"`, `"?"` — and emits that token when the item is chosen.
 * This turns it back into a keystroke and hands it to the one registry in
 * `keymap.ts`.
 *
 * The point is that there is no second implementation. A menu that called into
 * the app some other way would be a parallel copy of the keymap that drifts the
 * first time somebody changes a binding, and the menu would then quietly lie
 * about what it does. Here a menu item *is* its shortcut, so it cannot.
 *
 * # Why the de-duplication exists
 *
 * On macOS the main menu gets first refusal on a key equivalent, but WKWebView
 * does not reliably give it up: the app's own ⌘A selects every conversation
 * even though Select All sits in the Edit menu. So for an item that carries an
 * accelerator, pressing the keys can plausibly fire *both* paths — the webview
 * handles the keydown, and the menu emits the same token a moment later.
 *
 * Which one wins is not worth predicting, and getting it wrong means ⌘Z undoes
 * two things. So the bridge watches real keystrokes and drops a replay that
 * arrives right behind an identical one. Either path alone still works, which
 * is what makes this safe on a platform whose behaviour we cannot pin down.
 */

import { isTauri } from "./ipc";
import {
  detectModKey,
  normalizeToken,
  tokenFromEvent,
  type KeyEventLike,
  type Keymap,
  type ModKey,
} from "./keymap";

/** Must match `shell::MENU_EVENT`. */
export const MENU_EVENT = "mach://menu";

/**
 * How long a real keystroke suppresses an identical replay.
 *
 * Long enough to cover the IPC hop from the menu, short enough that deliberately
 * choosing the same menu item twice in a row still registers both times.
 */
export const REPLAY_GUARD_MS = 400;

/** `baseKey` lowercases and renames; this puts the name back on the way out. */
const KEY_NAMES: Record<string, string> = {
  escape: "Escape",
  enter: "Enter",
  tab: "Tab",
  backspace: "Backspace",
  delete: "Delete",
  space: " ",
  up: "ArrowUp",
  down: "ArrowDown",
  left: "ArrowLeft",
  right: "ArrowRight",
  home: "Home",
  end: "End",
  pageup: "PageUp",
  pagedown: "PageDown",
};

/**
 * Turns one canonical token back into the event the keymap would have seen.
 *
 * `target` is deliberately null rather than whatever currently has focus. A
 * menu choice is unambiguous intent, so it should work while the cursor is in
 * the composer — the `allowInInput` gate exists to stop stray letters becoming
 * commands mid-sentence, and nothing about picking a menu item is stray.
 */
export function keyEventFromToken(token: string): KeyEventLike {
  const parts = token.split("+");
  const key = parts.pop() ?? "";
  const mods = new Set(parts);
  return {
    key: KEY_NAMES[key] ?? key,
    code: /^[0-9]$/.test(key) ? `Digit${key}` : undefined,
    metaKey: mods.has("meta"),
    ctrlKey: mods.has("ctrl"),
    altKey: mods.has("alt"),
    shiftKey: mods.has("shift"),
    target: null,
  };
}

/** Just enough of `window` to watch keystrokes — so this is testable in node. */
export interface KeySource {
  addEventListener(
    type: "keydown",
    listener: (event: KeyboardEvent) => void,
    capture: boolean,
  ): void;
  removeEventListener(
    type: "keydown",
    listener: (event: KeyboardEvent) => void,
    capture: boolean,
  ): void;
}

export interface MenuBridgeOptions {
  /** Injected in tests; defaults to the Tauri event channel. */
  subscribe?: (handler: (token: string) => void) => Promise<() => void>;
  now?: () => number;
  /** Where real keystrokes are observed for de-duplication. Defaults to `window`. */
  keys?: KeySource | null;
  /** What "mod" resolves to. Defaults to the platform's. */
  mod?: ModKey;
}

/**
 * Wires the menu to `keymap` until the returned function is called.
 *
 * Outside Tauri — `bun run dev` in a browser tab, and every test that renders
 * the app — there is no menu, so this does nothing and costs nothing.
 */
export function connectMenu(
  keymap: Keymap,
  options: MenuBridgeOptions = {},
): () => void {
  const now = options.now ?? (() => Date.now());
  const mod = options.mod ?? detectModKey();
  const subscribe = options.subscribe ?? defaultSubscribe;
  if (!options.subscribe && !isTauri()) return () => {};

  const seen = new Map<string, number>();
  const keys =
    options.keys !== undefined
      ? options.keys
      : typeof window === "undefined"
        ? null
        : (window as unknown as KeySource);

  const onKeyDown = (event: KeyboardEvent) => {
    const token = tokenFromEvent(event as unknown as KeyEventLike);
    if (token) seen.set(token, now());
  };
  keys?.addEventListener("keydown", onKeyDown, true);

  let cancelled = false;
  let unsubscribe: (() => void) | null = null;

  void subscribe((token) => {
    const at = seen.get(token);
    if (at !== undefined && now() - at < REPLAY_GUARD_MS) return;

    // A sequence such as "g i" replays a token at a time; the keymap's own
    // sequence timer stitches them back together.
    //
    // Normalised first. `shell.rs` writes ids in the same vocabulary the
    // bindings are written in — "mod+," — and `keyEventFromToken` reads only
    // canonical modifier names, so an unnormalised "mod+," is a bare comma
    // with nothing held and matches no binding. Masked until now because
    // every `mod+` item also carries an accelerator the webview handles
    // first, and the guard above then drops the menu's duplicate; Settings…
    // worked because ⌘, never actually reached this line.
    for (const step of token.trim().split(/\s+/)) {
      keymap.handle(keyEventFromToken(normalizeToken(step, mod)), now());
    }
  }).then((off) => {
    if (cancelled) off();
    else unsubscribe = off;
  });

  return () => {
    cancelled = true;
    keys?.removeEventListener("keydown", onKeyDown, true);
    unsubscribe?.();
  };
}

async function defaultSubscribe(
  handler: (token: string) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  const off = await listen<string>(MENU_EVENT, (event) => handler(event.payload));
  return () => void off();
}
