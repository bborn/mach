// @vitest-environment jsdom

/**
 * The frame `⌘⏎` produces in the conversation underneath the composer.
 *
 * He sent a reply and the thread showed the sent message *and* a red `DRAFT`
 * row of the same words above it, for several seconds: "this UX still fucks me
 * up. was it sent? was it not?"
 *
 * Rust drops the draft's mirror in the same write that queues the message, so
 * the store is right immediately. What was late was the screen. Queuing is a
 * write, SQLite takes one writer at a time, and the sync loop is usually holding
 * it against a store measured in gigabytes — so the refetch that would have
 * shown the draft gone was chained to the very lock that made it slow. The rule
 * is that the UI never waits on that.
 *
 * So the assertions here are on the frame the keystroke itself produced, with
 * `getThread` deliberately never answering again: if the row is gone, it is gone
 * because the guess removed it, not because anything was refetched.
 */

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { MachProvider, useMach, type MachActions } from "@/hooks/useMach";
import { fixtureSource, setDataSource, type MachDataSource } from "@/lib/data";
import type { Message, Thread, ThreadDetail, ThreadId } from "@/types";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

const TARGET: ThreadId = 2;
const DRAFT_ID = "draft-19ff60c-1";

function message(over: Partial<Message> = {}): Message {
  return {
    id: 1,
    threadId: TARGET,
    accountId: 1,
    from: { name: "Alex Rivera", email: "alex@example.test" },
    to: [],
    cc: [],
    timestamp: 1_000,
    bodyText: "The original",
    snippet: "The original",
    attachments: [],
    isDraft: false,
    ...over,
  };
}

const THREAD: Thread = {
  id: TARGET,
  accountId: 1,
  subject: "Following up on our demo",
  snippet: "snippet",
  participants: [{ name: "Alex Rivera", email: "alex@example.test" }],
  timestamp: 2_000,
  unread: false,
  starred: false,
  hasAttachment: false,
  messageCount: 2,
  labelIds: ["INBOX", "DRAFT"],
};

/** What one render of the reading pane knows about the open conversation. */
interface Frame {
  drafts: string[];
  messages: number;
}

function stubSource(messages: Message[]) {
  let rows = messages;
  let reads = 0;
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
      return { threads: [THREAD], nextCursor: null };
    },
    async getThread(threadId): Promise<ThreadDetail | null> {
      reads += 1;
      return threadId === TARGET ? { thread: THREAD, messages: rows } : null;
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
    reads: () => reads,
    /** The queue write landing: the mirror is out of the store as well. */
    settled(next: Message[]) {
      rows = next;
    },
  };
}

function probe(): MachActions {
  return (window as unknown as { probe: MachActions }).probe;
}

function Probe({ onFrame }: { onFrame: (frame: Frame) => void }) {
  const { detail, actions } = useMach();
  onFrame({
    drafts: (detail?.messages ?? []).filter((m) => m.isDraft).map((m) => m.machDraftId ?? "?"),
    messages: detail?.messages.length ?? 0,
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

async function open(source: MachDataSource, frames: Frame[]) {
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

const DRAFT_ROW = message({ id: 2, isDraft: true, machDraftId: DRAFT_ID, bodyText: "Both items" });

describe("sending a draft repaints the conversation", () => {
  it("takes the draft row out on the frame ⌘⏎ produced, with nothing refetched", async () => {
    const frames: Frame[] = [];
    const stub = stubSource([message(), DRAFT_ROW]);
    await open(stub.source, frames);
    expect(frames[frames.length - 1]).toEqual({ drafts: [DRAFT_ID], messages: 2 });
    const reads = stub.reads();

    act(() => probe().draftSent(DRAFT_ID));

    expect(frames[frames.length - 1]).toEqual({ drafts: [], messages: 1 });
    expect(stub.reads()).toBe(reads);
  });

  it("leaves the other draft on the conversation alone", async () => {
    const frames: Frame[] = [];
    const other = message({ id: 3, isDraft: true, machDraftId: "draft-other" });
    const stub = stubSource([message(), DRAFT_ROW, other]);
    await open(stub.source, frames);

    act(() => probe().draftSent(DRAFT_ID));

    expect(frames[frames.length - 1]).toEqual({ drafts: ["draft-other"], messages: 2 });
  });

  it("puts it back on ⌘Z, which is the same frame the pill goes", async () => {
    const frames: Frame[] = [];
    const stub = stubSource([message(), DRAFT_ROW]);
    await open(stub.source, frames);
    act(() => probe().draftSent(DRAFT_ID));
    expect(frames[frames.length - 1]!.drafts).toEqual([]);

    act(() => probe().draftRecalled(DRAFT_ID));

    expect(frames[frames.length - 1]).toEqual({ drafts: [DRAFT_ID], messages: 2 });
  });

  /**
   * A guess stops being a guess when the store agrees with it — never on a
   * clock. Until then it has to keep hiding the row, or the draft the queue
   * write has not removed yet flickers back into the conversation.
   */
  it("stops guessing once the refetched conversation no longer has the row", async () => {
    const frames: Frame[] = [];
    const stub = stubSource([message(), DRAFT_ROW]);
    await open(stub.source, frames);
    act(() => probe().draftSent(DRAFT_ID));

    // The refetch that arrives before the write has landed: still hidden.
    await act(async () => probe().reload());
    await flush();
    expect(frames[frames.length - 1]!.drafts).toEqual([]);
    expect(frames[frames.length - 1]!.messages).toBe(1);

    // And the one after it, with the reply in the draft's place.
    const reply = message({ id: 4, bodyText: "Both items", timestamp: 3_000 });
    stub.settled([message(), reply]);
    await act(async () => probe().reload());
    await flush();
    expect(frames[frames.length - 1]).toEqual({ drafts: [], messages: 2 });

    // The guess is retired: a new draft on the same conversation is not hidden
    // by it. (Its id is the same one only because this is the same reply, put
    // back — which is exactly the case that must not stay invisible.)
    stub.settled([message(), DRAFT_ROW]);
    await act(async () => probe().reload());
    await flush();
    expect(frames[frames.length - 1]).toEqual({ drafts: [DRAFT_ID], messages: 2 });
  });
});
