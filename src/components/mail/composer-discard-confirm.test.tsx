// @vitest-environment jsdom

/**
 * The question a discard raises is asked where the discard was asked for.
 *
 * # What was wrong
 *
 * `discard` is the fourth of six controls on the composer's *bottom* edge, next
 * to `send`, `later`, `attach`, `pop out` and `close`. Pressing it drew a bar on
 * the composer's *top* edge — around 460px above the pointer that had just
 * clicked, and off the top of a popped-out draft entirely, since the bar was
 * rendered outside the overlay. Reported from inside the app as "why is discard
 * confirm far from where I clicked to discard?", which is the polite version of
 * "the click did nothing".
 *
 * # What is asserted
 *
 * That the question is the footer's second state — the same row, the same box,
 * beside the control that raised it — rather than anything above the message.
 * jsdom has no layout engine, so "adjacent" is stated structurally: the answers
 * are inside the element carrying `data-mach-composer-footer`, which is the last
 * row of the composer's column and comes after the writing area in document
 * order. The pixels were looked at in the real window; see the screenshots on
 * the commit.
 *
 * And that the keyboard can both answer it and take it back: Escape keeps the
 * draft — never discards it, which is the one mistake this key could make that
 * cannot be undone — and puts the caret back in the message it came out of.
 */

import { act, type RefObject } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeymapProvider, useKeyBindings } from "@/hooks/useKeymap";
import { detectModKey, normalizeToken } from "@/lib/keymap";
import { COMPOSER_FIXED_ROW } from "./composer-layout";
import { COMPOSER_KEYS, type Draft, type DraftKind } from "@/lib/compose";
import { Composer } from "./Composer";
import type { RichTextEditorHandle } from "./RichTextEditor";

declare global {
  // eslint-disable-next-line no-var
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

/** A reply with somebody to send to, so focus opens in the message. */
function draft(over: Partial<Draft> = {}): Draft {
  return {
    id: "d1",
    accountId: 1,
    kind: "reply" as DraftKind,
    to: [{ email: "ada@example.com" }],
    cc: [],
    bcc: [],
    subject: "Re: the roof",
    body: "<div>half a sentence</div>",
    bodyFormat: "html",
    updatedAt: 0,
    ...over,
  };
}

type Props = Parameters<typeof Composer>[0];

function props(over: Partial<Props> = {}): Props {
  return {
    draft: draft(),
    html: "<div>half a sentence</div>",
    bodyHeight: 340,
    onChange: () => {},
    onBodyChange: () => {},
    onSend: () => {},
    onClose: () => {},
    onDiscard: () => {},
    onAttach: () => {},
    onRemoveAttachment: () => {},
    ...over,
  };
}

/* ------------------------------------------------------------ the markup */

function markup(over: Partial<Props> = {}): HTMLElement {
  const host = document.createElement("div");
  host.innerHTML = renderToStaticMarkup(
    <KeymapProvider>
      <Composer {...props(over)} />
    </KeymapProvider>,
  );
  return host;
}

function footer(host: HTMLElement): HTMLElement {
  const row = host.querySelector<HTMLElement>("[data-mach-composer-footer]");
  if (!row) throw new Error("no footer in the composer");
  return row;
}

/** A button by the words on it, ignoring the key legend it carries. */
function button(host: HTMLElement, label: string): HTMLButtonElement | null {
  return (
    [...host.querySelectorAll("button")].find((candidate) => {
      const copy = candidate.cloneNode(true) as Element;
      for (const kbd of copy.querySelectorAll("kbd")) kbd.remove();
      return copy.textContent?.replace(/\s+/g, " ").trim() === label;
    }) ?? null
  );
}

describe("the question, once it is being asked", () => {
  it("is the footer, where the control that raised it is", () => {
    const host = markup({ confirmingDiscard: true, onKeepDraft: () => {} });
    const row = footer(host);

    const discard = button(host, "Discard");
    const keep = button(host, "Keep");
    expect(discard, "the composer should offer Discard").not.toBeNull();
    expect(keep, "the composer should offer Keep").not.toBeNull();
    expect(row.contains(discard!)).toBe(true);
    expect(row.contains(keep!)).toBe(true);
  });

  /*
   * The bug, stated as the thing that must not come back: nothing is drawn
   * above the message. The writing area is the row before the footer, and the
   * footer is the last one — so a question that has been put back on the
   * composer's top edge fails here rather than in his hands.
   */
  it("is below the message rather than above it", () => {
    const host = markup({ confirmingDiscard: true, onKeepDraft: () => {} });
    const row = footer(host);
    const message = host.querySelector('[role="textbox"][aria-label="Message"]')!;

    expect(row.parentElement!.lastElementChild).toBe(row);
    expect(message.compareDocumentPosition(row) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  /*
   * `discard` is where the pointer is when the question appears, and the row is
   * laid out so that a second click there lands on nothing: the answers are
   * pushed to the far end by the question's own `flex-1`, which is where the
   * bar this replaced put them too.
   */
  it("does not put Discard under the pointer that asked for it", () => {
    const host = markup({ confirmingDiscard: true, onKeepDraft: () => {} });
    const row = footer(host);
    const first = row.firstElementChild!;

    expect(first.tagName).toBe("SPAN");
    expect((first.getAttribute("class") ?? "").split(/\s+/)).toContain("flex-1");
    expect(button(host, "discard"), "the legend's own control is gone").toBeNull();
  });

  /** The row is still a row: it may not be the box that gives up space. */
  it("keeps the footer's size and its name", () => {
    const host = markup({ confirmingDiscard: true, onKeepDraft: () => {} });
    expect((footer(host).getAttribute("class") ?? "").split(/\s+/)).toContain(
      COMPOSER_FIXED_ROW,
    );
  });

  it("is not there at all until it is asked", () => {
    const host = markup();
    expect(button(host, "Keep")).toBeNull();
    expect(button(host, "discard")).not.toBeNull();
  });
});

/* ------------------------------------------------------------- the keys */

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  globalThis.IS_REACT_ACT_ENVIRONMENT = undefined;
});

function mount(over: Partial<Props> = {}) {
  act(() => {
    root.render(
      <KeymapProvider>
        <Composer {...props(over)} />
      </KeymapProvider>,
    );
  });
}

/** The same composer again, with the question now up. Nothing remounts. */
const rerender = mount;

/**
 * Press a binding at whatever has the caret, the way the window would.
 *
 * `held` is the OS repeating a key that has not been let go of, which is a
 * different thing from pressing it again and the composer's discard is the one
 * place that cares. See `armed` in `Composer`.
 */
function press(binding: string, { held = false } = {}) {
  const token = normalizeToken(binding, detectModKey());
  const parts = token.split("+");
  const key = parts.pop() ?? "";
  const named: Record<string, string> = { escape: "Escape", backspace: "Backspace" };
  act(() => {
    (document.activeElement ?? window).dispatchEvent(
      new KeyboardEvent("keydown", {
        key: named[key] ?? key,
        repeat: held,
        metaKey: parts.includes("meta"),
        ctrlKey: parts.includes("ctrl"),
        altKey: parts.includes("alt"),
        shiftKey: parts.includes("shift"),
        bubbles: true,
        cancelable: true,
      }),
    );
  });
}

const message = () =>
  container.querySelector<HTMLElement>('[role="textbox"][aria-label="Message"]')!;

/** The release that turns a press into a decision. See `armed` in `Composer`. */
function letGo() {
  act(() => {
    window.dispatchEvent(new KeyboardEvent("keyup", { key: "Backspace", bubbles: true }));
  });
}

describe("answering it from the keyboard", () => {
  /*
   * It was the *safe* answer under the hands until he asked for this one:
   * "why isn't discard confirm focused". A question raised by a key and
   * answered by a key whose default answer needs the pointer is a question he
   * has to reach for, and Keep already has a key of its own that works from
   * anywhere — Escape.
   *
   * What makes the destructive default safe is not distance, it is `armed`:
   * ⏎ lands on this button, and ⏎ is one of the keys that can raise the
   * question. See the two tests below.
   */
  it("puts the answer under the hands, and it is the one he asked for", () => {
    mount();
    expect(document.activeElement).toBe(message());

    rerender({ confirmingDiscard: true, onKeepDraft: () => {} });

    expect(document.activeElement?.textContent).toContain("Discard");
  });

  /*
   * The press that asks cannot also answer.
   *
   * Every key that raises the question can repeat: hold ⇧⌘⌫, or hold ⏎ on the
   * footer's own `discard` while it has focus, and the OS sends a second event
   * a quarter of a second later — at the button that has meanwhile taken the
   * focus, which is Discard. Nothing has been released in between, so nothing
   * has been decided in between.
   */
  it("is not answered by the discard key repeating while it is held", () => {
    const onDiscard = vi.fn();
    mount({ onDiscard, onKeepDraft: () => {} });
    rerender({ confirmingDiscard: true, onDiscard, onKeepDraft: () => {} });

    press(COMPOSER_KEYS.discard, { held: true });

    expect(onDiscard).not.toHaveBeenCalled();
  });

  /* The same press, arriving at the focused button as ⏎ rather than as ⇧⌘⌫. */
  it("is not answered by the button under a key nothing has released", () => {
    const onDiscard = vi.fn();
    mount({ onDiscard, onKeepDraft: () => {} });
    rerender({ confirmingDiscard: true, onDiscard, onKeepDraft: () => {} });

    act(() => {
      (document.activeElement as HTMLButtonElement).click();
    });

    expect(onDiscard).not.toHaveBeenCalled();
  });

  it("is answered by that button once the hands have let go of something", () => {
    const onDiscard = vi.fn();
    mount({ onDiscard, onKeepDraft: () => {} });
    rerender({ confirmingDiscard: true, onDiscard, onKeepDraft: () => {} });

    letGo();
    act(() => {
      (document.activeElement as HTMLButtonElement).click();
    });

    expect(onDiscard).toHaveBeenCalledTimes(1);
  });

  /* And a modifier coming up on its own is not letting go of anything. */
  it("is not armed by releasing the modifier while the key repeats", () => {
    const onDiscard = vi.fn();
    mount({ onDiscard, onKeepDraft: () => {} });
    rerender({ confirmingDiscard: true, onDiscard, onKeepDraft: () => {} });

    act(() => {
      window.dispatchEvent(new KeyboardEvent("keyup", { key: "Shift", bubbles: true }));
    });
    act(() => {
      (document.activeElement as HTMLButtonElement).click();
    });

    expect(onDiscard).not.toHaveBeenCalled();
  });

  /*
   * The pointer arms it on the way in. A mouse cannot reach the button without
   * pressing on it, and that press is the decision — so there is no delay to
   * sit through, which is the other way this could have been guarded and the
   * one that would have made a deliberate second press feel broken.
   */
  it("is armed by the pointer, so a click on it means it", () => {
    const onDiscard = vi.fn();
    mount({ onDiscard, onKeepDraft: () => {} });
    rerender({ confirmingDiscard: true, onDiscard, onKeepDraft: () => {} });

    const button = document.activeElement as HTMLButtonElement;
    act(() => {
      button.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
      button.click();
    });

    expect(onDiscard).toHaveBeenCalledTimes(1);
  });

  it("keeps the draft on Escape, and gives the caret back to the message", () => {
    const onKeepDraft = vi.fn();
    const onDiscard = vi.fn();
    const onClose = vi.fn();
    mount({ onKeepDraft, onDiscard, onClose });
    rerender({ confirmingDiscard: true, onKeepDraft, onDiscard, onClose });

    press(COMPOSER_KEYS.close);

    expect(onKeepDraft).toHaveBeenCalledTimes(1);
    // The two things Escape must never do while the question is up: throw the
    // draft away, and take the composer with it.
    expect(onDiscard).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(message());
  });

  /*
   * Focus and caret are two answers, and giving back only the first is the
   * half-fix that reads as a second bug: WebKit hands a contenteditable back at
   * character zero, so a writer who was mid-paragraph gets Keep, carries on
   * typing, and finds the words at the top of the draft. Asserted at the
   * editor's handle because jsdom has no Squire selection to read — the pixels
   * were checked in the real window by typing after cancelling.
   */
  it("puts the caret back where it was, not at the top of the draft", () => {
    const handle: RefObject<RichTextEditorHandle | null> = { current: null };
    mount({ editorRef: handle, onKeepDraft: () => {} });

    const real = handle.current!;
    const focus = vi.fn();
    // The offset the message would report while the question is being raised.
    handle.current = { ...real, caret: () => 42, focus };

    rerender({ editorRef: handle, confirmingDiscard: true, onKeepDraft: () => {} });
    press(COMPOSER_KEYS.close);

    expect(focus).toHaveBeenCalledWith(42);
  });

  it("still answers the discard key, which is the second press that means it", () => {
    const onDiscard = vi.fn();
    mount({ onDiscard, onKeepDraft: () => {} });
    rerender({ confirmingDiscard: true, onDiscard, onKeepDraft: () => {} });

    // A second press is a press, which means the first one ended. No wait, and
    // nothing to release first — the event says it is not a repeat.
    press(COMPOSER_KEYS.discard);

    expect(onDiscard).toHaveBeenCalledTimes(1);
  });

  /*
   * The composer's own keys sit at or above the overlay floor and so survive the
   * claim, which is right for ⇧⌘⌫ and wrong for ⌘⏎: a question the draft can be
   * sent out from under while it is being asked is not a question.
   */
  it("does not let the draft be sent out from under the question", () => {
    const onSend = vi.fn();
    mount({ onSend, onKeepDraft: () => {} });
    rerender({ onSend, confirmingDiscard: true, onKeepDraft: () => {} });

    press(COMPOSER_KEYS.send);

    expect(onSend).not.toHaveBeenCalled();
  });

  /*
   * Focus is on a button while the question is up, so `isTypingTarget` is false
   * and every shell binding would otherwise be live behind it — `e` archiving
   * the conversation the draft answers being the one that costs something. The
   * question claims the keyboard the way a dialog does; below the overlay floor
   * nothing answers.
   */
  it("takes the keyboard from everything below the overlay floor", () => {
    const shell = vi.fn();
    /** A shell-priority binding, standing in for `e` archives. */
    function Shell() {
      useKeyBindings([{ keys: "e", description: "Archive", priority: 0, handler: shell }]);
      return null;
    }
    act(() => {
      root.render(
        <KeymapProvider>
          <Shell />
          <Composer {...props({ confirmingDiscard: true, onKeepDraft: () => {} })} />
        </KeymapProvider>,
      );
    });

    press("e");

    expect(shell).not.toHaveBeenCalled();
  });
});
