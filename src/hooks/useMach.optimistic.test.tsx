// @vitest-environment jsdom

/**
 * Every command, frame by frame.
 *
 * `useMach.test.ts` covers the reducer and `lib/projection.test.ts` the rule;
 * neither can say *when* a change reaches the screen, and that is the whole
 * question here. An end-state test passes on the slow code too — the list does
 * arrive eventually — so every assertion below is either on the frame the
 * keystroke itself produced, or on the whole recording between the keystroke
 * and the list catching up.
 *
 * The stub source is shaped like the IPC one and not like the fixture one:
 * `execute` answers without the list having been refetched, and the new rows
 * arrive later, through `threads-changed`. Those are two events the real app
 * sees hundreds of milliseconds apart.
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
import type { Thread, ThreadDetail, ThreadId } from "@/types";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

const TARGET: ThreadId = 2;

function thread(id: number, over: Partial<Thread> = {}): Thread {
  return {
    id,
    accountId: 1,
    subject: `Conversation ${id}`,
    snippet: "snippet",
    participants: [{ name: "Someone", email: "someone@example.test" }],
    timestamp: 2_000 - id,
    unread: false,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
    ...over,
  };
}

/** What the frame recorder keeps about the row under test. */
interface Frame {
  present: boolean;
  starred?: boolean;
  unread?: boolean;
  status?: string;
  tone?: string;
}

function latest(frames: Frame[]): Frame {
  return frames[frames.length - 1]!;
}

/**
 * The inverse the command layer hands back, for the two commands these tests
 * take back. Nothing is guessed in the app — this stands in for Rust.
 */
function inverseOf(command: Command): Command | undefined {
  if (command.kind === "star") {
    return { kind: "star", threadIds: command.threadIds, starred: !command.starred };
  }
  if (command.kind === "archive") {
    return { kind: "unarchive", threadIds: command.threadIds };
  }
  return undefined;
}

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * A source that separates "the command answered" from "the list caught up".
 *
 * `willBecome` is what the write does to the store; it is applied when
 * `execute` resolves, exactly as SQLite would have it, and the list goes on
 * serving the *old* rows until `emit()` stands in for the debounced
 * `threads-changed` refetch.
 */
function stubSource(initial: Thread[] = [thread(1), thread(TARGET), thread(3)]) {
  let rows = initial;
  let listeners: (() => void)[] = [];
  let pending: ((rows: Thread[]) => Thread[]) | null = null;
  let refusal: CommandResult | null = null;
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
    async listEvents() {
      return [];
    },
    async listThreads() {
      return { threads: rows, nextCursor: null };
    },
    async getThread(threadId): Promise<ThreadDetail | null> {
      const found = rows.find((r) => r.id === threadId);
      return found ? { thread: found, messages: [] } : null;
    },
    async execute(command): Promise<CommandResult> {
      commands.push(command);
      if (refusal) return refusal;
      if (pending) {
        rows = pending(rows);
        pending = null;
      }
      return {
        ok: true,
        message: "Done",
        applied: "threadIds" in command ? command.threadIds : [],
        failed: [],
        undo: inverseOf(command),
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
    commands,
    /** What the command's own write does to the store. */
    willBecome(f: (rows: Thread[]) => Thread[]) {
      pending = f;
    },
    /** Make the next command come back refused, with everything rolled back. */
    refuse(message: string) {
      refusal = {
        ok: false,
        message,
        applied: [],
        failed: [
          {
            ids: [TARGET],
            kind: "forbidden",
            message,
            retriable: false,
            rolledBack: true,
          },
        ],
      };
    },
    /** Someone else changed the store — a sync pass, the phone, another window. */
    elsewhere(next: Thread[]) {
      rows = next;
      for (const listener of listeners) listener();
    },
    /** `threads-changed`, with no new rows behind it. */
    settle() {
      for (const listener of listeners) listener();
    },
  };
}

/**
 * `threads-changed`, waited out.
 *
 * The provider coalesces that event over 600ms before it refetches, which is
 * the window the whole bug lived in. Sitting through it for real is what makes
 * these assertions about the *after* rather than about a timer that never ran.
 */
async function refetch(s: { settle: () => void }) {
  await act(async () => {
    s.settle();
    await sleep(700);
  });
  await flush();
}

function probe(): MachActions {
  return (window as unknown as { probe: MachActions }).probe;
}

function Probe({ onFrame }: { onFrame: (frame: Frame) => void }) {
  const { visibleThreads, isUnread, ui, actions } = useMach();
  const row = visibleThreads.find((t) => t.id === TARGET);
  onFrame({
    present: row !== undefined,
    starred: row?.starred,
    unread: row ? isUnread(row) : undefined,
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

/** Mount the provider against `source` and put the cursor on the row. */
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
  await act(async () => probe().selectThread(TARGET));
  await flush();
}

/**
 * The three commands that take a row out of the mailbox.
 *
 * All three used to ride on a set of hidden ids kept beside the star's own
 * override, and none of the three was ever taken back out of it. They ride on
 * the projection now, which is the same mechanism the star uses.
 */
describe("a row leaves the list on the keystroke", () => {
  const cases: { name: string; press: () => void }[] = [
    { name: "archive", press: () => probe().archiveSelected() },
    { name: "trash", press: () => probe().trashSelected() },
    { name: "snooze", press: () => probe().snoozeSelected(Date.now() + 86_400_000) },
  ];

  for (const { name, press } of cases) {
    it(`${name} — gone in the frame the keystroke produced`, async () => {
      const s = stubSource();
      s.willBecome((rows) => rows.filter((r) => r.id !== TARGET));
      const frames: Frame[] = [];
      await mount(s.source, frames);
      expect(latest(frames).present).toBe(true);

      // Synchronously. Nothing has been awaited, `run` is still in flight, and
      // the list underneath is still the one fetched before the write.
      frames.length = 0;
      act(() => press());
      expect(latest(frames).present).toBe(false);

      // The command answers. The list has still not been refetched.
      await flush();
      // And then it is, carrying the same truth.
      await refetch(s);

      // The point of the whole test: no frame in between put the row back.
      expect(frames.every((f) => !f.present)).toBe(true);
    });
  }
});

describe("the star", () => {
  it("comes on with the keystroke and stays on", async () => {
    const s = stubSource();
    s.willBecome((rows) => rows.map((r) => (r.id === TARGET ? { ...r, starred: true } : r)));
    const frames: Frame[] = [];
    await mount(s.source, frames);

    frames.length = 0;
    act(() => probe().starSelected());
    expect(latest(frames).starred).toBe(true);

    await flush();
    await refetch(s);
    expect(frames.every((f) => f.starred === true)).toBe(true);
  });

  it("goes back out with the ⌘Z, not a refetch later", async () => {
    // The half that had no projection at all: the inverse a traversal
    // dispatches went through `run`, and `run` knew nothing about stars.
    const s = stubSource([thread(1), thread(TARGET, { starred: true, labelIds: ["INBOX", "STARRED"] }), thread(3)]);
    const frames: Frame[] = [];
    await mount(s.source, frames);

    // Star it off, so there is something on the stack to take back.
    s.willBecome((rows) =>
      rows.map((r) => (r.id === TARGET ? { ...r, starred: false, labelIds: ["INBOX"] } : r)),
    );
    act(() => probe().starSelected());
    await flush();
    await refetch(s);
    expect(latest(frames).starred).toBe(false);

    // ⌘Z. The star has to be back in the frame the undo produced.
    frames.length = 0;
    s.willBecome((rows) =>
      rows.map((r) => (r.id === TARGET ? { ...r, starred: true, labelIds: ["INBOX", "STARRED"] } : r)),
    );
    await act(async () => {
      void probe().undo();
    });
    expect(frames.some((f) => f.starred === true)).toBe(true);
    const firstOn = frames.findIndex((f) => f.starred === true);
    await flush();
    await refetch(s);
    expect(frames.slice(firstOn).every((f) => f.starred !== false)).toBe(true);
  });
});

describe("opening a conversation", () => {
  it("clears the unread mark before the markRead command answers", async () => {
    const s = stubSource([thread(1), thread(TARGET, { unread: true, labelIds: ["INBOX", "UNREAD"] })]);
    const frames: Frame[] = [];
    setDataSource(s.source);
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
    expect(latest(frames).unread).toBe(true);

    frames.length = 0;
    act(() => probe().selectThread(TARGET));
    expect(latest(frames).unread).toBe(false);

    s.willBecome((rows) =>
      rows.map((r) => (r.id === TARGET ? { ...r, unread: false, labelIds: ["INBOX"] } : r)),
    );
    await flush();
    await refetch(s);
    expect(frames.every((f) => f.unread !== true)).toBe(true);
  });
});

describe("a label", () => {
  it("takes the row out of the label being viewed on the keystroke", async () => {
    // Labelling has no key of its own; a plugin action is how it is reached,
    // and the plugin host hands its inverses over as a ⌘Z group. Running that
    // group is a `label` command going through `run` like any other.
    const s = stubSource([thread(1), thread(TARGET, { labelIds: ["INBOX", "Receipts"] })]);
    const frames: Frame[] = [];
    await mount(s.source, frames);
    await act(async () => probe().openFavorite({ kind: "mailbox", labelId: "Receipts", accountId: null, name: "Receipts" }));
    await flush();
    await act(async () => probe().selectThread(TARGET));
    await flush();
    expect(latest(frames).present).toBe(true);

    frames.length = 0;
    s.willBecome((rows) =>
      rows.map((r) => (r.id === TARGET ? { ...r, labelIds: ["INBOX"] } : r)),
    );
    await act(async () => {
      probe().pushUndoGroup("Labelled 1 conversation", [
        { kind: "label", threadIds: [TARGET], labelId: "Receipts", add: false },
      ]);
      // ⌘Z on that group dispatches the `label` command itself.
      void probe().undo();
    });
    expect(frames.some((f) => !f.present)).toBe(true);
    const fromGone = frames.slice(frames.findIndex((f) => !f.present));
    expect(fromGone.every((f) => !f.present)).toBe(true);
  });
});

/**
 * A refused write, which is the rule none of this is allowed to cost.
 */
describe("when Google refuses", () => {
  it("puts the row back and says why", async () => {
    const s = stubSource();
    s.refuse("Google refused the archive");
    const frames: Frame[] = [];
    await mount(s.source, frames);

    frames.length = 0;
    act(() => probe().archiveSelected());
    expect(latest(frames).present).toBe(false);

    // The rolled-back id is the one case where the guess goes immediately:
    // the write did not happen, and the row has to say so rather than wait for
    // a refetch to contradict it.
    await flush();
    expect(latest(frames).present).toBe(true);
    expect(latest(frames).status).toContain("Google refused the archive");
    expect(latest(frames).tone).toBe("error");
  });

  it("puts the star back out too", async () => {
    const s = stubSource();
    s.refuse("Google refused the star");
    const frames: Frame[] = [];
    await mount(s.source, frames);

    frames.length = 0;
    act(() => probe().starSelected());
    expect(latest(frames).starred).toBe(true);
    await flush();
    expect(latest(frames).starred).toBe(false);
    expect(latest(frames).tone).toBe("error");
  });
});

/**
 * The half that was missing entirely: a guess has to stop being one.
 *
 * Archive, trash, snooze and mark-read wrote into sets that nothing ever
 * emptied. A conversation archived here and put back in the inbox from the
 * phone came back in SQLite, came back out of `list_threads`, and was filtered
 * out of the list by an id nobody remembered adding — for the life of the
 * process.
 */
describe("a guess is retired against the list, and only against the list", () => {
  it("lets a conversation that comes back from elsewhere come back", async () => {
    const s = stubSource();
    const frames: Frame[] = [];
    await mount(s.source, frames);

    s.willBecome((rows) => rows.filter((r) => r.id !== TARGET));
    act(() => probe().archiveSelected());
    await flush();
    await refetch(s);
    expect(latest(frames).present).toBe(false);

    // Unarchived on the phone. Sync writes it back and says so.
    await act(async () => {
      s.elsewhere([thread(1), thread(TARGET), thread(3)]);
      await sleep(700);
    });
    await flush();
    expect(latest(frames).present).toBe(true);
  });

  it("holds the guess until the list actually agrees", async () => {
    const s = stubSource();
    const frames: Frame[] = [];
    await mount(s.source, frames);

    // The command answers, but the store is not updated — a refetch that
    // arrives before the write is visible must not put the row back.
    act(() => probe().archiveSelected());
    await flush();
    await refetch(s);
    expect(latest(frames).present).toBe(false);
  });
});
