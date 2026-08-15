// @vitest-environment jsdom

/**
 * The footer row is six controls, and every one of them is one.
 *
 * # What was wrong
 *
 * The row read as a legend and was half a button bar. `send`, `later` and
 * `close` were `<span>`s — a key glyph and a word, clickable in the sense that
 * a paragraph is clickable. `attach`, `discard` and `pop out` were buttons.
 * All six carried the same `inline-flex items-center gap-1 hover:…`: no
 * padding, so the hit target was the glyphs; no cursor; no pressed state; no
 * fill under the pointer. Reported from inside the app as "improve this UX …
 * these should feel like buttons/clickable/cursor change etc.", with arrows
 * drawn at the two that *were* buttons — which is the tell. If the ones that
 * worked did not look like they worked, the ones that did nothing looked
 * exactly the same.
 *
 * # What is asserted
 *
 * That every act the row offers can be reached and taken with the pointer and
 * with the keyboard, that the two routes do the same thing, and that a row
 * whose fields have gone read-only offers neither. And that the six controls
 * are one shape rather than six — the class-level half of "it looks like a
 * button", which is as far as jsdom can go; the pixels were looked at in the
 * real window, light and dark, and the screenshots are on the commit.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { detectModKey, normalizeToken } from "@/lib/keymap";
import { COMPOSER_KEYS, type Draft, type DraftKind } from "@/lib/compose";
import { Composer } from "./Composer";

declare global {
  // eslint-disable-next-line no-var
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

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

/** The word on a control, with the key chip it carries taken off. */
function label(element: Element): string {
  const copy = element.cloneNode(true) as Element;
  for (const kbd of copy.querySelectorAll("kbd")) kbd.remove();
  return copy.textContent?.replace(/\s+/g, " ").trim() ?? "";
}

function controls(host: ParentNode): HTMLButtonElement[] {
  return [...host.querySelectorAll<HTMLButtonElement>("[data-mach-composer-footer] button")];
}

function control(host: ParentNode, name: string): HTMLButtonElement {
  const found = controls(host).find((candidate) => label(candidate) === name);
  if (!found) throw new Error(`no ${name} in the footer`);
  return found;
}

/* ------------------------------------------------- the shape of the row */

function markup(over: Partial<Props> = {}): HTMLElement {
  const host = document.createElement("div");
  host.innerHTML = renderToStaticMarkup(
    <KeymapProvider>
      <Composer {...props(over)} />
    </KeymapProvider>,
  );
  return host;
}

describe("the footer's controls", () => {
  it("are all controls, and there is nothing else in the row", () => {
    const host = markup({ onPopOut: () => {} });
    expect(controls(host).map(label)).toEqual([
      "send",
      "later",
      "attach",
      "discard",
      "pop out",
      "close",
    ]);
    // And nothing in the row is a legend pretending to be one of them: a key
    // chip means a control, which is the defect stated as an invariant. The
    // other children are the states that are news rather than acts — see
    // `isLocalOnly` and `GhostHint` — and they carry no chip.
    const row = host.querySelector("[data-mach-composer-footer]")!;
    const chipped = [...row.children].filter((child) => child.querySelector("kbd"));
    expect(chipped.length).toBe(6);
    expect(chipped.every((child) => child.tagName === "BUTTON")).toBe(true);
  });

  it("each name a key, which is the row's whole vocabulary", () => {
    const host = markup({ onPopOut: () => {} });
    for (const button of controls(host)) {
      expect(button.querySelectorAll("kbd").length, label(button)).toBe(1);
    }
    // `attach` was the one with an icon where the others had chips, and
    // `discard` the one with neither.
    expect(control(host, "attach").querySelector("svg")).toBeNull();
    expect(control(host, "discard").querySelector("kbd")?.textContent).toBeTruthy();
  });

  /*
   * One box, six times. Stated as the classes that produce it because jsdom has
   * no layout and no styles: the height, the padding either side of the words —
   * which is the hit target the row did not have — and the cursor he asked for
   * by name. What differs between them is colour, and only colour.
   */
  it("are one box rather than six", () => {
    const host = markup({ onPopOut: () => {} });
    for (const button of controls(host)) {
      const classes = (button.getAttribute("class") ?? "").split(/\s+/);
      for (const token of ["h-6", "px-2", "cursor-pointer", "transition-colors"]) {
        expect(classes, `${label(button)} is missing ${token}`).toContain(token);
      }
      expect(button.getAttribute("type")).toBe("button");
    }
  });

  it("give the destructive one no red until it is pointed at", () => {
    const host = markup();
    const classes = control(host, "discard").getAttribute("class") ?? "";
    expect(classes).not.toMatch(/(?:^|\s)(?:bg|text)-danger/);
    expect(classes).toMatch(/hover:text-danger/);
  });

  it("do not offer the pointer a control that is not live", () => {
    const host = markup({ busy: true });
    const off = ["send", "later", "attach", "discard"];
    for (const name of off) {
      const button = control(host, name);
      expect(button.disabled, name).toBe(true);
      // Dimmed, and out of reach of the pointer — so no hover fill and no hand
      // cursor on something that will not answer.
      expect(button.getAttribute("class")).toContain("disabled:pointer-events-none");
      expect(button.getAttribute("class")).toContain("disabled:opacity-40");
    }
  });
});

/* ------------------------------------------------------- taking the acts */

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

/** A real click, the way a pointer makes one. */
function click(button: HTMLButtonElement) {
  act(() => {
    button.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    button.click();
  });
}

/** Press a binding at whatever has the caret, the way the window would. */
function press(binding: string) {
  const token = normalizeToken(binding, detectModKey());
  const parts = token.split("+");
  const key = parts.pop() ?? "";
  const named: Record<string, string> = { escape: "Escape", backspace: "Backspace" };
  act(() => {
    (document.activeElement ?? window).dispatchEvent(
      new KeyboardEvent("keydown", {
        key: named[key] ?? key,
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

/**
 * The acts, each with the two ways of taking it.
 *
 * `later` is the odd one: it opens the schedule row rather than calling
 * anything, so it is checked by what appears.
 */
const ACTS = [
  { name: "send", prop: "onSend", keys: COMPOSER_KEYS.send },
  { name: "attach", prop: "onAttach", keys: COMPOSER_KEYS.attach },
  { name: "discard", prop: "onDiscard", keys: COMPOSER_KEYS.discard },
  { name: "close", prop: "onClose", keys: COMPOSER_KEYS.close },
  { name: "pop out", prop: "onPopOut", keys: COMPOSER_KEYS.popOut },
] as const;

describe("every act in the row", () => {
  for (const act_ of ACTS) {
    it(`answers the pointer on ${act_.name}`, () => {
      const handler = vi.fn();
      mount({ onPopOut: () => {}, [act_.prop]: handler });
      click(control(container, act_.name));
      expect(handler).toHaveBeenCalledTimes(1);
    });

    it(`answers ${act_.keys} as well, and does the same thing`, () => {
      const handler = vi.fn();
      mount({ onPopOut: () => {}, [act_.prop]: handler });
      press(act_.keys);
      expect(handler).toHaveBeenCalledTimes(1);
    });
  }

  it("opens the schedule row from `later`, and from its key", () => {
    mount();
    const options = () => container.querySelectorAll("button").length;
    const before = options();

    click(control(container, "later"));
    expect(options()).toBeGreaterThan(before);

    // Back again, from the same control.
    click(control(container, "later"));
    expect(options()).toBe(before);

    press(COMPOSER_KEYS.schedule);
    expect(options()).toBeGreaterThan(before);
  });

  it("says which one is open, for anything reading the row", () => {
    mount();
    expect(control(container, "later").getAttribute("aria-expanded")).toBe("false");
    click(control(container, "later"));
    expect(control(container, "later").getAttribute("aria-expanded")).toBe("true");
  });
});

/*
 * A send in flight is the one state where the row has to refuse: the fields are
 * read-only behind it, and a second ⌘⏎ would queue the message twice. Both
 * routes decline, which is the point — a disabled button whose key still fires
 * is a control that lies about which of the two is the real one.
 */
describe("while a send is in flight", () => {
  for (const act_ of ACTS.filter((a) => a.name !== "close" && a.name !== "pop out")) {
    it(`refuses ${act_.name}, by pointer and by key`, () => {
      const handler = vi.fn();
      mount({ busy: true, [act_.prop]: handler });
      click(control(container, act_.name));
      press(act_.keys);
      expect(handler).not.toHaveBeenCalled();
    });
  }

  // Closing the panel is not a write, and neither is moving it. They stay.
  it("still lets the panel be closed", () => {
    const onClose = vi.fn();
    mount({ busy: true, onClose });
    click(control(container, "close"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
