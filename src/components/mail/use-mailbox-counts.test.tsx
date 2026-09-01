// @vitest-environment jsdom

/**
 * The counts have to follow the store, not the launch.
 *
 * A number that is right at startup and then sits there is worse than no
 * number: snoozing a conversation, waking one, or throwing a draft away are all
 * moments when the rail is being looked at. So this pins the two clocks the
 * hook runs on — `threads-changed`, which every command and every sync pass
 * emits, and `mailboxCountsChanged`, which `useMach`'s `reload()` fires for the
 * composer's writes — and pins that neither of them is a render.
 */

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fixtureSource, setDataSource, type MailboxCounts, type MachDataSource } from "@/lib/data";
import { mailboxCountsChanged, useMailboxCounts } from "./use-mailbox-counts";

/** The coalesce window the hook uses for `threads-changed`. */
const COALESCE_MS = 600;

declare global {
  // eslint-disable-next-line no-var
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  vi.useFakeTimers();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.useRealTimers();
  globalThis.IS_REACT_ACT_ENVIRONMENT = undefined;
  setDataSource(fixtureSource);
});

/** A source whose counts can be moved, and which reports how often it is asked. */
function source(initial: MailboxCounts) {
  const state = { counts: initial, reads: 0 };
  let notify: (() => void) | undefined;
  const stub: MachDataSource = {
    ...fixtureSource,
    async mailboxCounts() {
      state.reads += 1;
      return state.counts;
    },
    async onThreadsChanged(handler) {
      notify = handler;
      return () => {
        notify = undefined;
      };
    },
  };
  setDataSource(stub);
  return {
    state,
    /** What Rust pushes after a snooze, a wake, a command or a sync pass. */
    threadsChanged: () => notify?.(),
  };
}

const last = (seen: MailboxCounts[]) => seen[seen.length - 1];

function mount(seen: MailboxCounts[]) {
  function Probe() {
    const counts = useMailboxCounts();
    useEffect(() => {
      seen.push(counts);
    }, [counts]);
    return null;
  }
  act(() => root.render(<Probe />));
}

/** Let the mount read, and any coalesced refresh behind it, resolve. */
async function settle() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(COALESCE_MS + 50);
  });
}

describe("useMailboxCounts", () => {
  it("reads the store once on mount", async () => {
    const { state } = source({ drafts: 2, snoozed: 1 });
    const seen: MailboxCounts[] = [];
    mount(seen);
    await settle();

    expect(state.reads).toBe(1);
    expect(last(seen)).toEqual({ drafts: 2, snoozed: 1 });
  });

  it("follows a snooze without a relaunch", async () => {
    const store = source({ drafts: 0, snoozed: 0 });
    const seen: MailboxCounts[] = [];
    mount(seen);
    await settle();

    store.state.counts = { drafts: 0, snoozed: 1 };
    act(() => store.threadsChanged());
    await settle();

    expect(last(seen)).toEqual({ drafts: 0, snoozed: 1 });
  });

  it("follows a discarded draft, which emits no threads-changed", async () => {
    const store = source({ drafts: 1, snoozed: 0 });
    const seen: MailboxCounts[] = [];
    mount(seen);
    await settle();

    store.state.counts = { drafts: 0, snoozed: 0 };
    await act(async () => {
      mailboxCountsChanged();
      await Promise.resolve();
    });

    expect(last(seen)).toEqual({ drafts: 0, snoozed: 0 });
  });

  it("coalesces a burst of pushes into one read", async () => {
    const store = source({ drafts: 1, snoozed: 1 });
    mount([]);
    await settle();
    expect(store.state.reads).toBe(1);

    act(() => {
      for (let i = 0; i < 50; i++) store.threadsChanged();
    });
    await settle();

    expect(store.state.reads, "a backfill is one refresh, not fifty").toBe(2);
  });

  it("hands back the same object when nothing moved", async () => {
    const store = source({ drafts: 1, snoozed: 1 });
    const seen: MailboxCounts[] = [];
    mount(seen);
    await settle();

    act(() => store.threadsChanged());
    await settle();

    // Two: the empty state the hook starts in, and the one read from the
    // store. The refresh that answered with the same numbers added nothing —
    // the value never changed identity, so the rail's memo does not rebuild
    // every row on every coalesced refresh.
    expect(seen).toHaveLength(2);
  });
});
