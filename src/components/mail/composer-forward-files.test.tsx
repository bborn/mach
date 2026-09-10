// @vitest-environment jsdom

/**
 * A forward carries the files even when nobody named a message.
 *
 * # What was wrong
 *
 * `f` on an open conversation sends the words and leaves the attachment
 * behind. Reported against a forwarded invoice: "Just testing if the
 * attachment also forward" went out with the quoted original intact and the
 * PDF nowhere — no chip in the composer, nothing on the sent message.
 *
 * The composer fetches a forward's files in a second call, because a synced
 * attachment is a name and a size here until somebody asks Google for the
 * bytes. That call was guarded on the `replyToId` *argument*, which only the
 * message-cursor route supplies. `f` with the keyboard outside any message
 * row, and the Forward button under the thread, both pass nothing — so the
 * guard returned before the fetch, silently.
 *
 * `prepare` has no such gap: given no message it resolves the newest one in
 * the conversation, builds the forwarded text from it, and hands that parent
 * back on the draft. Asking the prepared draft rather than the caller makes
 * the two halves of a forward agree on which message they are forwarding.
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

/** The message `prepare` settles on when the caller names none: the newest. */
const NEWEST = 4321;

const state = vi.hoisted(() => ({
  fetched: [] as { draftId: string; messageId: number }[],
}));

vi.mock("@/lib/compose", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/compose")>();
  return {
    ...actual,
    loadDraftForThread: async () => null,
    prepareDraft: async (threadId: number, kind: string, replyToId?: number | null) =>
      ({
        id: "draft-forward",
        accountId: 1,
        threadId,
        // What Rust does: no message named, so the newest one in the thread.
        replyToId: replyToId ?? NEWEST,
        kind,
        to: [],
        cc: [],
        bcc: [],
        subject: "Fwd: Invoice #53",
        body: "",
        bodyFormat: "html",
        updatedAt: 0,
      }) as Draft,
    saveDraft: async (draft: Draft) => draft,
    discardDraft: async () => ({ ok: true, remote: "none" as const }),
    flushOutbox: async () => ({ pending: [], sent: [] }),
    forwardAttachments: async (draftId: string, messageId: number) => {
      state.fetched.push({ draftId, messageId });
      return {
        attachments: [
          {
            id: "att-1",
            draftId,
            filename: "invoice-53.pdf",
            mimeType: "application/pdf",
            size: 41_232,
            inline: false,
          },
        ],
        refused: [],
      };
    },
  };
});

const { ComposerDock } = await import("./ComposerDock");

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
  state.fetched = [];
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
    for (let i = 0; i < 8; i += 1) await Promise.resolve();
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

describe("forwarding with no message under the cursor", () => {
  it("fetches the files of the message the draft was prepared from", async () => {
    await openThread(1);
    await press("f");
    expect(state.fetched).toEqual([{ draftId: "draft-forward", messageId: NEWEST }]);
  });

  it("puts them on the composer as chips", async () => {
    await openThread(1);
    await press("f");
    expect(container.textContent).toContain("invoice-53.pdf");
  });
});

describe("forwarding a message the reader stepped into", () => {
  it("still fetches that one, not the newest", async () => {
    await openThread(1);
    const article = document.createElement("article");
    article.setAttribute("data-mach-message", "77");
    article.tabIndex = 0;
    container.append(article);
    article.focus();
    await press("f");
    expect(state.fetched).toEqual([{ draftId: "draft-forward", messageId: 77 }]);
  });
});
