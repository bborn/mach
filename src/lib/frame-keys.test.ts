/**
 * Which keystrokes the message document keeps, and which the app gets back.
 *
 * # The bug this exists for
 *
 * A message body is a real iframe. The moment anything inside it has focus —
 * one click to select a word, to scroll, or on the way to a link — its keydowns
 * fire in *its* document and never reach the window the keymap listens on. The
 * whole keyboard goes dead at once: `r`, archive, star, snooze, and the way back
 * to the list. Clicking the thread list revives all of it, which is why it
 * arrived as "the R shortcut for replying isn't working consistently" rather
 * than as "the keyboard stops working".
 *
 * Measured in Blink against the real component before the fix: with the frame
 * focused, `r` opened no composer and `e`, `s` and `u` did nothing at all.
 *
 * # Why this is a list and not "forward everything"
 *
 * Because a document being read owns some of these keys, and two of them would
 * do real damage if the app took them:
 *
 *  * ⌘A is "select all conversations" in the app and "select this message's
 *    text" here. Taking it would turn a copy into a fifty-thread selection.
 *  * ↑ and ↓ scroll the message. Taking them would move the thread cursor —
 *    and so the whole reading pane — out from under a message being read.
 *
 * Everything else has no meaning inside a read-only document: the sandbox
 * carries no `allow-forms` and the sanitizer drops every input, so there is
 * nothing in here a letter could be typed into.
 */

import { describe, expect, it } from "vitest";
import { frameKeepsKey } from "./message-body";

describe("keys inside a message body", () => {
  it("gives the app the letters it binds", () => {
    // The reported one first, then the rest of the triage vocabulary.
    for (const key of ["r", "a", "f", "e", "s", "u", "j", "k", "c", "#"]) {
      expect(frameKeepsKey({ key }), key).toBe(false);
    }
  });

  it("gives the app Escape and the modified keys it binds", () => {
    expect(frameKeepsKey({ key: "Escape" })).toBe(false);
    expect(frameKeepsKey({ key: "z", metaKey: true })).toBe(false);
    expect(frameKeepsKey({ key: "1", metaKey: true })).toBe(false);
    expect(frameKeepsKey({ key: "k", metaKey: true })).toBe(false);
  });

  it("keeps the keys that move within the message", () => {
    for (const key of [
      "ArrowUp",
      "ArrowDown",
      "ArrowLeft",
      "ArrowRight",
      "PageUp",
      "PageDown",
      "Home",
      "End",
      " ",
    ]) {
      expect(frameKeepsKey({ key }), key).toBe(true);
    }
  });

  it("keeps select-all and copy, whichever modifier is held", () => {
    // ⌘A is the one that matters: the app binds it to "select every
    // conversation", and here it means the text of this message.
    expect(frameKeepsKey({ key: "a", metaKey: true })).toBe(true);
    expect(frameKeepsKey({ key: "a", ctrlKey: true })).toBe(true);
    expect(frameKeepsKey({ key: "c", metaKey: true })).toBe(true);
    // Shift-held capitals reach the same answer; the check is on the letter.
    expect(frameKeepsKey({ key: "A", metaKey: true })).toBe(true);
  });

  it("does not keep a bare a or c — those are reply-all and compose", () => {
    expect(frameKeepsKey({ key: "a" })).toBe(false);
    expect(frameKeepsKey({ key: "c" })).toBe(false);
  });
});
