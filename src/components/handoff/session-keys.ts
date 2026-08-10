/**
 * The session pane's keyboard contract, as data.
 *
 * A terminal wants every keystroke and the app has global keys, so the split
 * has to be stated rather than discovered. It is the split macOS already uses,
 * and the one Terminal.app uses: **⌘ belongs to the application, and everything
 * else belongs to the process.**
 *
 * | while the terminal has focus | who gets it |
 * |---|---|
 * | any key without ⌘ — letters, ⎋, ⇥, arrows, `/`, `?`, ⌃C, `g` | the process |
 * | any ⌘ chord — ⌘K, ⌘1, ⌘,, ⌘Z, ⌘V | the app, exactly as if the pane were not here |
 *
 * That is one binding, not a list: a `*` at {@link TERMINAL_KEY_PRIORITY} which
 * *declines* every event carrying ⌘ and claims every event that does not. The
 * declined ones fall through the registry to whatever would have answered them
 * anyway — ⌘K reaches the palette, and a ⌘V nothing binds reaches the webview,
 * which is how paste works. The claimed ones are taken from every app binding
 * and then handed straight back to the page unmodified — `passthrough` in
 * `lib/keymap.ts` — because the emulator's own listener on its own textarea is
 * the thing that knows how to turn ⌥⇧← into bytes, and it sits downstream of
 * the registry.
 *
 * Nothing here is live while the terminal does not have focus. The app is then
 * exactly the app it was before this feature existed.
 *
 * # Escape, and getting out
 *
 * Escape goes to the process, always, while the terminal has focus — a TUI
 * wants it, `claude` interrupts on it, and a key that sometimes closes the
 * window you are typing into is the worst kind of key. Once the process has
 * exited the pane is a receipt rather than a session, and Escape dismisses it.
 * So Escape has one meaning at any moment, and which one is decided by whether
 * there is still a process to hear it.
 *
 * The way out is therefore a ⌘ chord, because those always reach the app:
 *
 * | ⇧⌘T | focus the terminal, or hand focus back where it came from |
 * | ⇧⌘W | end the session and close the pane, from anywhere |
 * | ⌥⌘↑ ⌥⌘↓ | taller, shorter — while the terminal has focus |
 *
 * Both directions of ⇧⌘T are one keystroke on purpose: the pane is one place,
 * and "put me in it" and "get me out of it" are the same thought.
 *
 * # Why this is a function rather than a `useKeyBindings` call in the component
 *
 * Because it is the part that has to be tested, and rendering the pane means
 * rendering an emulator that wants a canvas and a real layout engine.
 * `session-keys.test.ts` registers these against the same registry the app uses,
 * next to the palette's real ⌘K and a mail binding, and presses keys at it.
 */

import type { KeyBinding } from "@/lib/keymap";

/**
 * Above every app binding, including the palette's ⌘K at 200.
 *
 * It outranks everything rather than sitting at some negotiated level, because
 * the claim is total: while the terminal has focus, *no* app binding answers a
 * key the process could want. Which keys the app keeps is then a property of
 * one `if` in one handler instead of a list that the next global shortcut would
 * silently join.
 */
export const TERMINAL_KEY_PRIORITY = 900;

/** Above the palette (200) and the agent drawer's identical resize pair. */
export const SESSION_CHORD_PRIORITY = 260;

/** Below the palette's Escape, above the mail list's. */
export const SESSION_DISMISS_PRIORITY = 140;

export interface SessionKeyState {
  /** There is a session and its process is still running. */
  live: () => boolean;
  /** The emulator holds focus. */
  focused: () => boolean;
  /** There is a pane at all — running or finished. */
  present: () => boolean;
  /** The process has exited and the pane is a receipt. */
  finished: () => boolean;
  paletteOpen: () => boolean;
}

export interface SessionKeyActions {
  toggleFocus: () => void;
  end: () => void;
  taller: () => void;
  shorter: () => void;
}

export function sessionBindings(
  state: SessionKeyState,
  actions: SessionKeyActions,
): KeyBinding[] {
  return [
    {
      // The contract, as one binding. Read the file's doc before changing it.
      keys: "*",
      priority: TERMINAL_KEY_PRIORITY,
      allowInInput: true,
      passthrough: true,
      when: () => state.live() && state.focused(),
      handler: (event) => {
        // ⌘ is the application's. Declining lets the registry carry on down
        // the list, so ⌘K reaches the palette and a chord nothing binds reaches
        // the webview — which is how ⌘C and ⌘V stay the clipboard.
        if (event.metaKey) return false;
        return true;
      },
    },
    {
      keys: "shift+mod+t",
      group: "Session",
      description: "Terminal session",
      allowInInput: true,
      priority: SESSION_CHORD_PRIORITY,
      when: () => state.live(),
      handler: () => actions.toggleFocus(),
    },
    {
      keys: "shift+mod+w",
      group: "Session",
      description: "End the session",
      allowInInput: true,
      priority: SESSION_CHORD_PRIORITY,
      when: () => state.present(),
      handler: () => actions.end(),
    },
    {
      keys: "escape",
      allowInInput: true,
      priority: SESSION_DISMISS_PRIORITY,
      when: () => state.finished() && !state.paletteOpen(),
      handler: () => actions.end(),
    },
    {
      keys: "mod+alt+up",
      group: "Session",
      description: "Taller",
      allowInInput: true,
      // Gated on the terminal having focus, so the pane you are typing into is
      // the one that resizes; the agent drawer keeps the pair otherwise. The
      // priority is above its, so `conflicts()` sees a decision, not a tie.
      priority: SESSION_CHORD_PRIORITY,
      when: () => state.live() && state.focused(),
      handler: () => actions.taller(),
    },
    {
      keys: "mod+alt+down",
      group: "Session",
      description: "Shorter",
      allowInInput: true,
      priority: SESSION_CHORD_PRIORITY,
      when: () => state.live() && state.focused(),
      handler: () => actions.shorter(),
    },
  ];
}
