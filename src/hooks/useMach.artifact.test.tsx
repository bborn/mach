// @vitest-environment jsdom

/**
 * Where pressing a card lands.
 *
 * The drawer knows *what* the agent surfaced; the shell knows where that thing
 * lives, and `openArtifact` is the seam between them. What it has to get right
 * for an event is the anchor: the grid shows a window, so opening a meeting
 * three weeks out without moving the anchor selects an event on a week that is
 * not on screen — the card would appear to do nothing at all.
 */

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { MachProvider, useMach, type MachActions } from "@/hooks/useMach";
import { fixtureSource, setDataSource, type MachDataSource } from "@/lib/data";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

/** The shell state, as the provider exposes it. Not exported by name. */
type Ui = ReturnType<typeof useMach>["ui"];

const START = Date.UTC(2026, 7, 20, 17, 0);

/** An empty store: this is about navigation, not about rows. */
function stubSource(): MachDataSource {
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
      return { threads: [], nextCursor: null };
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

function probe(): MachActions {
  return (window as unknown as { probe: MachActions }).probe;
}

function Probe({ onFrame }: { onFrame: (ui: Ui) => void }) {
  const { ui, actions } = useMach();
  onFrame(ui);
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

async function mount(frames: Ui[]) {
  setDataSource(stubSource());
  await act(async () => {
    root.render(
      <KeymapProvider>
        <MachProvider>
          <Probe onFrame={(ui) => frames.push(ui)} />
        </MachProvider>
      </KeymapProvider>,
    );
  });
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("opening what the agent surfaced", () => {
  it("puts an event on screen at the week it is actually in", async () => {
    const frames: Ui[] = [];
    await mount(frames);

    await act(async () =>
      probe().openArtifact({
        kind: "event",
        eventId: 91,
        startMs: START,
        endMs: START + 1_800_000,
        label: "30 min meeting between Bruno and Kerrie",
        conferenceUrl: "https://meet.google.com/vht-epjb-pjd",
      }),
    );

    const ui = frames[frames.length - 1];
    expect(ui.mode).toBe("calendar");
    expect(ui.eventId).toBe(91);
    // The whole point of carrying `startMs` on the artifact.
    expect(ui.anchor).toBe(START);
  });

  it("puts a conversation in the reading pane", async () => {
    const frames: Ui[] = [];
    await mount(frames);

    await act(async () =>
      probe().openArtifact({
        kind: "thread",
        threadId: 41774,
        label: "Series A data room",
        from: "Tawny Chen",
        atMs: 1_754_000_000_000,
      }),
    );

    const ui = frames[frames.length - 1];
    expect(ui.mode).toBe("mail");
    expect(ui.threadId).toBe(41774);
  });
});
