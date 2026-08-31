// @vitest-environment jsdom

/**
 * A row of tabs is one control, and the keyboard has to agree.
 *
 * The composer strip is the reason this exists, and the two failures it could
 * have shipped with are both here: a stop per tab, which puts every open draft
 * between the writer and the message; and no stop at all, which is what a
 * roving `tabIndex` degrades to the moment nothing matches the selection.
 *
 * The arrow keys go through the keymap rather than an `onKeyDown`, so they are
 * exercised the way the app produces them — a `keydown` on `window`, in the
 * capture phase, with a `KeymapProvider` mounted.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { TabStrip, type TabItem } from "./tabs";

let host: HTMLDivElement;
let root: Root;
let selected: string[];

const ITEMS: TabItem[] = ["a", "b", "c"].map((id) => ({
  id,
  label: `Draft ${id}`,
  children: id,
}));

function mount(activeId: string | null, items: TabItem[] = ITEMS) {
  act(() => {
    root.render(
      <KeymapProvider>
        <TabStrip
          items={items}
          activeId={activeId}
          onSelect={(id) => selected.push(id)}
          label="Open drafts"
        />
      </KeymapProvider>,
    );
  });
}

function tabs(): HTMLButtonElement[] {
  return [...host.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
}

function press(key: string) {
  act(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  });
}

// jsdom has no layout, so it has no `scrollIntoView`. Same stub as
// `ThreadList.renders.test.tsx` and the calendar's key tests.
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = function scrollIntoView() {};
}

beforeEach(() => {
  selected = [];
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

describe("TabStrip", () => {
  it("is a tablist of tabs, and says which one is selected", () => {
    mount("b");
    const list = host.querySelector('[role="tablist"]')!;
    expect(list.getAttribute("aria-label")).toBe("Open drafts");
    expect(tabs().map((tab) => tab.getAttribute("aria-selected"))).toEqual([
      "false",
      "true",
      "false",
    ]);
  });

  it("takes one tab stop, not one per tab", () => {
    mount("b");
    expect(tabs().map((tab) => tab.tabIndex)).toEqual([-1, 0, -1]);
  });

  // Otherwise ⇥ walks straight past a strip nothing in it is selected in.
  it("keeps a stop when the selection names no tab in it", () => {
    mount(null);
    expect(tabs().map((tab) => tab.tabIndex)).toEqual([0, -1, -1]);
  });

  it("moves focus along the row with the arrows, and to the ends", () => {
    mount("a");
    act(() => tabs()[0]!.focus());
    press("ArrowRight");
    expect(document.activeElement).toBe(tabs()[1]);
    press("ArrowRight");
    expect(document.activeElement).toBe(tabs()[2]);
    // It stops at the end rather than wrapping: the row is short and a wrap
    // that lands back at the start reads as nothing having happened.
    press("ArrowRight");
    expect(document.activeElement).toBe(tabs()[2]);
    press("Home");
    expect(document.activeElement).toBe(tabs()[0]);
    press("End");
    expect(document.activeElement).toBe(tabs()[2]);
  });

  /*
   * Moving is not choosing. Selecting a composer navigates to its
   * conversation, so arrowing across the strip with automatic activation would
   * be one navigation per keystroke.
   */
  it("selects nothing until ⏎ or Space", () => {
    mount("a");
    act(() => tabs()[0]!.focus());
    press("ArrowRight");
    press("ArrowRight");
    expect(selected).toEqual([]);
    press("Enter");
    expect(selected).toEqual(["c"]);
    press(" ");
    expect(selected).toEqual(["c", "c"]);
  });

  it("leaves the arrows alone once focus has left the strip", () => {
    mount("a");
    act(() => tabs()[0]!.focus());
    act(() => (document.activeElement as HTMLElement).blur());
    press("ArrowRight");
    expect(document.activeElement).not.toBe(tabs()[1]);
  });
});
