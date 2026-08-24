// @vitest-environment jsdom

/**
 * Which keys the app takes back while focus is inside a message body — from
 * the path that works in the *app*.
 *
 * `frame-keys.test.ts` beside this one covers `frameKeepsKey`, the rule both
 * paths share. This covers the second path, which exists because the first one
 * does not run here: WebKit will not invoke a listener whose target document
 * has scripting disabled, so the keydown forwarding in `MessageFrame` attaches
 * and never fires. That is why the fix in `8ee75b3` measured clean in Blink and
 * the bug came back a third time, as "after I click a link in an email, the E
 * archive keycut doesn't register".
 *
 * The key is read off the `NSEvent` instead — `src-tauri/src/frame_keyboard.rs`
 * — and this is the half that decides what it meant.
 */

import { beforeEach, describe, expect, it } from "vitest";
import {
  asKeyEvent,
  frameKeyIsOurs,
  keyboardInFrame,
  type FrameKeyPayload,
} from "./frame-keyboard";

function key(over: Partial<FrameKeyPayload> = {}): FrameKeyPayload {
  return {
    key: "e",
    code: "",
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...over,
  };
}

/** Focus in a message body is `document.activeElement` being the iframe. */
function focusFrame(): HTMLIFrameElement {
  const frame = document.createElement("iframe");
  document.body.append(frame);
  frame.focus();
  return frame;
}

function focusApp(): void {
  const button = document.createElement("button");
  document.body.append(button);
  button.focus();
}

beforeEach(() => {
  document.body.innerHTML = "";
});

describe("keyboardInFrame", () => {
  it("is false with the focus anywhere in the app", () => {
    focusApp();
    expect(keyboardInFrame()).toBe(false);
  });

  it("is true with the focus inside the message body", () => {
    focusFrame();
    expect(keyboardInFrame()).toBe(true);
  });
});

describe("frameKeyIsOurs", () => {
  /*
   * The double-handling guard, and the rule with teeth. Rust gates its monitor
   * on a flag the frontend set, and that flag crosses a process boundary — it
   * can be stale by exactly the width of one message, which is the width of a
   * keystroke. A key acted on here *and* delivered by the DOM in the ordinary
   * way archives two conversations for one `e`.
   */
  it("refuses a key that arrived while the focus was not in a message", () => {
    focusApp();
    expect(frameKeyIsOurs(key())).toBe(false);
  });

  it("takes a letter, which means nothing in a document you cannot type into", () => {
    focusFrame();
    for (const pressed of ["r", "a", "f", "e", "s", "u", "j", "k", "c", "#"]) {
      expect(frameKeyIsOurs(key({ key: pressed })), pressed).toBe(true);
    }
  });

  it("takes Escape, which is the way back out of the message", () => {
    focusFrame();
    expect(frameKeyIsOurs(key({ key: "Escape" }))).toBe(true);
  });

  /*
   * The frame keeps what belongs to the document being read, and here that is
   * not a courtesy: the key was never swallowed — the monitor publishes and
   * hands the `NSEvent` straight back — so the frame is scrolling or copying
   * with it at the same moment. Acting on it here as well would do both at
   * once. ⌘A is the one that would hurt: the app binds it to "select every
   * conversation".
   */
  it("leaves the frame what the frame is using", () => {
    focusFrame();
    for (const held of ["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "]) {
      expect(frameKeyIsOurs(key({ key: held })), held).toBe(false);
    }
    expect(frameKeyIsOurs(key({ key: "a", metaKey: true }))).toBe(false);
    expect(frameKeyIsOurs(key({ key: "c", metaKey: true }))).toBe(false);
  });
});

describe("asKeyEvent", () => {
  it("reads as a key pressed on nothing typeable, which is the truth", () => {
    const event = asKeyEvent(key({ key: "e" }));
    expect(event.target?.isContentEditable).toBe(false);
    expect(event.target?.tagName).toBe("IFRAME");
  });

  /*
   * There is no DOM event here to cancel: the `NSEvent` went back to AppKit
   * before this payload was sent. Both are present because the keymap calls
   * them, and both do nothing.
   */
  it("cancels nothing, because there is nothing here to cancel", () => {
    const event = asKeyEvent(key());
    expect(() => {
      event.preventDefault?.();
      event.stopPropagation?.();
    }).not.toThrow();
  });

  /*
   * `code` is carried for one case: macOS Option remaps the number row to
   * `¡™£¢∞§¶•ª`, so `alt+1` is only recognisable by its physical key. Absent
   * for everything else rather than invented.
   */
  it("carries the physical digit when Option is held", () => {
    expect(asKeyEvent(key({ key: "¡", code: "Digit1", altKey: true })).code).toBe("Digit1");
    expect(asKeyEvent(key({ key: "e" })).code).toBeUndefined();
  });
});
