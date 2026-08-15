// @vitest-environment jsdom

/**
 * "mach should warn me about sending without a subject".
 *
 * # What it is
 *
 * The footer's third state, and the second question it can be. `discard` had
 * already taught this row how to ask something — the band, the answers at the
 * far end, the arming guard, the caret handed back — so a subject is asked
 * about in the same box rather than in a dialog of its own. See `asking` in
 * `Composer`.
 *
 * # What is asserted
 *
 * The gate: whitespace is not a subject, a real subject is never asked about,
 * and every way of sending goes through the one check — ⌘⏎, the footer's
 * `send`, and each instant on the `later` row, because a message scheduled
 * without a subject arrives without one.
 *
 * The answer: yes sends immediately and exactly once; the press that raised the
 * question cannot also answer it; Escape is the whole way out and puts the
 * caret in the subject field, which is the thing being asked about.
 *
 * And that the two questions cannot both be up: neither key can raise the other
 * one's while it is being asked.
 *
 * jsdom has no layout engine, so "the same row" is stated structurally. The
 * pixels were looked at in the real window; see the screenshots on the commit.
 */

import { act, type RefObject } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { detectModKey, normalizeToken } from "@/lib/keymap";
import { COMPOSER_FIXED_ROW } from "./composer-layout";
import { COMPOSER_KEYS, scheduleOptions, type Draft, type DraftKind } from "@/lib/compose";
import { Composer } from "./Composer";
import type { RichTextEditorHandle } from "./RichTextEditor";

declare global {
  // eslint-disable-next-line no-var
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

/**
 * A new message with somebody to send it to and nothing on the subject line.
 *
 * `new` rather than `reply` because that is where this can actually happen: a
 * reply's subject is derived and `replySubject` returns `Re:` at its emptiest,
 * so a reply always has one. It is also the kind whose subject is a field
 * rather than a heading, which is where the caret goes on the way out.
 */
function draft(over: Partial<Draft> = {}): Draft {
  return {
    id: "d1",
    accountId: 1,
    kind: "new" as DraftKind,
    to: [{ email: "ada@example.com" }],
    cc: [],
    bcc: [],
    subject: "",
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
    // Without a surface to hold the question there is nothing to raise, and the
    // composer sends the way it always did. Every test here has one.
    onConfirmSubject: () => {},
    onSendAnyway: () => {},
    onKeepWriting: () => {},
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
  it("is the footer, in the band the other question already uses", () => {
    const host = markup({ confirmingSubject: true });
    const row = footer(host);

    expect(row.textContent).toContain("Send without a subject?");
    const send = button(host, "Send");
    const back = button(host, "Add one");
    expect(send, "the band should offer Send").not.toBeNull();
    expect(back, "the band should offer a way back").not.toBeNull();
    expect(row.contains(send!)).toBe(true);
    expect(row.contains(back!)).toBe(true);
    // The legend it replaced, gone: the row is the question while it is asked.
    expect(button(host, "later")).toBeNull();
    expect(button(host, "discard")).toBeNull();
  });

  /*
   * The copy takes the measure, which is what pushes the answers to the far end
   * — away from the pointer that has just clicked `send`, a sixth of the way
   * along this row.
   */
  it("puts the answers at the far end, the way the other one does", () => {
    const host = markup({ confirmingSubject: true });
    const first = footer(host).firstElementChild!;

    expect(first.tagName).toBe("SPAN");
    expect((first.getAttribute("class") ?? "").split(/\s+/)).toContain("flex-1");
  });

  /** The row is still a row: it may not be the box that gives up space. */
  it("keeps the footer's size and its name", () => {
    const host = markup({ confirmingSubject: true });
    expect((footer(host).getAttribute("class") ?? "").split(/\s+/)).toContain(
      COMPOSER_FIXED_ROW,
    );
  });

  it("is below the message rather than above it", () => {
    const host = markup({ confirmingSubject: true });
    const row = footer(host);
    const message = host.querySelector('[role="textbox"][aria-label="Message"]')!;

    expect(row.parentElement!.lastElementChild).toBe(row);
    expect(message.compareDocumentPosition(row) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("is not there at all until it is asked", () => {
    const host = markup();
    expect(button(host, "Add one")).toBeNull();
    expect(button(host, "send")).not.toBeNull();
  });

  /*
   * Both flags at once should be impossible — the dock clears one when it sets
   * the other — but if it ever happened, the question that cannot be undone is
   * the one worth answering first.
   */
  it("gives the row to the discard question if both ever arrive", () => {
    const host = markup({ confirmingSubject: true, confirmingDiscard: true });
    expect(footer(host).textContent).toContain("Discard this draft?");
    expect(footer(host).textContent).not.toContain("Send without a subject?");
  });
});

/* --------------------------------------------------------------- the keys */

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

/** Press a binding at whatever has the caret, the way the window would. */
function press(binding: string, { held = false } = {}) {
  const token = normalizeToken(binding, detectModKey());
  const parts = token.split("+");
  const key = parts.pop() ?? "";
  const named: Record<string, string> = { escape: "Escape", backspace: "Backspace", enter: "Enter" };
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
const subject = () => container.querySelector<HTMLInputElement>("#composer-subject")!;
const find = (label: string) => button(container, label);

/** The release that turns a press into a decision. See `armed` in `Composer`. */
function letGo() {
  act(() => {
    window.dispatchEvent(new KeyboardEvent("keyup", { key: "Enter", bubbles: true }));
  });
}

describe("what raises it", () => {
  it("is a subject that is not there", () => {
    const onConfirmSubject = vi.fn();
    const onSend = vi.fn();
    mount({ onConfirmSubject, onSend });

    press(COMPOSER_KEYS.send);

    expect(onConfirmSubject).toHaveBeenCalledTimes(1);
    // Nothing is queued behind the question.
    expect(onSend).not.toHaveBeenCalled();
  });

  /*
   * The whole reason the check is `trim()`: a subject of spaces is drawn as
   * "(no subject)" at both ends, so a warning that took whitespace for a
   * subject would be a warning the space bar walks past.
   */
  it("is a subject of nothing but whitespace", () => {
    const onConfirmSubject = vi.fn();
    const onSend = vi.fn();
    mount({ draft: draft({ subject: "  \t \n " }), onConfirmSubject, onSend });

    press(COMPOSER_KEYS.send);

    expect(onConfirmSubject).toHaveBeenCalledTimes(1);
    expect(onSend).not.toHaveBeenCalled();
  });

  /*
   * The failure that matters most. A warning that fires on a message which has
   * a subject teaches the writer to answer it without reading it, and then the
   * one time it is right he has already pressed ⌘⏎ twice.
   */
  it("is never a subject that exists", () => {
    const onConfirmSubject = vi.fn();
    const onSend = vi.fn();
    mount({ draft: draft({ subject: "The roof" }), onConfirmSubject, onSend });

    press(COMPOSER_KEYS.send);

    expect(onConfirmSubject).not.toHaveBeenCalled();
    expect(onSend).toHaveBeenCalledTimes(1);
  });

  it("is never a subject with words and space around them", () => {
    const onConfirmSubject = vi.fn();
    const onSend = vi.fn();
    mount({ draft: draft({ subject: "  The roof  " }), onConfirmSubject, onSend });

    press(COMPOSER_KEYS.send);

    expect(onConfirmSubject).not.toHaveBeenCalled();
    expect(onSend).toHaveBeenCalledTimes(1);
  });

  /* A reply is derived down to `Re:`, so it has one and is never asked. */
  it("is never a reply, whose subject it did not choose", () => {
    const onConfirmSubject = vi.fn();
    const onSend = vi.fn();
    mount({
      draft: draft({ kind: "reply" as DraftKind, subject: "Re:" }),
      onConfirmSubject,
      onSend,
    });

    press(COMPOSER_KEYS.send);

    expect(onConfirmSubject).not.toHaveBeenCalled();
    expect(onSend).toHaveBeenCalledTimes(1);
  });

  it("is the footer's own send, not only the key", () => {
    const onConfirmSubject = vi.fn();
    const onSend = vi.fn();
    mount({ onConfirmSubject, onSend });

    act(() => {
      find("send")!.click();
    });

    expect(onConfirmSubject).toHaveBeenCalledTimes(1);
    expect(onSend).not.toHaveBeenCalled();
  });

  /*
   * And a scheduled send, which is the one that could have been missed: the
   * delay changes nothing about what lands in somebody's inbox. The instant
   * goes with the question so that answering it still schedules.
   */
  it("is a scheduled send too, and it carries the instant", () => {
    const onConfirmSubject = vi.fn();
    const onSend = vi.fn();
    mount({ onConfirmSubject, onSend });

    press(COMPOSER_KEYS.schedule);
    const [first] = scheduleOptions();
    act(() => {
      find(first.label)!.click();
    });

    expect(onSend).not.toHaveBeenCalled();
    expect(onConfirmSubject).toHaveBeenCalledTimes(1);
    expect(typeof onConfirmSubject.mock.calls[0][0]).toBe("number");
  });
});

describe("answering it from the keyboard", () => {
  it("puts the affirmative under the hands, as the other question does", () => {
    mount();
    expect(document.activeElement).toBe(message());

    rerender({ confirmingSubject: true });

    expect(document.activeElement?.textContent).toContain("Send");
  });

  /*
   * The press that asks cannot also answer. ⌘⏎ raises this question, and the OS
   * sends a second event a quarter of a second later at the Send button that
   * has meanwhile taken the focus. Nothing was released in between, so nothing
   * was decided in between.
   */
  it("is not answered by the send key repeating while it is held", () => {
    const onSendAnyway = vi.fn();
    mount({ onSendAnyway });
    rerender({ confirmingSubject: true, onSendAnyway });

    press(COMPOSER_KEYS.send, { held: true });

    expect(onSendAnyway).not.toHaveBeenCalled();
  });

  /* The same press arriving at the focused button as ⏎ rather than as ⌘⏎. */
  it("is not answered by the button under a key nothing has released", () => {
    const onSendAnyway = vi.fn();
    mount({ onSendAnyway });
    rerender({ confirmingSubject: true, onSendAnyway });

    act(() => {
      (document.activeElement as HTMLButtonElement).click();
    });

    expect(onSendAnyway).not.toHaveBeenCalled();
  });

  it("is answered once the hands have let go of something, and only once", () => {
    const onSendAnyway = vi.fn();
    const onSend = vi.fn();
    mount({ onSendAnyway, onSend });
    rerender({ confirmingSubject: true, onSendAnyway, onSend });

    letGo();
    act(() => {
      (document.activeElement as HTMLButtonElement).click();
    });

    expect(onSendAnyway).toHaveBeenCalledTimes(1);
    // One path out, and it is not the one that would ask again.
    expect(onSend).not.toHaveBeenCalled();
  });

  it("is answered by a second press of the key that asked", () => {
    const onSendAnyway = vi.fn();
    mount({ onSendAnyway });
    rerender({ confirmingSubject: true, onSendAnyway });

    // Not a repeat: a second press is a press, so the first one ended.
    press(COMPOSER_KEYS.send);

    expect(onSendAnyway).toHaveBeenCalledTimes(1);
  });

  it("is armed by the pointer, so a click on it means it", () => {
    const onSendAnyway = vi.fn();
    mount({ onSendAnyway });
    rerender({ confirmingSubject: true, onSendAnyway });

    const answer = document.activeElement as HTMLButtonElement;
    act(() => {
      answer.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
      answer.click();
    });

    expect(onSendAnyway).toHaveBeenCalledTimes(1);
  });

  /*
   * One key out. The message this question stands in front of was already
   * recallable, so the warning may cost a keystroke to dismiss and not two —
   * and Escape must never be the key that sends it.
   */
  it("is dismissed by Escape, which never sends and never closes the panel", () => {
    const onKeepWriting = vi.fn();
    const onSendAnyway = vi.fn();
    const onSend = vi.fn();
    const onClose = vi.fn();
    mount({ onKeepWriting, onSendAnyway, onSend, onClose });
    rerender({ confirmingSubject: true, onKeepWriting, onSendAnyway, onSend, onClose });

    press(COMPOSER_KEYS.close);

    expect(onKeepWriting).toHaveBeenCalledTimes(1);
    expect(onSendAnyway).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });

  /*
   * Where the caret lands is the one place this question deliberately differs
   * from the discard: that one gives back exactly what it took, because
   * cancelling it changes nothing. This one was asked about a field, and "no"
   * means "let me write one" — so the field it is about takes the caret and the
   * next thing typed is the subject.
   */
  it("leaves the caret in the subject, which is what it asked about", () => {
    mount();
    rerender({ confirmingSubject: true });

    press(COMPOSER_KEYS.close);

    expect(document.activeElement).toBe(subject());
  });

  it("does the same from the band's own way out", () => {
    const onKeepWriting = vi.fn();
    mount({ onKeepWriting });
    rerender({ confirmingSubject: true, onKeepWriting });

    letGo();
    act(() => {
      find("Add one")!.click();
    });

    expect(onKeepWriting).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(subject());
  });

  /*
   * With no field to go to — a reply, whose subject is a heading — it falls
   * back to the discard question's answer: back where they were, at the offset
   * they were at. Asserted at the editor's handle because jsdom has no Squire
   * selection to read.
   */
  it("falls back to where the caret was when there is no subject field", () => {
    const handle: RefObject<RichTextEditorHandle | null> = { current: null };
    const reply = draft({ kind: "reply" as DraftKind, subject: "" });
    mount({ draft: reply, editorRef: handle });

    const real = handle.current!;
    const focus = vi.fn();
    handle.current = { ...real, caret: () => 42, focus };

    rerender({ draft: reply, editorRef: handle, confirmingSubject: true });
    press(COMPOSER_KEYS.close);

    expect(focus).toHaveBeenCalledWith(42);
  });
});

describe("one question at a time", () => {
  /*
   * ⇧⌘⌫ is live in the composer and would otherwise raise the discard question
   * behind this one, leaving two questions in a row that holds one.
   */
  it("does not let a discard be raised behind the subject question", () => {
    const onDiscard = vi.fn();
    mount({ onDiscard });
    rerender({ confirmingSubject: true, onDiscard });

    press(COMPOSER_KEYS.discard);

    expect(onDiscard).not.toHaveBeenCalled();
  });

  /* And the other way: ⌘⏎ is dead while the discard question is up, so it
     cannot send the draft out from under it — nor ask about its subject. */
  it("does not let the subject question be raised behind a discard", () => {
    const onConfirmSubject = vi.fn();
    const onSend = vi.fn();
    mount({ onConfirmSubject, onSend, onKeepDraft: () => {} });
    rerender({ confirmingDiscard: true, onConfirmSubject, onSend, onKeepDraft: () => {} });

    press(COMPOSER_KEYS.send);

    expect(onConfirmSubject).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
  });

  /* The other composer keys wait for an answer, the way they do for a discard. */
  it("holds the schedule and attach keys while it is up", () => {
    const onAttach = vi.fn();
    mount({ onAttach });
    rerender({ confirmingSubject: true, onAttach });

    press(COMPOSER_KEYS.attach);
    press(COMPOSER_KEYS.schedule);

    expect(onAttach).not.toHaveBeenCalled();
    expect(find("In 3 hours")).toBeNull();
  });
});
