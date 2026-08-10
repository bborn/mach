// @vitest-environment jsdom

/**
 * Six drafts are not easier to destroy than one.
 *
 * The single-draft discard has asked since it was written, and the reason is in
 * `ComposerDock`: a discard ends at `drafts.delete`, Gmail does not hand the id
 * back, and an "undo" could only mean creating a *different* draft containing
 * the same words. None of that gets better in bulk, so the bulk one asks too —
 * and the thing worth a test is that the *first* press writes nothing.
 *
 * `discardThreadDrafts` is stubbed, because what it does is send IPC and what
 * this is about is whether it is called at all.
 */

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { fixtureSource, setDataSource, type MachDataSource } from "@/lib/data";
import type { Thread } from "@/types";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

const discardThreadDrafts = vi.fn(async () => ({
  discarded: 6,
  missing: 0,
  remoteFailed: 0,
}));

vi.mock("@/lib/compose", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/compose")>();
  return { ...actual, discardThreadDrafts };
});

const { MachProvider, useMach } = await import("@/hooks/useMach");
type Actions = import("@/hooks/useMach").MachActions;

function draftThread(id: number): Thread {
  return {
    id,
    accountId: 1,
    subject: `Draft ${id}`,
    snippet: "…",
    participants: [{ name: "Me", email: "me@example.test" }],
    timestamp: 1_000 + id,
    unread: false,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["DRAFT"],
  };
}

const DRAFTS = [1, 2, 3, 4, 5, 6].map(draftThread);

function source(): MachDataSource {
  return {
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
      return { threads: DRAFTS, nextCursor: null };
    },
    async getThread() {
      return null;
    },
    async onThreadsChanged() {
      return () => {};
    },
    async onSyncStatus() {
      return () => {};
    },
  };
}

/** The latest render's actions and the one field the question lives in. */
function probe(): { actions: Actions; armed: boolean; selected: number } {
  return (window as unknown as { probe: ReturnType<typeof probe> }).probe;
}

function Probe() {
  const { actions, ui } = useMach();
  useEffect(() => {
    (window as unknown as { probe: unknown }).probe = {
      actions,
      armed: ui.confirmDiscard,
      selected: ui.selection.ids.length,
    };
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
  discardThreadDrafts.mockClear();
  setDataSource(source());
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

async function mount() {
  await act(async () => {
    root.render(
      <KeymapProvider>
        <MachProvider>
          <Probe />
        </MachProvider>
      </KeymapProvider>,
    );
  });
  await flush();
}

/** Open Drafts and tick all six. */
async function selectSixDrafts() {
  await act(async () => probe().actions.selectAllThreads());
  await flush();
  expect(probe().selected).toBe(6);
}

describe("discarding a selection of drafts", () => {
  it("asks on the first press and writes nothing", async () => {
    await mount();
    await selectSixDrafts();

    await act(async () => probe().actions.discardSelected());
    await flush();

    expect(probe().armed).toBe(true);
    expect(discardThreadDrafts).not.toHaveBeenCalled();
    // The rows are still there to be looked at while the question is up.
    expect(probe().selected).toBe(6);
  });

  it("throws them away on the second", async () => {
    await mount();
    await selectSixDrafts();

    await act(async () => probe().actions.discardSelected());
    await flush();
    await act(async () => probe().actions.discardSelected());
    await flush();

    expect(discardThreadDrafts).toHaveBeenCalledTimes(1);
    expect(discardThreadDrafts).toHaveBeenCalledWith([1, 2, 3, 4, 5, 6]);
    expect(probe().armed).toBe(false);
    expect(probe().selected).toBe(0);
  });

  it("takes the question back when the selection changes under it", async () => {
    // Otherwise a question asked about six drafts could be answered about
    // seven — or about one, after a stray `x`.
    await mount();
    await selectSixDrafts();

    await act(async () => probe().actions.discardSelected());
    await flush();
    expect(probe().armed).toBe(true);

    await act(async () => probe().actions.clearSelection());
    await flush();

    expect(probe().armed).toBe(false);
    expect(discardThreadDrafts).not.toHaveBeenCalled();
  });
});
