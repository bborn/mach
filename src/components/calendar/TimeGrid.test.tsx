// @vitest-environment jsdom

/**
 * Where the week grid opens, and the one way it got that wrong.
 *
 * The report: the calendar opened at 20:02 showing **1 AM at the top** — a
 * screen and a half of empty night with the day's events below the fold —
 * without anybody scrolling it. No arithmetic in `calendar-geometry` can
 * produce that at eight in the evening, and its tests say so. The defect was
 * one line lower down: the opening scroll was a single `scrollTop` write at
 * mount, and **a `scrollTop` write is a request, not an assignment**. An
 * element with no layout — no height, nothing to overflow — takes the write,
 * keeps zero, and reports zero afterwards. Zero is midnight.
 *
 * The calendar is mounted for the life of the window whether or not it is the
 * mode on screen, so "mount" and "laid out" are two different moments, and the
 * effect had `[]` for a dependency list: having missed once, it never asked
 * again.
 *
 * So the three claims here are about the *retry*, not about the number:
 *
 *   1. a grid with no layout is not written to at all — the write that would
 *      be dropped is the write that leaves midnight behind;
 *   2. the first layout the grid is ever given anchors it;
 *   3. preferences arriving after first paint move the floor, and re-anchor —
 *      unless the user has scrolled, in which case the grid is theirs.
 *
 * jsdom has no layout at all, which makes it exactly the right place for this:
 * the sizes are stated, so the moment they arrive is stated too.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HOUR_HEIGHT } from "@/lib/calendar-geometry";
import { LOCAL_STORAGE_KEY } from "@/lib/prefs";
import { PreferencesProvider } from "@/components/prefs/PreferencesProvider";
import { TimeGrid } from "./TimeGrid";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

/** Every `ResizeObserver` the render made, so a layout can be announced. */
let observers: (() => void)[] = [];

/**
 * Enough of a `ResizeObserver` to announce a layout.
 *
 * The day columns use one too — for how many overlapping blocks fit — so the
 * entry has to be shaped like a real one rather than absent.
 */
class FakeResizeObserver {
  constructor(private readonly callback: (entries: ResizeObserverEntry[]) => void) {}
  observe(target: Element) {
    observers.push(() =>
      this.callback([
        { target, contentRect: new DOMRect(0, 0, 200, VIEWPORT) } as ResizeObserverEntry,
      ]),
    );
  }
  disconnect() {}
}

let container: HTMLDivElement;
let root: Root;

/** Two in the afternoon, which is a time the grid can honour exactly. */
const TWO_PM = new Date(2026, 7, 12, 14, 0, 0, 0);
const VIEWPORT = 500;
const CONTENT = 24 * HOUR_HEIGHT + 82; // the day, plus the header and all-day strip

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  observers = [];
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.useFakeTimers();
  vi.setSystemTime(TWO_PM);
  window.localStorage.clear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  globalThis.IS_REACT_ACT_ENVIRONMENT = undefined;
});

function grid() {
  return (
    <TimeGrid
      days={[TWO_PM]}
      events={[]}
      colorFor={() => "#7c6cff"}
      dark={false}
      selectedId={null}
      onSelect={() => {}}
      onOpen={() => {}}
      onDraft={() => {}}
      onMove={() => {}}
      todayNonce={0}
    />
  );
}

/** The scroll container — the grid's own root element. */
function scroller(): HTMLElement {
  const node = container.firstElementChild;
  if (!(node instanceof HTMLElement)) throw new Error("the grid did not render");
  return node;
}

/** Give the grid a box, the way a window that has been laid out would. */
function layOut(node: HTMLElement, viewport = VIEWPORT) {
  Object.defineProperty(node, "clientHeight", { configurable: true, get: () => viewport });
  Object.defineProperty(node, "scrollHeight", { configurable: true, get: () => CONTENT });
}

/** What the grid should be showing at 2pm with the default nine-to-five. */
const AFTERNOON = 14 * HOUR_HEIGHT - VIEWPORT / 4;

describe("the opening scroll", () => {
  it("does not write into a grid that has no layout", () => {
    act(() => root.render(grid()));
    // jsdom stores whatever is assigned, so this is a real assertion: the old
    // code wrote 672 here, against an element that could not take it, and the
    // browser kept the zero — which is the bug, at 1 AM.
    expect(scroller().scrollTop).toBe(0);
  });

  it("takes the first layout the grid is ever given", () => {
    act(() => root.render(grid()));
    const node = scroller();
    expect(node.scrollTop).toBe(0);

    layOut(node);
    act(() => observers.forEach((fire) => fire()));

    expect(node.scrollTop).toBe(AFTERNOON);
  });

  it("anchors immediately when the layout is already there", () => {
    // The ordinary case: the window is up, the calendar mounts into a real box.
    // A `ResizeObserver` is not needed and the first paint is already right.
    const laidOut = document.createElement("div");
    Object.defineProperty(HTMLDivElement.prototype, "clientHeight", {
      configurable: true,
      get: () => VIEWPORT,
    });
    Object.defineProperty(HTMLDivElement.prototype, "scrollHeight", {
      configurable: true,
      get: () => CONTENT,
    });
    try {
      act(() => root.render(grid()));
      expect(scroller().scrollTop).toBe(AFTERNOON);
    } finally {
      Reflect.deleteProperty(HTMLDivElement.prototype, "clientHeight");
      Reflect.deleteProperty(HTMLDivElement.prototype, "scrollHeight");
      laidOut.remove();
    }
  });
});

describe("preferences arriving after first paint", () => {
  /** Mount inside the store, which answers on a promise the way the app's does. */
  function mountWithStore() {
    act(() => root.render(<PreferencesProvider>{grid()}</PreferencesProvider>));
  }

  /** Let the store's read resolve. */
  async function settle() {
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
  }

  it("re-anchors when a late working day moves the floor", async () => {
    // Six in the morning, stored — so the floor is 5.5 rather than the default
    // 8.5, which is the difference between opening on the working day and
    // opening on the hour you are actually in.
    window.localStorage.setItem(
      LOCAL_STORAGE_KEY,
      JSON.stringify({ workingHours: { start: 6, end: 22 } }),
    );
    vi.setSystemTime(new Date(2026, 7, 12, 10, 0, 0, 0));

    mountWithStore();
    const node = scroller();
    layOut(node);
    act(() => observers.forEach((fire) => fire()));
    // The default nine-to-five is all the first render has to go on, and at
    // ten in the morning its floor is still below now: 08:30 at the top.
    expect(node.scrollTop).toBe(8.5 * HOUR_HEIGHT);

    await settle();

    // With the working day the user actually keeps, now is free to sit a
    // quarter of the way down where it belongs.
    expect(node.scrollTop).toBe(10 * HOUR_HEIGHT - VIEWPORT / 4);
  });

  it("leaves a grid the user has scrolled where they put it", async () => {
    window.localStorage.setItem(
      LOCAL_STORAGE_KEY,
      JSON.stringify({ workingHours: { start: 6, end: 22 } }),
    );
    vi.setSystemTime(new Date(2026, 7, 12, 8, 0, 0, 0));

    mountWithStore();
    const node = scroller();
    layOut(node);
    act(() => observers.forEach((fire) => fire()));
    expect(node.scrollTop).toBe(8.5 * HOUR_HEIGHT);

    // A wheel, a scrollbar, an arrow key onto a block off screen — anything
    // that moves it and is not us.
    act(() => {
      node.scrollTop = 900;
      node.dispatchEvent(new Event("scroll", { bubbles: false }));
    });

    await settle();

    expect(node.scrollTop).toBe(900);
  });
});
