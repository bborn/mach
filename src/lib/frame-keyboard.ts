/**
 * The keyboard, while the reader's focus is inside a message body.
 *
 * # What is broken, and why nothing here can fix it
 *
 * A message body is a sandboxed iframe with no `allow-scripts`. The instant
 * anything in it has focus — one click to follow a link, to scroll, or to
 * select a word — its keydowns fire in *that* document, and every shortcut in
 * the app dies at once: not `e` alone, but archive, star, snooze and the way
 * back to the list.
 *
 * `MessageFrame` has answered this since the first report by attaching a
 * capture-phase `keydown` listener to the frame's document from the parent and
 * forwarding what it hears into the keymap. That works in Blink and is why the
 * fix was believed to have landed. It never runs in WebKit: `JSEventListener`
 * refuses to invoke a listener whose target's context has scripting disabled,
 * and a frame sandboxed without `allow-scripts` has scripting disabled by
 * definition. Attaching succeeds; firing never happens; nothing says so. See
 * `message-body.ts`'s `FRAME_SANDBOX`, which records the same measurement for
 * the *click* listener four lines above it.
 *
 * So the key is read below the engine instead — `src-tauri/src/frame_keyboard.rs`
 * publishes it off the `NSEvent` — and this is the half that decides what it
 * meant. Rust knows nothing about focus, views or bindings, and must not.
 *
 * # Moving the focus out was the other option
 *
 * It would have cost text selection, which is not on offer. A drag-select *is*
 * focus in the frame: blur it and WebKit drops the selection, and ⌘C — which
 * the frame handles natively and correctly today — has nothing to copy. Every
 * "hand focus back once the interaction settles" variant has to guess when a
 * drag began, and the parent cannot see a pointer inside the frame at all.
 */

import { frameKeepsKey } from "@/lib/message-body";
import { isTauri } from "@/lib/ipc";
import type { KeyEventLike } from "@/lib/keymap";

/** The event Rust emits. Mirrors `frame_keyboard::FRAME_KEY_EVENT`. */
export const FRAME_KEY_EVENT = "frame-key";

/** One key, as Rust read it off the `NSEvent`. Mirrors `frame_keyboard::FrameKey`. */
export interface FrameKeyPayload {
  key: string;
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

/**
 * Whether the keyboard is inside a message body right now.
 *
 * `document.activeElement` is an `<iframe>` exactly when focus is inside one,
 * and the message body is the only iframe in the app — `MessageFrame` renders
 * the sole one. A more specific test would have to reach for an attribute on an
 * element another module owns, to answer a question this already answers.
 */
export function keyboardInFrame(root: Document = document): boolean {
  return root.activeElement?.tagName === "IFRAME";
}

/**
 * What to do with a key Rust published.
 *
 * Pure, and the whole of the judgement, so the interesting half is testable
 * without a webview or an event monitor.
 *
 *  * **Not in a frame — ignore it.** The monitor is gated on the same fact, but
 *    the flag crosses a process boundary and focus can move while a key is in
 *    flight. The DOM delivered that key to the app in the ordinary way, and
 *    acting on it here as well would archive two conversations for one `e`.
 *  * **A key the frame keeps — ignore it.** Arrows, PageUp/Down, Home/End and
 *    space scroll the message being read; ⌘A and ⌘C are select-all and copy of
 *    its text, and ⌘A is the dangerous one because the app binds it to "select
 *    every conversation". `frameKeepsKey` is the same rule the dead listener
 *    used, kept in one place.
 *
 * Everything else — the letters, Escape, the account keys — has no meaning
 * inside a read-only document and belongs to the app.
 */
export function frameKeyIsOurs(payload: FrameKeyPayload, root: Document = document): boolean {
  if (!keyboardInFrame(root)) return false;
  return !frameKeepsKey(payload);
}

/**
 * The payload as the keymap reads events.
 *
 * `preventDefault` and `stopPropagation` are no-ops on purpose: there is no DOM
 * event here to cancel. The `NSEvent` was handed back to AppKit before this
 * message was sent — the monitor publishes and never swallows — so the key also
 * reaches the frame, where a letter in a scripting-disabled, non-editable
 * document does nothing at all.
 *
 * `target` reads as "not typing", which is the truth: the sandbox has no
 * `allow-forms` and the sanitizer drops every input, so there is nothing in
 * there a letter could be typed into.
 */
export function asKeyEvent(payload: FrameKeyPayload): KeyEventLike {
  return {
    key: payload.key,
    code: payload.code || undefined,
    metaKey: payload.metaKey,
    ctrlKey: payload.ctrlKey,
    altKey: payload.altKey,
    shiftKey: payload.shiftKey,
    target: { tagName: "IFRAME", isContentEditable: false },
    preventDefault: () => {},
    stopPropagation: () => {},
  };
}

/**
 * Tell Rust where the keyboard is, so the monitor knows whether to say anything.
 *
 * This is the flag that keeps the whole mechanism quiet. With it down the
 * `NSEvent` handler returns on one atomic read and there is no IPC at all: a
 * person typing a message pays nothing, and only a person whose focus is in a
 * message body — whose keys are the ones currently being lost — pays anything.
 *
 * Reported on focus changes, which happen when somebody clicks into or out of a
 * message, rather than per keystroke.
 */
async function reportFrameFocus(inside: boolean): Promise<void> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("set_frame_focus", { inside });
  } catch (error) {
    // Said out loud rather than swallowed — a monitor nobody is talking to
    // means the shortcuts are dead in a message body again, which is the whole
    // bug. Not a status line, though: the reader has no action to take, and an
    // unhandled rejection would be filed as a boot failure by the watchdog in
    // `index.html`.
    console.error("frame-keyboard: could not report focus to the key monitor", error);
  }
}

/**
 * Wire the frame's keyboard back to the keymap.
 *
 * Two halves, and they are not symmetrical. `focusin`/`focusout` are listened
 * for on the app's own document, which is scripted and therefore does fire
 * them — the focus *change* is visible from out here even though the keys
 * inside the frame are not. The keys themselves arrive from Rust.
 *
 * Returns a function that stops, and lowers the flag on the way out so a torn
 * down listener cannot leave Rust talking to nobody.
 */
export function connectFrameKeys(env: {
  keymap: { handle: (event: KeyEventLike) => boolean };
}): () => void {
  // There is no monitor outside the app, and no bug either: in a browser tab the
  // frame's own keydown listener fires, which is the whole reason this was
  // believed fixed for so long. See `MessageFrame`.
  if (!isTauri()) return () => {};

  let inside = keyboardInFrame();
  void reportFrameFocus(inside);

  // One listener for both directions: `focusout` is followed by `focusin` when
  // focus lands somewhere else, and by nothing at all when it lands nowhere —
  // so the answer is read from `document.activeElement` rather than from which
  // event arrived. A microtask, because during `focusout` the active element is
  // still the old one.
  const onFocusChange = () => {
    queueMicrotask(() => {
      const next = keyboardInFrame();
      if (next === inside) return;
      inside = next;
      void reportFrameFocus(next);
    });
  };
  document.addEventListener("focusin", onFocusChange, true);
  document.addEventListener("focusout", onFocusChange, true);

  let stopped = false;
  let off: (() => void) | null = null;
  void (async () => {
    const { listen } = await import("@tauri-apps/api/event");
    const unlisten = await listen<FrameKeyPayload>(FRAME_KEY_EVENT, (event) => {
      if (!frameKeyIsOurs(event.payload)) return;
      env.keymap.handle(asKeyEvent(event.payload));
    });
    if (stopped) void unlisten();
    else off = unlisten;
  })();

  return () => {
    stopped = true;
    document.removeEventListener("focusin", onFocusChange, true);
    document.removeEventListener("focusout", onFocusChange, true);
    void reportFrameFocus(false);
    off?.();
  };
}
