// @vitest-environment jsdom

/**
 * The composer told the editor it has the keyboard.
 *
 * # The bug this exists for
 *
 * Squire learns it is focused from a `focus` event on its own root and keeps
 * the answer in a private flag. While that flag is false `getSelection` does
 * not read the live caret at all — it returns a range at the top of the
 * message — and every block command asks it for the range. So the editor went
 * on accepting characters perfectly while:
 *
 *   * ⏎ split the *first* block rather than the one being typed in, putting
 *     each new line above everything already written. From the writer's side
 *     that reads as Return doing nothing at all to the line he is on.
 *   * the linkifier that runs inside the same split got the wrong text node,
 *     so a typed URL never became a link. A message went out to a real
 *     recipient as one run-on paragraph with a bare URL in the middle of it.
 *   * bold, both lists and the quote button acted at the top of the message.
 *
 * # Why the flag was wrong
 *
 * `focus()` on an element that already has focus fires no event. `main.tsx`
 * mounts the app in `StrictMode`, which runs every mount effect twice: the
 * first pass builds a Squire and is then destroyed, taking its listeners with
 * it, and the composer focuses the element in between. The surviving Squire
 * attaches its listener to an element that is already focused and never hears
 * anything. Clicking away and back was the only repair.
 *
 * # What is asserted, and what cannot be
 *
 * The property that was missing is exactly "a focus event reached the editor
 * even though the element already had focus", and that is what these assert.
 * They deliberately do not use Squire: jsdom has no contenteditable and no
 * selection to speak of, so an end-to-end "press Return, read the HTML" test is
 * not available here. That half was checked in Blink against the real
 * component — before the fix, typing `first line`, ⏎, `second line` produced
 * `<div>second line</div><div>first line</div>`; after it,
 * `<div>first line</div><div>second line</div>`, and
 * `https://calendar.influencekit.co/` came back wrapped in an `<a href>`.
 */

import { describe, expect, it, vi } from "vitest";
import { focusAndAnnounce } from "./RichTextEditor";

function editable(): HTMLElement {
  const node = document.createElement("div");
  node.contentEditable = "true";
  // jsdom will not move focus to an element it considers unfocusable, and it
  // does not infer that from `contenteditable`.
  node.tabIndex = 0;
  document.body.appendChild(node);
  return node;
}

describe("handing the keyboard to the editor", () => {
  it("announces the focus even when the element already had it", () => {
    const node = editable();
    node.focus();
    expect(document.activeElement).toBe(node);

    // The state the bug lived in: focused, and the editor never told. A second
    // `focus()` fires nothing on its own, which is the whole trap.
    const heard = vi.fn();
    node.addEventListener("focus", heard);

    focusAndAnnounce({ focus: () => node.focus() }, node);

    expect(heard).toHaveBeenCalledTimes(1);
  });

  it("asks the editor to focus itself first", () => {
    const node = editable();
    const focus = vi.fn(() => node.focus());

    focusAndAnnounce({ focus }, node);

    expect(focus).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(node);
  });

  it("says nothing when the focus did not land", () => {
    const node = editable();
    const elsewhere = editable();
    elsewhere.focus();

    const heard = vi.fn();
    node.addEventListener("focus", heard);
    // An editor whose `focus()` does not take — disabled, detached, covered.
    // Announcing focus this element does not have would be a lie, and Squire
    // would then read a selection that is somewhere else entirely.
    focusAndAnnounce({ focus: () => {} }, node);

    expect(heard).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(elsewhere);
  });

  it("does nothing at all before the editor exists", () => {
    expect(() => focusAndAnnounce(null, editable())).not.toThrow();
    expect(() => focusAndAnnounce({ focus: () => {} }, null)).not.toThrow();
  });
});
