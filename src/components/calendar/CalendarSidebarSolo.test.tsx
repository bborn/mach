// @vitest-environment jsdom

/**
 * ⌥-click on a calendar, driven rather than read.
 *
 * `CalendarSidebar.test.tsx` renders the rail as static markup on purpose —
 * there is nothing there that needs a DOM, and a server render is a stronger
 * statement about the elements being real buttons. A modifier click is not that
 * kind of claim: `altKey` only exists on an event, so this one gets jsdom.
 *
 * What it is here to prove is a negative. Soloing must not write anything: not
 * `ui.hiddenCalendars` (which arrives as `onToggle`) and not the persisted
 * visibility map (which is `remember`, reached only from the × and from the
 * "Hidden from list" drawer). If solo ever starts unticking the other rows to
 * do its job, the five calendars he has taken out of the list are one press
 * from being reconstructed wrong, and these assertions are what stops it.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { Account, Calendar, CalendarId } from "@/types";
import type { Solo } from "@/lib/calendar-solo";
import { nextSolo } from "@/lib/calendar-solo";
import { CalendarSidebar } from "./CalendarSidebar";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

const ACCOUNT: Account = {
  id: 1,
  email: "bruno@example.com",
  name: "Bruno",
  colorIndex: 1,
  kind: "personal",
};

const CALENDARS: Calendar[] = [
  { id: "family", accountId: 1, name: "Family", colorIndex: 1, selected: true },
  { id: "work", accountId: 1, name: "Work", colorIndex: 2, selected: true },
  // Google says the subscription is gone, so the rail starts it out of the list.
  { id: "dead", accountId: 1, name: "Expired", colorIndex: 3, deleted: true },
];

let container: HTMLDivElement;
let root: Root;
let toggled: CalendarId[];
let solo: Solo | null;

function draw(hidden: CalendarId[] = []) {
  act(() => {
    root.render(
      <CalendarSidebar
        accounts={[ACCOUNT]}
        calendars={CALENDARS}
        hidden={hidden}
        colorFor={() => "#16a765"}
        dark={false}
        solo={solo}
        onToggle={(id) => toggled.push(id)}
        // The shell's `applySolo`, which is the one place the toggle is
        // decided — the rail only ever names a target.
        onSolo={(target) => {
          solo = nextSolo(solo, target);
          draw(hidden);
        }}
        settings={{ mergeDuplicates: false, showDeclined: false, showWeekends: true }}
        onSettings={() => {}}
      />,
    );
  });
}

/** The row's own press target, by its visible name. */
function row(name: string): HTMLButtonElement {
  const found = [...container.querySelectorAll("button")].find(
    (button) => button.textContent?.trim() === name,
  );
  if (!found) throw new Error(`no row for ${name}`);
  return found as HTMLButtonElement;
}

function chip(label: string): HTMLButtonElement {
  const found = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
  if (!found) throw new Error(`no button labelled ${label}`);
  return found;
}

function click(element: HTMLElement, init: MouseEventInit = {}) {
  act(() => {
    element.dispatchEvent(new MouseEvent("click", { bubbles: true, ...init }));
  });
}

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  toggled = [];
  solo = null;
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  globalThis.IS_REACT_ACT_ENVIRONMENT = false;
});

describe("⌥-click on a calendar", () => {
  it("solos it, where a plain click still shows or hides it", () => {
    draw();
    click(row("Family"));
    expect(toggled).toEqual(["family"]);
    expect(solo).toBeNull();

    click(row("Work"), { altKey: true });
    expect(solo).toEqual({ kind: "calendar", id: "work" });
    // The modifier is an accelerator, not a second write: nothing was ticked or
    // unticked to make the solo happen.
    expect(toggled).toEqual(["family"]);
  });

  it("un-solos on a second press, and still writes nothing", () => {
    draw();
    click(row("Work"), { altKey: true });
    click(row("Work"), { altKey: true });
    expect(solo).toBeNull();
    expect(toggled).toEqual([]);
  });

  it("moves the solo rather than stacking a second one", () => {
    draw();
    click(row("Family"), { altKey: true });
    click(row("Work"), { altKey: true });
    expect(solo).toEqual({ kind: "calendar", id: "work" });
    expect(toggled).toEqual([]);
  });

  it("leaves a calendar taken out of the list out of it, before and after", () => {
    // The rail's two axes are separate (see `CalendarSidebar`), and solo is
    // about neither. "Expired" is unlisted at the start of this and unlisted at
    // the end, with no `onToggle` in between to make `reconcileVisibility`
    // decide it has been asked for back.
    draw(["dead"]);
    const listing = () => container.textContent ?? "";
    expect(listing()).toContain("Hidden from list (1)");

    click(row("Family"), { altKey: true });
    expect(listing()).toContain("Hidden from list (1)");
    expect(listing()).not.toContain("Expired");

    click(row("Family"), { altKey: true });
    expect(listing()).toContain("Hidden from list (1)");
    expect(toggled).toEqual([]);
  });

  it("solos an unticked calendar without ticking it", () => {
    // Work is off. Soloing it puts its events on the grid for as long as the
    // solo lasts; what it must not do is change what the rail says, or the tick
    // would still be on when the solo ended.
    draw(["work"]);
    click(row("Work"), { altKey: true });
    expect(solo).toEqual({ kind: "calendar", id: "work" });
    expect(toggled).toEqual([]);
    expect(row("Work").getAttribute("aria-pressed")).toBe("false");
  });
});

describe("the solo chip", () => {
  it("is a real button, so the gesture is not the only way in", () => {
    draw();
    const button = chip("Show only Family");
    expect(button.tagName).toBe("BUTTON");
    // Faded until the row is hovered, and focusable throughout: `opacity`, not
    // `display`, so it stays in the tab order and in the accessibility tree.
    expect(button.className).toContain("opacity-0");
    expect(button.className).toContain("focus-visible:opacity-100");

    click(button);
    expect(solo).toEqual({ kind: "calendar", id: "family" });
    expect(toggled).toEqual([]);
  });

  it("does the same thing the ⌥-click does", () => {
    draw();
    click(chip("Show only Work"));
    const fromChip = solo;
    solo = null;
    draw();
    click(row("Work"), { altKey: true });
    expect(solo).toEqual(fromChip);
  });

  it("stays lit while it is the way back", () => {
    draw();
    click(row("Family"), { altKey: true });
    const button = chip("Show every calendar");
    expect(button.getAttribute("aria-pressed")).toBe("true");
    // No longer waiting on a hover: with one calendar soloed the return has to
    // be visible without hunting for it.
    expect(button.className).not.toContain("opacity-0");
    expect(button.getAttribute("title")).toContain("Show every calendar");

    click(button);
    expect(solo).toBeNull();
  });
});
