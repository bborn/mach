// @vitest-environment jsdom

/**
 * Deleting drafts from the list, one row or forty.
 *
 * The bug: four drafts selected in the Drafts mailbox, delete pressed, nothing
 * deleted, and a red toast reading "4 failed — thread has no locally known
 * Gmail message ids; sync it before acting on it". A draft has no message id
 * `messages.batchModify` will take, so the label-delta engine refused it —
 * correctly, for the path it was on, and it was the wrong path.
 *
 * The routing is in Rust (`commands::drafts`), which is the only side that
 * knows which conversations are holding a draft and which of those also hold
 * sent mail. What this file pins is the frontend's half of the contract:
 *
 *  * **one command per gesture**, naming every selected row, drafts included —
 *    a selection split into two dispatches would be two round trips, two
 *    status messages and two entries on the undo stack for one keystroke;
 *  * **the summary is the one the command layer wrote**, counting what actually
 *    happened rather than what was asked for;
 *  * **⌘Z is offered for exactly what it can do.** `drafts.delete` is
 *    permanent, so a mixed delete puts the conversations on the stack under a
 *    label that does not mention the drafts.
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
import { describeUndo, type UndoState } from "@/lib/undo-stack";
import type { Thread, ThreadId } from "@/types";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

/** Two ordinary conversations and two drafts, as the Drafts mailbox lists them. */
const CONVERSATION: ThreadId = 1;
const REPLYING: ThreadId = 2;
const DRAFT_ONLY: ThreadId = 3;
const OTHER_DRAFT: ThreadId = 4;

function thread(id: number, labelIds: string[]): Thread {
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
    labelIds,
    ...{},
  };
}

const ROWS: Thread[] = [
  thread(CONVERSATION, ["INBOX"]),
  thread(REPLYING, ["INBOX", "DRAFT"]),
  thread(DRAFT_ONLY, ["DRAFT"]),
  thread(OTHER_DRAFT, ["DRAFT"]),
];

/**
 * A source that answers the way `commands::mail` does once the drafts pass has
 * run: the message counts the two things separately, and `undoLabel` names only
 * the half the inverse covers.
 */
function stubSource(rows: Thread[]) {
  const commands: Command[] = [];
  let answer: (command: Command) => CommandResult = (command) => ({
    ok: true,
    message: "Done",
    applied: "threadIds" in command ? command.threadIds : [],
    failed: [],
  });

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
      commands.push(command);
      return answer(command);
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
    answers(next: (command: Command) => CommandResult) {
      answer = next;
    },
  };
}

interface Frame {
  rows: ThreadId[];
  status?: string;
  tone?: string;
  undo: UndoState;
}

function probe(): MachActions {
  return (window as unknown as { probe: MachActions }).probe;
}

function setMailbox(labelId: string) {
  (window as unknown as { setMailbox: (id: string) => void }).setMailbox(labelId);
}

function Probe({ onFrame }: { onFrame: (frame: Frame) => void }) {
  const { visibleThreads, ui, actions, undoState, dispatch } = useMach();
  onFrame({
    rows: visibleThreads.map((t) => t.id),
    status: ui.status?.message,
    tone: ui.status?.tone,
    undo: undoState,
  });
  useEffect(() => {
    (window as unknown as { probe: unknown }).probe = actions;
    (window as unknown as { setMailbox: unknown }).setMailbox = (labelId: string) =>
      dispatch({ type: "label", labelId });
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

/** Tick these rows, the way `x` does. */
async function select(ids: ThreadId[]) {
  for (const id of ids) {
    await act(async () => probe().clickThread(id, { extend: false, toggle: true }));
  }
  await flush();
}

function latest(frames: Frame[]): Frame {
  return frames[frames.length - 1]!;
}

describe("deleting a selection that holds drafts", () => {
  it("dispatches one trash naming every selected row, drafts included", async () => {
    const s = stubSource(ROWS);
    const frames: Frame[] = [];
    await mount(s.source, frames);
    await act(async () => setMailbox("DRAFT"));
    await flush();

    await select([DRAFT_ONLY, OTHER_DRAFT, REPLYING]);
    await act(async () => probe().trashSelected());
    await flush();

    expect(s.commands).toHaveLength(1);
    expect(s.commands[0]).toEqual({
      kind: "trash",
      threadIds: [DRAFT_ONLY, OTHER_DRAFT, REPLYING],
    });
  });

  it("takes the draft rows out of the Drafts mailbox in the frame the keystroke produced", async () => {
    const s = stubSource(ROWS);
    const frames: Frame[] = [];
    await mount(s.source, frames);
    await act(async () => setMailbox("DRAFT"));
    await flush();
    // The stub does not filter by mailbox — the store does that — so the list
    // is everything. What matters is which rows the guess takes out of it.
    expect(latest(frames).rows).toContain(DRAFT_ONLY);

    await select([DRAFT_ONLY, OTHER_DRAFT]);
    frames.length = 0;
    // Synchronously: nothing has been awaited and the list underneath is still
    // the one fetched before the write.
    act(() => probe().trashSelected());
    expect(latest(frames).rows).not.toContain(DRAFT_ONLY);
    expect(latest(frames).rows).not.toContain(OTHER_DRAFT);
    // A conversation that is not being deleted stays exactly where it was.
    expect(latest(frames).rows).toContain(REPLYING);

    await flush();
    expect(frames.every((f) => !f.rows.includes(DRAFT_ONLY))).toBe(true);
  });

  it("shows the summary the command layer wrote, counting both halves", async () => {
    const s = stubSource(ROWS);
    s.answers((command) => ({
      ok: true,
      message: "Trashed 1 conversation · discarded 2 drafts",
      undoLabel: "Trashed 1 conversation",
      applied: "threadIds" in command ? command.threadIds : [],
      failed: [],
      undo: { kind: "untrash", threadIds: [REPLYING] },
    }));
    const frames: Frame[] = [];
    await mount(s.source, frames);
    await act(async () => setMailbox("DRAFT"));
    await flush();

    await select([REPLYING, DRAFT_ONLY, OTHER_DRAFT]);
    await act(async () => probe().trashSelected());
    await flush();

    const frame = latest(frames);
    expect(frame.status).toBe("Trashed 1 conversation · discarded 2 drafts");
    expect(frame.tone).toBe("info");

    // ⌘Z restores the conversation and cannot restore the drafts, so it offers
    // only the conversation. Reusing the status line here would be a button
    // claiming to bring a deleted draft back.
    expect(describeUndo(frame.undo.done[0] ?? null)).toBe("Undo trashed 1 conversation");
  });

  it("says which rows failed, and leaves them selected", async () => {
    const s = stubSource(ROWS);
    s.answers(() => ({
      ok: false,
      message: "Discarded 1 draft",
      applied: [DRAFT_ONLY],
      failed: [
        {
          ids: [OTHER_DRAFT],
          kind: "forbidden",
          message: "the draft is still at Gmail: google forbidden (403)",
          retriable: false,
          rolledBack: false,
        },
      ],
    }));
    const frames: Frame[] = [];
    await mount(s.source, frames);
    await act(async () => setMailbox("DRAFT"));
    await flush();

    await select([DRAFT_ONLY, OTHER_DRAFT]);
    await act(async () => probe().trashSelected());
    await flush();

    const frame = latest(frames);
    expect(frame.tone).toBe("error");
    expect(frame.status).toContain("Discarded 1 draft");
    expect(frame.status).toContain("1 failed");
    expect(frame.status).toContain("the draft is still at Gmail");
    // The one that did not go is back on screen, and still ticked, so the same
    // keystroke retries it.
    expect(frame.rows).toContain(OTHER_DRAFT);
    expect(frame.rows).not.toContain(DRAFT_ONLY);
  });

  it("still offers a plain undo when no draft was involved", async () => {
    const s = stubSource(ROWS);
    s.answers((command) => ({
      ok: true,
      message: "Trashed 1 conversation",
      applied: "threadIds" in command ? command.threadIds : [],
      failed: [],
      undo: { kind: "untrash", threadIds: [CONVERSATION] },
    }));
    const frames: Frame[] = [];
    await mount(s.source, frames);

    await select([CONVERSATION]);
    await act(async () => probe().trashSelected());
    await flush();

    expect(describeUndo(latest(frames).undo.done[0] ?? null)).toBe(
      "Undo trashed 1 conversation",
    );
  });
});
