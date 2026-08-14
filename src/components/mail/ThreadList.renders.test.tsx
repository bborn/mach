// @vitest-environment jsdom

/**
 * How many rows a keystroke repaints.
 *
 * `j` changes the cursor on exactly two rows, and `ThreadRow` is wrapped in
 * `memo` so that those two are the only two React re-renders. That was not what
 * happened. The list built the click handler inline —
 * `onSelect={(e) => actions.clickThread(thread.id, …)}` — so every row got a new
 * function on every render, the shallow prop compare failed on every row, and
 * the `memo` never once held. Moving the cursor one row re-rendered the whole
 * mailbox: up to three hundred rows, each with a `cn()` class computation and a
 * date format, for a change to two of them.
 *
 * It is invisible in an end-state test — the list is correct either way — and
 * invisible in a screenshot. The only thing that says it is a render count, so
 * that is what this asserts.
 *
 * The count is on `ThreadRow` specifically, not on the subtree: `ThreadList`
 * itself re-renders on every cursor move and always will, because the provider
 * hands it a new context value. The claim here is narrower and is the one that
 * scales with the size of the mailbox.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { MachProvider, useMach, type MachActions } from "@/hooks/useMach";
import { fixtureSource, setDataSource, type MachDataSource } from "@/lib/data";
import type { Thread } from "@/types";

/**
 * The real row, counted.
 *
 * A spy rather than a stand-in: the thing under test is whether React decides to
 * call this component, which means it has to be the component the list actually
 * renders, `memo` and all.
 */
const rendered: number[] = [];
vi.mock("./ThreadRow", async (importOriginal) => {
  const { memo } = await import("react");
  const actual = await importOriginal<typeof import("./ThreadRow")>();
  const Real = actual.ThreadRow;
  // `memo` on the outside as well as the inside, and deliberately so: the
  // wrapper has to bail on exactly the comparison the real row bails on, or it
  // would count a render React was about to skip. Both use the default shallow
  // compare, so the two agree by construction.
  const Counted = memo((props: Parameters<typeof Real>[0]) => {
    rendered.push(props.thread.id);
    return <Real {...props} />;
  }) as unknown as typeof Real;
  return { ...actual, ThreadRow: Counted };
});

// Imported after the mock is declared; vitest hoists `vi.mock` above it.
const { ThreadList } = await import("./ThreadList");

const ROWS = 60;

function thread(id: number): Thread {
  return {
    id,
    accountId: 1,
    subject: `Conversation ${id}`,
    snippet: "snippet",
    participants: [{ name: "Someone", email: "someone@example.test" }],
    timestamp: 2_000_000 - id,
    unread: false,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
  };
}

const rows = Array.from({ length: ROWS }, (_, i) => thread(i + 1));

function source(): MachDataSource {
  return {
    ...fixtureSource,
    kind: "fixture",
    // A mailbox with no account behind it renders `MailboxNotice` instead of
    // rows, and this test is about the rows.
    async listAccounts() {
      return [
        {
          id: 1,
          email: "owner@example.test",
          name: "Owner",
          colorIndex: 1 as const,
          kind: "personal" as const,
        },
      ];
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
    async getThread(threadId) {
      const found = rows.find((r) => r.id === threadId);
      return found ? { thread: found, messages: [] } : null;
    },
    async onThreadsChanged() {
      return () => {};
    },
    async onSyncStatus() {
      return () => {};
    },
  };
}

function Probe() {
  const { actions } = useMach();
  (window as unknown as { probe: MachActions }).probe = actions;
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
  // jsdom has no layout, so it has neither of these. The list only uses them to
  // drag the viewport after the cursor, which is not what is being counted.
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = function scrollIntoView() {};
  }
  if (!window.IntersectionObserver) {
    window.IntersectionObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
      takeRecords() {
        return [];
      }
      root = null;
      rootMargin = "";
      thresholds = [];
    } as unknown as typeof IntersectionObserver;
  }
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  rendered.length = 0;
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

describe("moving the cursor through a mailbox", () => {
  it("re-renders the two rows the cursor left and arrived at, and no others", async () => {
    setDataSource(source());
    await act(async () => {
      root.render(
        <KeymapProvider>
          <MachProvider>
            <Probe />
            <ThreadList />
          </MachProvider>
        </KeymapProvider>,
      );
    });
    await flush();

    const probe = () => (window as unknown as { probe: MachActions }).probe;
    await act(async () => probe().selectThread(1));
    await flush();
    expect(rendered.length).toBeGreaterThan(0);

    // One `j`, from a settled list.
    rendered.length = 0;
    await act(async () => probe().moveCursor(1));
    await flush();

    const touched = [...new Set(rendered)].sort((a, b) => a - b);
    expect(touched).toEqual([1, 2]);
    expect(rendered.length).toBeLessThanOrEqual(4);
  });
});
