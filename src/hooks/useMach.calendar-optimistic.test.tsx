// @vitest-environment jsdom

/**
 * Every calendar command, frame by frame.
 *
 * The twin of `useMach.optimistic.test.tsx`, and it exists because the calendar
 * had none of what that file pins down. `rsvp`, `createEvent`, `updateEvent`,
 * `deleteEvent` and `moveEvent` all went out through a write path of their own
 * that executed against the data source directly, so the grid did not move
 * until Google had answered and the event window had been refetched. Answering
 * "Going" from the right-click menu changed nothing on screen at all.
 *
 * So the assertion that matters here is not "the block ends up in the right
 * place" — it did, eventually, on the slow code too. It is **before the command
 * has answered**: `execute` in this file hangs until the test says otherwise,
 * so anything asserted while it is out is something the user would have seen in
 * the frame the gesture produced.
 */

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { MachProvider, useMach, type MachActions } from "@/hooks/useMach";
import {
  fixtureSource,
  setDataSource,
  type Command,
  type CommandResult,
  type MachDataSource,
} from "@/lib/data";
import type { CalendarEvent, EventId } from "@/types";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

const TARGET: EventId = 7;
const NOON = Date.UTC(2026, 0, 15, 12, 0, 0);
const HOUR = 3_600_000;

function event(id: EventId, over: Partial<CalendarEvent> = {}): CalendarEvent {
  return {
    id,
    calendarId: "primary",
    accountId: 1,
    title: `Event ${id}`,
    start: NOON,
    end: NOON + HOUR,
    allDay: false,
    attendees: [],
    rsvp: "needsAction",
    guests: [
      { email: "me@example.test", isSelf: true, response: "needsAction" },
      { email: "them@example.test", organizer: true, response: "accepted" },
    ],
    ...over,
  };
}

/** What the recorder keeps about the grid on each render. */
interface Frame {
  ids: EventId[];
  rsvp?: string;
  start?: number;
  calendarId?: string;
  /** Every title on the grid, so a placeholder can be found by name. */
  titles: string[];
  status?: string;
  tone?: string;
}

function latest(frames: Frame[]): Frame {
  return frames[frames.length - 1]!;
}

function ok(over: Partial<CommandResult> = {}): CommandResult {
  return { ok: true, message: "Done", applied: [], failed: [], ...over };
}

function refused(message: string, ids: EventId[]): CommandResult {
  return {
    ok: false,
    message,
    applied: [],
    failed: [{ ids, kind: "forbidden", message, retriable: false, rolledBack: true }],
  };
}

/**
 * A source whose `execute` does not answer until the test lets it.
 *
 * The whole point: everything asserted between the call and `answer()` is a
 * frame the user would have been looking at while Google was still thinking.
 * `willBecome` is what the command's own write does to SQLite, applied when the
 * command answers — the store then goes on serving the old rows until
 * `reloadEvents` refetches, exactly as the real one does.
 */
function stubSource(initial: CalendarEvent[] = [event(TARGET), event(8)]) {
  let rows = initial;
  let release: ((result: CommandResult) => void) | null = null;
  let pending: ((rows: CalendarEvent[]) => CalendarEvent[]) | null = null;
  const commands: Command[] = [];

  const source: MachDataSource = {
    ...fixtureSource,
    kind: "fixture",
    async listAccounts() {
      return [];
    },
    async listLabels() {
      return [];
    },
    async listCalendars() {
      return [];
    },
    async listThreads() {
      return { threads: [], nextCursor: null };
    },
    async listEvents() {
      return rows;
    },
    execute(command): Promise<CommandResult> {
      commands.push(command);
      return new Promise((resolve) => {
        release = (result) => {
          if (pending) {
            rows = pending(rows);
            pending = null;
          }
          resolve(result);
        };
      });
    },
    async onThreadsChanged() {
      return () => {};
    },
    async onSyncStatus() {
      return () => {};
    },
  };

  return {
    source,
    commands,
    /** The command's own write to the store, applied when it answers. */
    willBecome(f: (rows: CalendarEvent[]) => CalendarEvent[]) {
      pending = f;
    },
    /** Let the command in flight answer. */
    async answer(result: CommandResult = ok()) {
      const go = release;
      release = null;
      await act(async () => {
        go?.(result);
        await flush();
      });
      await flush();
    },
  };
}

function probe(): MachActions {
  return (window as unknown as { probe: MachActions }).probe;
}

function Probe({ onFrame }: { onFrame: (frame: Frame) => void }) {
  const { visibleEvents, ui, actions } = useMach();
  const row = visibleEvents.find((e) => e.id === TARGET);
  onFrame({
    ids: visibleEvents.map((e) => e.id),
    rsvp: row?.rsvp,
    start: row?.start,
    calendarId: row?.calendarId,
    titles: visibleEvents.map((e) => e.title),
    status: ui.status?.message,
    tone: ui.status?.tone,
  });
  useEffect(() => {
    (window as unknown as { probe: unknown }).probe = actions;
  });
  return null;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
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

async function mount(source: MachDataSource, frames: Frame[]) {
  setDataSource(source);
  await act(async () => {
    root.render(
      <KeymapProvider>
        <MachProvider>
          <Probe onFrame={(frame) => frames.push(frame)} />
        </MachProvider>
      </KeymapProvider>,
    );
  });
  await flush();
  // The anchor is "now", and the fixtures sit on a fixed day. Put the grid on
  // them so `visibleEvents` is about the rows under test.
  await act(async () => probe().goToday());
  await flush();
}

/** Dispatch a command and stop, with the command still out. */
function dispatch(command: Command): void {
  act(() => {
    void probe().execute(command);
  });
}

describe("answering an invitation", () => {
  it("shows the answer before the command has come back", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);
    expect(latest(frames).rsvp).toBe("needsAction");

    dispatch({ kind: "rsvp", eventId: TARGET, response: "accepted" });

    // Nothing has answered. This is the frame the menu item produced.
    expect(latest(frames).rsvp).toBe("accepted");
    expect(s.commands).toEqual([{ kind: "rsvp", eventId: TARGET, response: "accepted" }]);

    s.willBecome((rows) =>
      rows.map((row) => (row.id === TARGET ? { ...row, rsvp: "accepted" as const } : row)),
    );
    await s.answer();
    expect(latest(frames).rsvp).toBe("accepted");
  });

  it("puts the answer back when Google refuses it, and says why", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);

    dispatch({ kind: "rsvp", eventId: TARGET, response: "declined" });
    expect(latest(frames).rsvp).toBe("declined");

    await s.answer(refused("Google refused", [TARGET]));

    expect(latest(frames).rsvp).toBe("needsAction");
    expect(latest(frames).tone).toBe("error");
    expect(latest(frames).status).toContain("Google refused");
  });

  it("never leaves the grid saying something the store did not say", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);

    dispatch({ kind: "rsvp", eventId: TARGET, response: "tentative" });
    s.willBecome((rows) =>
      rows.map((row) => (row.id === TARGET ? { ...row, rsvp: "tentative" as const } : row)),
    );
    await s.answer();

    // The whole recording after the gesture: never back to `needsAction`, not
    // for one frame between the command answering and the refetch landing.
    const after = frames.slice(frames.findIndex((f) => f.rsvp === "tentative"));
    expect(after.every((f) => f.rsvp === "tentative")).toBe(true);
  });
});

describe("deleting an event", () => {
  it("takes the block off the grid in the same frame", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);
    expect(latest(frames).ids).toContain(TARGET);

    dispatch({ kind: "deleteEvent", eventId: TARGET, scope: "this" });
    expect(latest(frames).ids).not.toContain(TARGET);
    // And only that one.
    expect(latest(frames).ids).toEqual([8]);

    s.willBecome((rows) => rows.filter((row) => row.id !== TARGET));
    await s.answer();
    expect(latest(frames).ids).toEqual([8]);
  });

  it("puts it back when the delete is refused", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);

    dispatch({ kind: "deleteEvent", eventId: TARGET, scope: "this" });
    expect(latest(frames).ids).not.toContain(TARGET);

    await s.answer(refused("the account needs re-authorizing", [TARGET]));
    expect(latest(frames).ids).toContain(TARGET);
    expect(latest(frames).status).toContain("re-authorizing");
    expect(latest(frames).tone).toBe("error");
  });
});

describe("moving an event", () => {
  it("draws it at the new time before the command has answered", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);
    expect(latest(frames).start).toBe(NOON);

    dispatch({
      kind: "updateEvent",
      eventId: TARGET,
      patch: { startTs: NOON + HOUR, endTs: NOON + 2 * HOUR, isAllDay: false },
      scope: "this",
    });
    expect(latest(frames).start).toBe(NOON + HOUR);

    s.willBecome((rows) =>
      rows.map((row) =>
        row.id === TARGET ? { ...row, start: NOON + HOUR, end: NOON + 2 * HOUR } : row,
      ),
    );
    await s.answer();
    expect(latest(frames).start).toBe(NOON + HOUR);
  });

  it("snaps back visibly when the write is refused", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);

    dispatch({
      kind: "updateEvent",
      eventId: TARGET,
      patch: { startTs: NOON + HOUR, endTs: NOON + 2 * HOUR, isAllDay: false },
      scope: "this",
    });
    expect(latest(frames).start).toBe(NOON + HOUR);

    await s.answer(refused("Google had an error", [TARGET]));
    expect(latest(frames).start).toBe(NOON);
    expect(latest(frames).tone).toBe("error");
  });

  it("re-points it at the destination calendar at once", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);
    expect(latest(frames).calendarId).toBe("primary");

    dispatch({ kind: "moveEvent", eventId: TARGET, accountId: 2, calendarId: "work" });
    expect(latest(frames).calendarId).toBe("work");

    await s.answer(refused("Google refused", [TARGET]));
    expect(latest(frames).calendarId).toBe("primary");
  });
});

describe("creating an event", () => {
  const draft = {
    title: "Standup",
    startTs: NOON + 2 * HOUR,
    endTs: NOON + 3 * HOUR,
    isAllDay: false,
    attendees: [],
    recurrence: [],
  };

  it("draws the block before the event exists", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);
    expect(latest(frames).titles).not.toContain("Standup");

    dispatch({ kind: "createEvent", accountId: 1, calendarId: "primary", draft });
    expect(latest(frames).titles).toContain("Standup");
    // Under an id nothing on Google could have, so no verb can address it.
    expect(latest(frames).ids.some((id) => id < 0)).toBe(true);
  });

  it("hands the block over to the real row without a gap", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);

    dispatch({ kind: "createEvent", accountId: 1, calendarId: "primary", draft });
    s.willBecome((rows) => [
      ...rows,
      event(42, { title: "Standup", start: draft.startTs, end: draft.endTs }),
    ]);
    await s.answer(ok({ applied: [42] }));

    const after = frames.slice(frames.findIndex((f) => f.titles.includes("Standup")));
    // Never once absent between the gesture and the row landing, and never two
    // copies of it either.
    expect(after.every((f) => f.titles.filter((t) => t === "Standup").length === 1)).toBe(true);
    expect(latest(frames).ids).toContain(42);
    expect(latest(frames).ids.every((id) => id > 0)).toBe(true);
  });

  it("takes the block away again when the create is refused", async () => {
    const frames: Frame[] = [];
    const s = stubSource();
    await mount(s.source, frames);

    dispatch({ kind: "createEvent", accountId: 1, calendarId: "primary", draft });
    expect(latest(frames).titles).toContain("Standup");

    await s.answer(refused("No calendar to create on", []));
    expect(latest(frames).titles).not.toContain("Standup");
    expect(latest(frames).ids).toEqual([TARGET, 8]);
    expect(latest(frames).tone).toBe("error");
  });
});
