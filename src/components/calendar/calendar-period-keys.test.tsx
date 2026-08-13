// @vitest-environment jsdom

/**
 * ⇧← and ⇧→ mean two things, and the calendar picks the right one.
 *
 * The chord was already spoken for: it moves the *selected* event by a day,
 * which is the gesture Google Calendar has. What it did with nothing selected
 * was print "Pick an event first" — an error message in answer to the most
 * natural way there is to ask for next week, now that the bare arrows belong to
 * the events.
 *
 * So the nudge declines the key when there is nothing to move, and the period
 * step behind it takes it. That is a property of the registry rather than of
 * either binding, which is why this test drives the real `CalendarMode` through
 * a real `KeymapProvider` and presses a real keydown: registering the two by
 * hand in the order this file assumes would be asserting the assumption.
 */

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { KeymapProvider, useKeymap } from "@/hooks/useKeymap";
import type { Keymap } from "@/lib/keymap";
import { MachProvider, useMach, type MachActions } from "@/hooks/useMach";
import { PreferencesProvider } from "@/components/prefs/PreferencesProvider";
import {
  fixtureSource,
  setDataSource,
  type Command,
  type CommandResult,
  type MachDataSource,
} from "@/lib/data";
import type { Account, Calendar, CalendarEvent, EventId } from "@/types";
import { CalendarMode } from "./CalendarMode";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

const TARGET: EventId = 7;
const DAY = 86_400_000;
const WEEK = 7 * DAY;
/** A Wednesday, so a week either way stays inside the loaded window. */
const NOON = new Date(2026, 0, 14, 12, 0, 0, 0).getTime();
const HOUR = 3_600_000;

const ACCOUNT = { id: 1, email: "bruno@example.com", name: "Bruno" } as Account;
const CALENDAR: Calendar = { id: "primary", accountId: 1, name: "Bruno", colorIndex: 1 };

const MEETING: CalendarEvent = {
  id: TARGET,
  calendarId: "primary",
  accountId: 1,
  title: "Scope review",
  start: NOON,
  end: NOON + HOUR,
  allDay: false,
  attendees: [],
};

function source(commands: Command[]): MachDataSource {
  return {
    ...fixtureSource,
    kind: "fixture",
    async listAccounts() {
      return [ACCOUNT];
    },
    async listLabels() {
      return [];
    },
    async listCalendars() {
      return [CALENDAR];
    },
    async listThreads() {
      return { threads: [], nextCursor: null };
    },
    async listEvents() {
      return [MEETING];
    },
    async execute(command): Promise<CommandResult> {
      commands.push(command);
      return { ok: true, message: "Done", applied: [], failed: [] };
    },
    async onThreadsChanged() {
      return () => {};
    },
    async onSyncStatus() {
      return () => {};
    },
  };
}

/** What the calendar looks like from outside, one render at a time. */
interface Frame {
  anchor: number;
  eventId: EventId | null;
  start?: number;
  status?: string;
}

let frames: Frame[] = [];

function latest(): Frame {
  return frames[frames.length - 1]!;
}

interface Handle {
  actions: MachActions;
  dispatch: ReturnType<typeof useMach>["dispatch"];
  keymap: Keymap;
}

function probe(): Handle {
  return (window as unknown as { probe: Handle }).probe;
}

function Probe() {
  const { ui, visibleEvents, actions, dispatch } = useMach();
  const keymap = useKeymap();
  frames.push({
    anchor: ui.anchor,
    eventId: ui.eventId,
    start: visibleEvents.find((e) => e.id === TARGET)?.start,
    status: ui.status?.message,
  });
  useEffect(() => {
    (window as unknown as { probe: Handle }).probe = { actions, dispatch, keymap };
  });
  return null;
}

class FakeResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  frames = [];
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = FakeResizeObserver;
  Element.prototype.scrollIntoView = () => {};
  if (!window.matchMedia) {
    window.matchMedia = ((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener() {},
      removeEventListener() {},
      addListener() {},
      removeListener() {},
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia;
  }
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  setDataSource(fixtureSource);
});

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function mount(commands: Command[]) {
  setDataSource(source(commands));
  await act(async () => {
    root.render(
      <PreferencesProvider>
        <KeymapProvider>
          <MachProvider>
            <Probe />
            <CalendarMode />
          </MachProvider>
        </KeymapProvider>
      </PreferencesProvider>,
    );
  });
  await flush();
  // The store opens in mail, anchored on "now"; the fixture sits on a fixed day.
  await act(async () => {
    probe().actions.setMode("calendar");
    probe().dispatch({ type: "anchor", anchor: NOON });
  });
  await flush();
}

/** A real keydown at the window, which is where the registry listens. */
async function press(key: string, shiftKey = true) {
  await act(async () => {
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key, shiftKey, bubbles: true, cancelable: true }),
    );
  });
  await flush();
}

async function select(eventId: EventId | null) {
  await act(async () => probe().dispatch({ type: "event", eventId }));
  await flush();
}

describe("⇧← and ⇧→ on the calendar", () => {
  it("moves the selected event a day, and leaves the range where it was", async () => {
    const commands: Command[] = [];
    await mount(commands);
    await select(TARGET);
    const anchor = latest().anchor;

    await press("ArrowRight");

    expect(commands).toEqual([
      {
        kind: "updateEvent",
        eventId: TARGET,
        patch: { startTs: NOON + DAY, endTs: NOON + HOUR + DAY, isAllDay: false },
        scope: "this",
      },
    ]);
    expect(latest().start).toBe(NOON + DAY);
    expect(latest().anchor).toBe(anchor);
  });

  it("moves it back on ⇧←", async () => {
    const commands: Command[] = [];
    await mount(commands);
    await select(TARGET);
    const anchor = latest().anchor;

    await press("ArrowLeft");

    expect(commands).toHaveLength(1);
    expect(latest().start).toBe(NOON - DAY);
    expect(latest().anchor).toBe(anchor);
  });

  it("steps the period forward with nothing selected, and writes nothing", async () => {
    const commands: Command[] = [];
    await mount(commands);
    await select(null);
    const anchor = latest().anchor;

    await press("ArrowRight");

    expect(latest().anchor).toBe(anchor + WEEK);
    expect(commands).toEqual([]);
    // Not the "Pick an event first" the nudge used to answer with.
    expect(latest().status ?? "").not.toContain("Pick an event");
  });

  it("steps the period back on ⇧←", async () => {
    const commands: Command[] = [];
    await mount(commands);
    await select(null);
    const anchor = latest().anchor;

    await press("ArrowLeft");

    expect(latest().anchor).toBe(anchor - WEEK);
    expect(commands).toEqual([]);
  });

  it("goes back to moving the event the moment one is selected again", async () => {
    const commands: Command[] = [];
    await mount(commands);

    await select(null);
    await press("ArrowRight");
    expect(commands).toEqual([]);

    await select(TARGET);
    await press("ArrowRight");
    expect(commands).toHaveLength(1);
  });

  it("reports no tie over the chord — the precedence is stated, not inherited", async () => {
    // Two live bindings on one key at the *same* priority is what `conflicts()`
    // calls a conflict, and in development it warns about one on every
    // keypress. These two differ on purpose, so it has nothing to say.
    await mount([]);
    await select(TARGET);
    expect(probe().keymap.conflicts()).toEqual([]);
  });

  it("prints both meanings on the shortcut sheet", async () => {
    await mount([]);
    const rows = probe()
      .keymap.active()
      .filter((b) => b.keys === "shift+right" && b.description)
      .map((b) => `${b.group}: ${b.description}`);
    expect(rows.sort()).toEqual(["Calendar: Next period", "Event: Move to the next day"]);
  });

  it("still says 'Pick an event first' for the nudges with no competitor", async () => {
    // ⇧↑/⇧↓ and the ⌥ resizes are unchanged: nothing else wants those keys, so
    // declining would leave the press doing nothing at all.
    const commands: Command[] = [];
    await mount(commands);
    await select(null);

    await press("ArrowDown");

    expect(latest().status).toContain("Pick an event first");
    expect(commands).toEqual([]);
  });
});
