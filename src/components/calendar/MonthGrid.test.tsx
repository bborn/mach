// @vitest-environment jsdom

/**
 * A multi-day event is one bar, not a chip on every day it covers.
 *
 * Month view used to bucket by day and draw the same title three times, so a
 * Tuesday–Thursday trip looked like three separate events and vanished into
 * "+2 more" on the busy day. Week view already packed all-day events into
 * spanning bars; this is that packing on each week of the month.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { CalendarEvent } from "@/types";
import type { MergedEvent } from "@/lib/calendar-merge";
import { daysOfMonthGrid } from "@/lib/time";
import { MonthGrid } from "./MonthGrid";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

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

const ANCHOR = new Date(2026, 8, 1).getTime();
const DAYS = daysOfMonthGrid(ANCHOR);

function event(over: Partial<CalendarEvent> & Pick<CalendarEvent, "id" | "title">): CalendarEvent {
  return {
    calendarId: "primary",
    accountId: 1,
    start: Date.UTC(2026, 8, 22),
    end: Date.UTC(2026, 8, 25),
    allDay: true,
    attendees: [],
    ...over,
  };
}

function merged(row: CalendarEvent): MergedEvent {
  return { event: row, copies: [row] };
}

function mount(events: CalendarEvent[]) {
  act(() => {
    root.render(
      <MonthGrid
        days={DAYS}
        anchorMonth={ANCHOR}
        events={events.map(merged)}
        colorFor={() => "#4f8ef7"}
        dark={false}
        selectedId={null}
        onSelect={() => {}}
        onMove={() => {}}
      />,
    );
  });
}

function chips(id: number): HTMLElement[] {
  return [...container.querySelectorAll(`[data-event-id="${id}"]`)] as HTMLElement[];
}

describe("multi-day events", () => {
  it("offers a grab on the bar, so option-drag can copy it", () => {
    mount([event({ id: 1, title: "Shoptalk Las Vegas" })]);
    expect(chips(1)[0].className).toContain("cursor-grab");
  });

  it("draws a three-day all-day event once, not once per day", () => {
    mount([event({ id: 1, title: "Shoptalk Las Vegas" })]);
    expect(chips(1)).toHaveLength(1);
    expect(chips(1)[0].textContent).toContain("Shoptalk Las Vegas");
  });

  it("spans the bar across the days it covers", () => {
    mount([event({ id: 1, title: "Shoptalk Las Vegas" })]);
    const bar = chips(1)[0];
    // Tue–Thu of a Mon-start week: 1/7 inset, 3/7 wide.
    expect(bar.style.left).toContain("14.285");
    expect(bar.style.width).toContain("42.857");
  });

  it("splits a trip that crosses a week into one bar per week", () => {
    // Thu 24 – Mon 28: this week Thu–Sun, next week Monday.
    mount([
      event({
        id: 1,
        title: "Weekend",
        start: Date.UTC(2026, 8, 24),
        end: Date.UTC(2026, 8, 29),
      }),
    ]);
    expect(chips(1)).toHaveLength(2);
  });

  it("stacks two trips that cover the same days", () => {
    mount([
      event({ id: 1, title: "Shoptalk Las Vegas" }),
      event({ id: 2, title: "Shoptalk Las Vegas (copy)" }),
    ]);
    expect(chips(1)).toHaveLength(1);
    expect(chips(2)).toHaveLength(1);
    expect(chips(1)[0].style.top).not.toBe(chips(2)[0].style.top);
  });

  it("keeps a timed meeting as a one-day chip with its time", () => {
    const start = new Date(2026, 8, 22, 11, 0).getTime();
    mount([
      event({
        id: 2,
        title: "OL Daily Tactical",
        start,
        end: start + 30 * 60_000,
        allDay: false,
      }),
    ]);
    expect(chips(2)).toHaveLength(1);
    expect(chips(2)[0].textContent).toMatch(/11/);
    expect(chips(2)[0].textContent).toContain("OL Daily Tactical");
  });
});
