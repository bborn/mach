// @vitest-environment jsdom

/**
 * A message that did not send has to be findable without going looking.
 *
 * The bug this covers is not a crash. `compose_outbox.state = 'failed'` has
 * existed for as long as the queue has, and nothing rendered it: four rows sat
 * in the owner's store for eighteen days, the newest a reply to a client, and
 * he found out when somebody read them out of SQLite for him. So the assertions
 * here are about *presence* — a row in the rail that exists only when something
 * is wrong, a panel that names the message and Google's reason, and two
 * decisions that reach the queue.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { MachProvider } from "@/hooks/useMach";
import { fixtureSource, setDataSource, describeSendFailure } from "@/lib/data";
import type { FailedSend } from "@/lib/compose";
import { railItems, type RailHandlers, type RailInput } from "./rail-model";
import { commandsWith } from "@/components/palette/CommandPalette";

/* ------------------------------------------------------------------ doubles */

const retried: string[] = [];
const discarded: string[] = [];
let rows: FailedSend[] = [];

vi.mock("@/lib/compose", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/compose")>();
  return {
    ...actual,
    listFailedSends: async () => rows,
    retrySend: async (id: string) => {
      retried.push(id);
      rows = rows.filter((row) => row.id !== id);
      return true;
    },
    discardSend: async (id: string) => {
      discarded.push(id);
      rows = rows.filter((row) => row.id !== id);
      return true;
    },
  };
});

const { UnsentPanel } = await import("./UnsentPanel");

function failure(over: Partial<FailedSend> = {}): FailedSend {
  return {
    id: "ob-1",
    accountId: 1,
    threadId: null,
    subject: "Re: Checking in",
    to: [{ name: "Briana Alberghini", email: "briana@fresh.example" }],
    cc: [],
    bcc: [{ email: "bcc@sdr.example" }],
    error: "google resource not found: Requested entity was not found.",
    createdAt: 1_700_000_000_000,
    attempts: 1,
    body: "Following up on the reporting we talked about.",
    attachments: [],
    ...over,
  };
}

/* --------------------------------------------------------------- the rail */

const HANDLERS: RailHandlers = {
  open: () => {},
  openLabel: () => {},
  openFavorite: () => {},
  unfavorite: () => {},
  toggle: () => {},
  openUnsent: () => {},
};

function rail(unsent: number, on: RailHandlers = HANDLERS) {
  const input: RailInput = {
    accounts: [],
    mailboxes: [],
    favorites: [],
    accountId: null,
    labelId: "INBOX",
    threadId: null,
    unread: { byAccount: new Map(), total: 0, capped: false },
    counts: { drafts: 0, snoozed: 0 },
    unsent,
    collapsed: [],
  };
  return railItems(input, on);
}

describe("the rail's Unsent row", () => {
  it("is not there when nothing failed", () => {
    expect(rail(0).map((item) => item.key)).not.toContain("unsent");
  });

  it("is the first row, counted, and marked as a problem", () => {
    const items = rail(4);
    expect(items[0]?.key).toBe("unsent");
    expect(items[0]?.label).toBe("Unsent");
    expect(items[0]?.count).toBe(4);
    expect(items[0]?.tone).toBe("warning");
    expect(items[0]?.shortcut).toBe("g u");
  });

  it("opens the list when it is activated", () => {
    let opened = 0;
    const items = rail(1, { ...HANDLERS, openUnsent: () => (opened += 1) });
    items[0]?.activate?.();
    expect(opened).toBe(1);
  });
});

/* ------------------------------------------------------------- the wording */

describe("what the status line says at the moment of failure", () => {
  it("names the message and keeps Google's reason verbatim", () => {
    expect(
      describeSendFailure({ id: "ob-1", subject: "Re: Checking in", message: "Invalid To header" }),
    ).toBe("Not sent: “Re: Checking in” — Invalid To header");
  });

  it("still says something for a message with no subject", () => {
    // Two of the owner's four failures have no subject at all.
    expect(describeSendFailure({ id: "ob-2", subject: "", message: "Invalid To header" })).toBe(
      "Not sent — Invalid To header",
    );
  });
});

/* ------------------------------------------------------------- the palette */

describe("the ⌘K entry", () => {
  it("is absent while the queue is clean", () => {
    expect(commandsWith(false, 0).map((c) => c.id)).not.toContain("unsent");
  });

  it("appears when something did not send", () => {
    expect(commandsWith(false, 2).map((c) => c.id)).toContain("unsent");
  });
});

/* --------------------------------------------------------------- the panel */

declare global {
  // eslint-disable-next-line no-var
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

describe("the panel", () => {
  let host: HTMLDivElement;
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
    retried.length = 0;
    discarded.length = 0;
    rows = [failure(), failure({ id: "ob-2", subject: "", body: "" })];
    setDataSource({ ...fixtureSource, async onSendFailed() { return () => {}; } });
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
  });

  afterEach(() => {
    act(() => root.unmount());
    host.remove();
    setDataSource(fixtureSource);
    globalThis.IS_REACT_ACT_ENVIRONMENT = undefined;
  });

  const open = async () => {
    await act(async () => {
      root.render(
        <KeymapProvider>
          <MachProvider>
            <UnsentPanel />
          </MachProvider>
        </KeymapProvider>,
      );
    });
    // `g u` is the way in, and it is the same key the rail row advertises.
    await act(async () => {
      press("g");
      press("u");
    });
  };

  const press = (key: string) => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  };

  it("names the message, who it was for, and why it did not go", async () => {
    await open();
    const text = host.textContent ?? "";
    expect(text).toContain("Re: Checking in");
    expect(text).toContain("briana@fresh.example");
    expect(text).toContain("bcc@sdr.example");
    expect(text).toContain("google resource not found");
    // The words survive: a message Google refuses on its address cannot be
    // retried into working, and this is all that is left of it.
    expect(text).toContain("Following up on the reporting");
  });

  it("says (no subject) rather than nothing", async () => {
    await open();
    expect(host.textContent).toContain("(no subject)");
  });

  it("reaches the queue with both decisions", async () => {
    await open();
    const first = host.querySelector('[data-unsent-id="ob-1"]');
    const buttons = first?.querySelectorAll("button") ?? [];
    expect([...buttons].map((b) => b.textContent)).toEqual(["Retry", "Discard"]);

    await act(async () => {
      (buttons[0] as HTMLButtonElement).click();
    });
    expect(retried).toEqual(["ob-1"]);

    const second = host.querySelector('[data-unsent-id="ob-2"]');
    const secondButtons = second?.querySelectorAll("button") ?? [];
    await act(async () => {
      (secondButtons[1] as HTMLButtonElement).click();
    });
    expect(discarded).toEqual(["ob-2"]);
  });

  it("is not reachable when there is nothing to show", async () => {
    rows = [];
    await open();
    expect(host.querySelector("[data-unsent-id]")).toBeNull();
  });
});
