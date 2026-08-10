// @vitest-environment jsdom

/**
 * The star, frame by frame.
 *
 * `useMach.test.ts` covers the reducer, which is the easy half: it can say that
 * an override is recorded and that it can be dropped, and it cannot say *when*
 * dropping it is correct. That is the whole of this bug — the star appeared on
 * the keystroke, went out again the moment `execute_command` answered, and came
 * back when the debounced `threads-changed` refetch landed most of a second
 * later. Reported from inside the app as "starring a msg flashes the star
 * before it sticks".
 *
 * So this mounts the provider for real, against a data source that behaves the
 * way the IPC one does: `execute` resolves *without* the list having been
 * refetched, and the new rows only arrive later, through `threads-changed`.
 * Every render is recorded, and the assertion is on the whole recording rather
 * than on the end state — an end-state test passed before the fix too.
 */

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { MachProvider, useMach, type MachActions } from "@/hooks/useMach";
import {
  fixtureSource,
  setDataSource,
  type CommandResult,
  type MachDataSource,
} from "@/lib/data";
import type { Thread, ThreadId } from "@/types";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

/** The frame on screen right now. `Array.prototype.at` is above this target. */
function latest(frames: (boolean | undefined)[]): boolean | undefined {
  return frames[frames.length - 1];
}

const STARRED_ID: ThreadId = 1;

function thread(over: Partial<Thread> = {}): Thread {
  return {
    id: STARRED_ID,
    accountId: 1,
    subject: "Invoice #51",
    snippet: "Amount due",
    participants: [{ name: "Whiny Nil Bookkeeper", email: "books@example.test" }],
    timestamp: 1_000,
    unread: false,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
    ...over,
  };
}

/**
 * A source that separates "the command answered" from "the list caught up".
 *
 * The fixture source cannot show this bug: it never changes its rows, so a
 * refetch after a star returns the same unstarred row forever and the override
 * is the only thing holding the star up. The real one writes SQLite before it
 * answers and then emits `threads-changed`, which is two events the frontend
 * sees hundreds of milliseconds apart. This has both, and the test drives them
 * separately.
 */
function stubSource() {
  let rows: Thread[] = [thread()];
  let listeners: (() => void)[] = [];
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
    async listEvents() {
      return [];
    },
    async listThreads() {
      return { threads: rows, nextCursor: null };
    },
    async getThread() {
      return null;
    },
    async execute(command): Promise<CommandResult> {
      // Answers, and says nothing about the list. That is exactly what the IPC
      // command does: the write has landed in SQLite, and the frontend's copy
      // of the row is whatever the last `list_threads` returned.
      return {
        ok: true,
        message: "Starred",
        applied: "threadIds" in command ? command.threadIds : [],
        failed: [],
      };
    },
    async onThreadsChanged(handler) {
      listeners.push(handler);
      return () => {
        listeners = listeners.filter((l) => l !== handler);
      };
    },
    async onSyncStatus() {
      return () => {};
    },
  };
  return {
    source,
    /** What a sync pass or a command's own write eventually puts on the wire. */
    settle(next: Thread[]) {
      rows = next;
      for (const listener of listeners) listener();
    },
  };
}

/**
 * The actions as of the *latest* render.
 *
 * Read fresh every time rather than captured once: `starSelected` closes over
 * the ids the command will act on, so a copy taken before the cursor moved
 * would star nothing at all.
 */
function probe(): MachActions {
  return (window as unknown as { probe: MachActions }).probe;
}

/** Records the star on every render, which is the thing under test. */
function Probe({ onFrame }: { onFrame: (starred: boolean | undefined) => void }) {
  const { visibleThreads, actions } = useMach();
  const row = visibleThreads.find((t) => t.id === STARRED_ID);
  onFrame(row?.starred);
  // Published so the test can press the key without a keymap round trip.
  useEffect(() => {
    (window as unknown as { probe: unknown }).probe = actions;
  });
  return null;
}

function Tree({ onFrame }: { onFrame: (starred: boolean | undefined) => void }) {
  return (
    <KeymapProvider>
      <MachProvider>
        <Probe onFrame={onFrame} />
      </MachProvider>
    </KeymapProvider>
  );
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  // jsdom has no `matchMedia`, and the provider reads it to resolve the theme.
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

/** Let every pending promise and effect settle. */
async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("the star does not flash", () => {
  it("stays on from the keystroke through the refetch", async () => {
    const { source, settle } = stubSource();
    setDataSource(source);

    const frames: (boolean | undefined)[] = [];
    await act(async () => {
      root.render(<Tree onFrame={(starred) => frames.push(starred)} />);
    });
    await flush();

    // Put the cursor on the row, so `s` has something to act on.
    await act(async () => probe().selectThread(STARRED_ID));
    await flush();
    expect(latest(frames)).toBe(false);

    // The keystroke. Synchronously, so this is the frame the user sees before
    // anything has been awaited — `run` is still in flight.
    frames.length = 0;
    act(() => probe().starSelected());
    expect(latest(frames)).toBe(true);

    // `execute` answers. Before the fix the override was dropped here, and the
    // row fell back to the list's copy — which is still the unstarred one,
    // because nothing has refetched yet.
    await flush();
    expect(latest(frames)).toBe(true);

    // The refetch finally lands, carrying the star.
    await act(async () => settle([thread({ starred: true })]));
    await flush();
    expect(latest(frames)).toBe(true);

    // The point of the whole test: no frame in between showed it off.
    expect(frames).not.toContain(false);
    expect(frames).not.toContain(undefined);
  });

  it("puts the star back out when Gmail refuses the write", async () => {
    const { source } = stubSource();
    source.execute = async (command) => ({
      ok: false,
      message: "Google refused",
      applied: [],
      failed: [
        {
          ids: "threadIds" in command ? command.threadIds : [],
          kind: "forbidden",
          message: "Google refused",
          retriable: false,
          rolledBack: true,
        },
      ],
    });
    setDataSource(source);

    const frames: (boolean | undefined)[] = [];
    await act(async () => {
      root.render(<Tree onFrame={(starred) => frames.push(starred)} />);
    });
    await flush();

    await act(async () => probe().selectThread(STARRED_ID));
    await flush();

    act(() => probe().starSelected());
    expect(latest(frames)).toBe(true);

    // A rollback is not a flash: the write did not happen, so the star has to
    // go back out and say so.
    await flush();
    expect(latest(frames)).toBe(false);
  });
});
