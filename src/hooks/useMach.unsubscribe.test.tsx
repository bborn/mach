// @vitest-environment jsdom

/**
 * One gesture, three different things it can honestly be.
 *
 * `unsubscribeAction` decides *which* message the app would write to, and its
 * own test covers that. This covers what happens next, which is the part with
 * consequences: an unsubscribe confirms to a stranger that the address is read,
 * so the branch that refuses to send one has to be the branch that runs, and a
 * request the sender refused has to end up on screen rather than in a swallowed
 * promise.
 */

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import {
  MachProvider,
  useMach,
  type MachActions,
  type StatusMessage,
} from "@/hooks/useMach";
import {
  fixtureSource,
  inverseOf,
  setDataSource,
  type Command,
  type CommandResult,
  type MachDataSource,
} from "@/lib/data";
import { describeUndo, peekUndo, type UndoState } from "@/lib/undo-stack";
import type { Message, Thread, UnsubscribeOffer } from "@/types";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

const THREAD_ID = 1;
const MESSAGE_ID = 512;

function thread(over: Partial<Thread> = {}): Thread {
  return {
    id: THREAD_ID,
    accountId: 1,
    subject: "Bookkeeping digest — Monday 10 August",
    snippet: "Three things need you; the rest is filed.",
    participants: [{ name: "Whiny Nil", email: "books@whinynil.example" }],
    timestamp: 1_000,
    unread: false,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
    ...over,
  };
}

function message(unsubscribe?: UnsubscribeOffer): Message {
  return {
    id: MESSAGE_ID,
    threadId: THREAD_ID,
    accountId: 1,
    from: { name: "Whiny Nil", email: "books@whinynil.example" },
    to: [],
    cc: [],
    timestamp: 1_000,
    bodyText: "Three things need you",
    snippet: "Three things need you",
    attachments: [],
    isDraft: false,
    unsubscribe,
  };
}

/** A source that records commands and can be told to refuse the unsubscribe. */
function stubSource(options: { offer?: UnsubscribeOffer; refuse?: boolean; rows?: Thread[] } = {}) {
  const commands: Command[] = [];
  const opened: number[] = [];
  const rows = options.rows ?? [thread()];

  const source: MachDataSource = {
    ...fixtureSource,
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
      return { thread: rows[0]!, messages: [message(options.offer)] };
    },
    async execute(command): Promise<CommandResult> {
      commands.push(command);
      if (command.kind === "unsubscribe" && options.refuse) {
        return {
          ok: false,
          message: "Unsubscribe refused",
          applied: [],
          failed: [
            {
              ids: [],
              kind: "server",
              message: "the list server returned 503",
              retriable: true,
              rolledBack: false,
            },
          ],
        };
      }
      return {
        ok: true,
        message: "Done",
        // As the command layer answers: an inverse for the archive, and none
        // for the unsubscribe. The undo stack records only the first.
        undo: inverseOf(command),
        applied: "threadIds" in command ? command.threadIds : [],
        failed: [],
      };
    },
    async openUnsubscribePage(messageId) {
      opened.push(messageId);
    },
    async onThreadsChanged() {
      return () => {};
    },
    async onSyncStatus() {
      return () => {};
    },
  };

  return { source, commands, opened };
}

function probe(): MachActions {
  return (window as unknown as { probe: MachActions }).probe;
}

function status(): StatusMessage | null {
  return (window as unknown as { status: StatusMessage | null }).status;
}

/** What ⌘Z would say it is about to do. */
function undoOffer(): string | null {
  return describeUndo(peekUndo((window as unknown as { undoState: UndoState }).undoState));
}

function Probe() {
  const { actions, ui, undoState } = useMach();
  useEffect(() => {
    (window as unknown as { probe: unknown }).probe = actions;
    (window as unknown as { status: unknown }).status = ui.status;
    (window as unknown as { undoState: unknown }).undoState = undoState;
  });
  return null;
}

function Tree() {
  return (
    <KeymapProvider>
      <MachProvider>
        <Probe />
      </MachProvider>
    </KeymapProvider>
  );
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
  vi.useRealTimers();
});

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

/** Mount, open the conversation, and hand back the recorded commands. */
async function open(stub: ReturnType<typeof stubSource>) {
  setDataSource(stub.source);
  await act(async () => {
    root.render(<Tree />);
  });
  await flush();
  await act(async () => probe().selectThread(THREAD_ID));
  await flush();
  // Opening marks it read, and that is not what any of these tests are about.
  stub.commands.length = 0;
}

describe("unsubscribe", () => {
  it("reports spam instead, and never writes to the sender", async () => {
    const stub = stubSource({ offer: { offer: "reportSpam", reason: "unknownSender" } });
    await open(stub);

    await act(async () => probe().unsubscribe());
    await flush();

    expect(stub.commands).toEqual([{ kind: "reportSpam", threadIds: [THREAD_ID] }]);
    expect(stub.opened).toEqual([]);
  });

  /*
   * A link is a page with a form on it, and Rust will not fill one in. The
   * command must not be dispatched at all: dispatching it and having Rust
   * decline would put a failure on screen for a case that is working exactly as
   * intended.
   */
  it("opens the page for a link, and dispatches nothing", async () => {
    const stub = stubSource({ offer: { offer: "unsubscribe", method: "link" } });
    await open(stub);

    await act(async () => probe().unsubscribe());
    await flush();

    expect(stub.commands).toEqual([]);
    expect(stub.opened).toEqual([MESSAGE_ID]);
  });

  it("archives the conversation and asks the sender to stop", async () => {
    const stub = stubSource({ offer: { offer: "unsubscribe", method: "oneClick" } });
    await open(stub);

    await act(async () => probe().unsubscribe());
    await flush();

    // The archive first, because it is the acknowledgement — the row is gone
    // before the sender has been contacted at all.
    expect(stub.commands).toEqual([
      { kind: "archive", threadIds: [THREAD_ID] },
      { kind: "unsubscribe", messageId: MESSAGE_ID },
    ]);
    expect(status()?.message).toBe("Unsubscribed from Whiny Nil");
    // Nothing can take it back, so nothing is offered.
    expect(status()?.undo).toBeUndefined();
    expect(status()?.tone).toBe("info");

    /*
     * ⌘Z names the archive, not the gesture.
     *
     * The toast said "Unsubscribing from Whiny Nil…" while the request was in
     * flight, and if the undo entry inherited that label the button beside it
     * would offer to undo an unsubscribe — which nothing in the app, or on the
     * sender's server, can do. `run`'s `undoLabel` is what keeps the two apart.
     */
    expect(undoOffer()).toBe("Undo archived 1 conversation");
  });

  it("does not archive a conversation that has already left the inbox", async () => {
    const stub = stubSource({
      offer: { offer: "unsubscribe", method: "mail" },
      rows: [thread({ labelIds: ["ARCHIVE"] })],
    });
    await open(stub);

    await act(async () => probe().unsubscribe());
    await flush();

    expect(stub.commands).toEqual([{ kind: "unsubscribe", messageId: MESSAGE_ID }]);
  });

  /*
   * The failure this feature exists to not have. The conversation is already
   * archived, the request went nowhere, and the sender goes on sending — so the
   * line has to name the list and the reason, and carry the one thing left to
   * try.
   */
  it("says so out loud when the sender refuses, with the page beside it", async () => {
    const stub = stubSource({
      offer: { offer: "unsubscribe", method: "oneClick" },
      refuse: true,
    });
    await open(stub);

    await act(async () => probe().unsubscribe());
    await flush();

    const said = status();
    expect(said?.tone).toBe("error");
    expect(said?.message).toBe(
      "Could not unsubscribe from Whiny Nil — the list server returned 503",
    );
    expect(said?.action?.word).toBe("Open page");

    act(() => said!.action!.run());
    await flush();
    expect(stub.opened).toEqual([MESSAGE_ID]);
  });

  it("says one quiet line when there is nothing to unsubscribe from", async () => {
    const stub = stubSource();
    await open(stub);

    await act(async () => probe().unsubscribe());
    await flush();

    expect(stub.commands).toEqual([]);
    expect(status()?.message).toBe("No unsubscribe offered here");
    expect(status()?.tone).toBe("info");
  });
});
