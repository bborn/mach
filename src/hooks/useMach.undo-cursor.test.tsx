// @vitest-environment jsdom

/**
 * Where the cursor is standing after a ⌘Z.
 *
 * "when I archive a msg, then undo, my selection is wrong. e.g. the next
 * message down is selected. that's weird. when I'm on a message (selected) and
 * I archive, then undo, i'd expect the same message selected."
 *
 * Archive moving the cursor onward is right — the conversation is gone and you
 * want to keep going — and every test here that presses `e` asserts it still
 * does. What was missing is the other half: undo put the row back and left the
 * hand beside it.
 *
 * `undo-stack.test.ts` covers the traversal with a fake list. This is the same
 * behaviour against the real one, which is the only place the awkward cases can
 * be asked at all: a mailbox the user has navigated away from, and a remembered
 * conversation the list no longer has.
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
    labelIds: ["INBOX", "CATEGORY_PERSONAL"],
    ...over,
  };
}

/** What the recorder keeps: where the cursor is, and what is ticked. */
interface Frame {
  cursor: ThreadId | null;
  ticked: ThreadId[];
  rows: ThreadId[];
  mailbox: string;
}

function latest(frames: Frame[]): Frame {
  return frames[frames.length - 1]!;
}

/** The inverse Rust hands back. Nothing in the app guesses one. */
function inverseOf(command: Command): Command | undefined {
  if (command.kind === "archive") return { kind: "unarchive", threadIds: command.threadIds };
  if (command.kind === "unarchive") return { kind: "archive", threadIds: command.threadIds };
  return undefined;
}

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function stubSource(initial: Thread[]) {
  let rows = initial;
  let listeners: (() => void)[] = [];
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
    async listThreads({ labelId }) {
      return {
        threads: rows.filter((r) =>
          labelId === "PRIMARY" || labelId === undefined
            ? r.labelIds.includes("INBOX")
            : r.labelIds.includes(labelId),
        ),
        nextCursor: null,
      };
    },
    async getThread(threadId): Promise<ThreadDetail | null> {
      const found = rows.find((r) => r.id === threadId);
      return found ? { thread: found, messages: [] } : null;
    },
    async execute(command): Promise<CommandResult> {
      commands.push(command);
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
    /** A sync pass, another window, the phone — the store changed under us. */
    elsewhere(next: Thread[]) {
      rows = next;
      for (const listener of listeners) listener();
    },
    settle() {
      for (const listener of listeners) listener();
    },
  };
}

/** `threads-changed`, coalesced over 600ms in the provider and waited out here. */
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

/** Changing mailbox is not an action; the rail dispatches it. */
function openMailbox(labelId: string) {
  (window as unknown as { dispatch: (a: { type: "label"; labelId: string }) => void }).dispatch({
    type: "label",
    labelId,
  });
}

function Probe({ onFrame }: { onFrame: (frame: Frame) => void }) {
  const { visibleThreads, ui, actions, dispatch } = useMach();
  onFrame({
    cursor: ui.threadId,
    ticked: [...ui.selection.ids],
    rows: visibleThreads.map((t) => t.id),
    mailbox: ui.labelId,
  });
  useEffect(() => {
    (window as unknown as { probe: unknown }).probe = actions;
    (window as unknown as { dispatch: unknown }).dispatch = dispatch;
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
}

const four = () => [thread(1), thread(2), thread(3), thread(4)];

describe("undo puts the cursor back", () => {
  it("archive moves the cursor on; undo brings it back to the same conversation", async () => {
    const s = stubSource(four());
    const frames: Frame[] = [];
    await mount(s.source, frames);

    await act(async () => probe().selectThread(2));
    await flush();
    expect(latest(frames).cursor).toBe(2);

    act(() => probe().archiveSelected());
    // The behaviour that is *not* changing: the row is gone and the cursor has
    // walked on to the next one, in the frame the keystroke produced.
    expect(latest(frames).cursor).toBe(3);
    expect(latest(frames).rows).not.toContain(2);
    await flush();

    act(() => void probe().undo());
    // And in the frame ⌘Z produced, the conversation is back and so is the hand.
    expect(latest(frames).rows).toContain(2);
    expect(latest(frames).cursor).toBe(2);
    await flush();
    await refetch(s);
    expect(latest(frames).cursor).toBe(2);
  });

  it("returns a group's cursor to where the hand was, with the ticks", async () => {
    const s = stubSource(four());
    const frames: Frame[] = [];
    await mount(s.source, frames);

    // Tick 2 and 3, standing on 3.
    await act(async () => probe().selectThread(2));
    await flush();
    act(() => probe().toggleAtCursor());
    await act(async () => probe().selectThread(3));
    await flush();
    act(() => probe().toggleAtCursor());
    expect(latest(frames).ticked).toEqual([2, 3]);

    act(() => probe().archiveSelected());
    expect(latest(frames).cursor).toBe(4);
    expect(latest(frames).ticked).toEqual([]);
    await flush();

    act(() => void probe().undo());
    // 2 and 3 both came back; the cursor belongs on the one the hand was on.
    expect(latest(frames).rows).toEqual([1, 2, 3, 4]);
    expect(latest(frames).cursor).toBe(3);
    expect(latest(frames).ticked).toEqual([2, 3]);
  });

  it("redo leaves the cursor where the archive left it", async () => {
    const s = stubSource(four());
    const frames: Frame[] = [];
    await mount(s.source, frames);

    await act(async () => probe().selectThread(2));
    await flush();
    act(() => probe().archiveSelected());
    await flush();
    act(() => void probe().undo());
    await flush();
    expect(latest(frames).cursor).toBe(2);

    act(() => void probe().redo());
    // Not on the conversation it just archived again — where the original
    // archive had moved the cursor to.
    expect(latest(frames).cursor).toBe(3);
    expect(latest(frames).rows).not.toContain(2);
    await flush();

    // And ⌘Z after ⇧⌘Z means the same thing it meant the first time.
    act(() => void probe().undo());
    expect(latest(frames).cursor).toBe(2);
  });
});

describe("when the remembered conversation is not there to go back to", () => {
  it("leaves the cursor alone in a mailbox it does not belong to", async () => {
    const s = stubSource([...four(), thread(9, { labelIds: ["STARRED"] })]);
    const frames: Frame[] = [];
    await mount(s.source, frames);

    await act(async () => probe().selectThread(2));
    await flush();
    act(() => probe().archiveSelected());
    await flush();

    // Off to another mailbox, which clears the cursor as it always has.
    await act(async () => openMailbox("STARRED"));
    await flush();
    await act(async () => probe().selectThread(9));
    await flush();
    expect(latest(frames).cursor).toBe(9);

    act(() => void probe().undo());
    await flush();
    // The conversation goes back to the inbox. The cursor stays in the mailbox
    // the user is actually looking at.
    expect(latest(frames).mailbox).toBe("STARRED");
    expect(latest(frames).cursor).toBe(9);
    expect(latest(frames).rows).toEqual([9]);
    expect(s.commands.map((c) => c.kind)).toEqual(["archive", "unarchive"]);
  });

  it("leaves the cursor alone when the remembered row has gone", async () => {
    const s = stubSource(four());
    const frames: Frame[] = [];
    await mount(s.source, frames);

    // Ticked 2 and 3 while standing on 4 — so the cursor is not one of the
    // conversations the undo is bringing back.
    await act(async () => probe().selectThread(2));
    await flush();
    act(() => probe().toggleAtCursor());
    await act(async () => probe().selectThread(3));
    await flush();
    act(() => probe().toggleAtCursor());
    await act(async () => probe().selectThread(4));
    await flush();

    act(() => probe().archiveSelected());
    await flush();
    expect(latest(frames).cursor).toBe(4);

    // 4 leaves the mailbox behind our back — a filter, the phone, another
    // window — and the hand moves to 1.
    s.elsewhere([thread(1)]);
    await refetch(s);
    await act(async () => probe().selectThread(1));
    await flush();

    act(() => void probe().undo());
    // 2 and 3 come back and are ticked again; the cursor is not sent to a row
    // that is not there.
    expect(latest(frames).cursor).toBe(1);
    expect(latest(frames).ticked).toEqual([2, 3]);
    expect(latest(frames).rows).toContain(2);
    await flush();
  });
});
