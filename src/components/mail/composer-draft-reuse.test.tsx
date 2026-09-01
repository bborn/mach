// @vitest-environment jsdom

/**
 * One conversation, one draft — including the untouched one.
 *
 * # What was wrong
 *
 * `r` on a conversation that already held a draft prepared a *second* one
 * whenever the first had no words in it yet. A reply composer that is opened
 * and closed without typing is not nothing: its recipients are filled in, so
 * `isDraftEmpty` is false, autosave writes the row, and the row is mirrored
 * into the conversation and pushed to Gmail. Replying again minted a new id
 * and left that one behind for ever.
 *
 * What the owner then saw, and reported: the reply he had just sent, and above
 * it a red `DRAFT` row of the same conversation that never went away. "I sent a
 * reply but still showing draft? super confusing? makes me not trust." His
 * store held both — `compose_drafts` had `draft-…-7b9d5be37f21f054` with a body
 * of `<div><br></div>`, and the outbox had sent `draft-…-1455dd99d1306c69`,
 * minted six seconds later on the same thread. Sending takes its own draft's
 * mirror out; nothing was ever going to take out the other one.
 *
 * # What is asserted
 *
 * That a conversation with a draft on it never prepares another, whatever is
 * or is not written in the one it has — which is what the reply strip a few
 * lines below `open` already tells the reader it does.
 */

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { PreferencesProvider } from "@/components/prefs/PreferencesProvider";
import { MachProvider, useMach, type MachActions } from "@/hooks/useMach";
import { fixtureSource, setDataSource } from "@/lib/data";
import type { Draft } from "@/lib/compose";
import type { ThreadId } from "@/types";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

const state = vi.hoisted(() => ({
  existing: null as Draft | null,
  prepared: [] as string[],
  saved: [] as string[],
  discarded: [] as string[],
}));

vi.mock("@/lib/compose", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/compose")>();
  return {
    ...actual,
    loadDraftForThread: async () => state.existing,
    prepareDraft: async (threadId: number, kind: string) => {
      state.prepared.push(kind);
      return {
        id: `draft-fresh-${state.prepared.length}`,
        accountId: 1,
        threadId,
        kind,
        to: [{ email: "alex@example.test" }],
        cc: [],
        bcc: [],
        subject: "Re: something",
        body: "",
        bodyFormat: "html",
        updatedAt: 0,
      } as Draft;
    },
    saveDraft: async (draft: Draft) => {
      state.saved.push(draft.id);
      return draft;
    },
    discardDraft: async (draftId: string) => {
      state.discarded.push(draftId);
      return { ok: true, remote: "none" as const };
    },
    flushOutbox: async () => ({ pending: [], sent: [] }),
  };
});

const { ComposerDock } = await import("./ComposerDock");

/** The untouched reply: recipients filled in by `prepare`, nothing typed. */
function untouched(): Draft {
  return {
    id: "draft-already-here",
    accountId: 1,
    threadId: 1 as unknown as number,
    kind: "replyAll",
    to: [{ email: "alex@example.test" }],
    cc: [],
    bcc: [],
    subject: "Re: something",
    body: "<div><br></div>",
    bodyFormat: "html",
    updatedAt: 0,
  };
}

function probe(): MachActions {
  return (window as unknown as { probe: MachActions }).probe;
}

function Probe() {
  const { actions } = useMach();
  useEffect(() => {
    (window as unknown as { probe: unknown }).probe = actions;
  });
  return null;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  // jsdom has no layout, and the tab strip scrolls its active tab into view.
  Element.prototype.scrollIntoView = () => {};
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
  state.existing = null;
  state.prepared = [];
  state.saved = [];
  state.discarded = [];
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
    for (let i = 0; i < 6; i += 1) await Promise.resolve();
  });
}

async function openThread(threadId: ThreadId) {
  setDataSource(fixtureSource);
  await act(async () => {
    root.render(
      <KeymapProvider>
        <PreferencesProvider>
          <MachProvider>
            <Probe />
            <ComposerDock />
          </MachProvider>
        </PreferencesProvider>
      </KeymapProvider>,
    );
  });
  await flush();
  await act(async () => probe().selectThread(threadId));
  await flush();
}

async function press(key: string) {
  await act(async () => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  });
  await flush();
}

describe("replying to a conversation that already holds a draft", () => {
  it("prepares one when there is nothing to resume", async () => {
    await openThread(1);
    await press("r");
    expect(state.prepared).toEqual(["reply"]);
  });

  it("resumes the untouched draft rather than minting a second id", async () => {
    state.existing = untouched();
    await openThread(1);
    await press("r");
    expect(state.prepared).toEqual([]);
  });

  it("does the same for reply-all and forward", async () => {
    state.existing = untouched();
    await openThread(1);
    await press("a");
    await press("f");
    expect(state.prepared).toEqual([]);
  });
});

/**
 * The other half, and the one that made the phantom in the first place.
 *
 * A reply composer opened and closed without a word typed used to leave a row
 * behind: `prepare` fills in the recipients, so `isDraftEmpty` said no, so
 * autosave wrote it, mirrored it into the conversation and pushed it to Gmail.
 * A conversation he had only glanced at replying to then carried `Draft`.
 */
describe("a reply composer that was opened and never typed in", () => {
  it("leaves nothing behind when it is closed", async () => {
    await openThread(1);
    await press("r");
    expect(state.prepared).toEqual(["reply"]);

    await press("Escape");

    expect(state.saved).toEqual([]);
    expect(state.discarded).toEqual(["draft-fresh-1"]);
  });
});
