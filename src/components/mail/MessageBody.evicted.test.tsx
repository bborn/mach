// @vitest-environment jsdom

/**
 * The read path for a message whose HTML was evicted.
 *
 * `MessageBody.test.tsx` covers the presentational half, which can be rendered
 * to a string. This is the other half — the two calls, in order, and what is on
 * screen between them — and it needs a real mount because the whole claim is
 * about *when* things appear: the text has to be there before the fetch resolves
 * and not after it.
 *
 * Nothing here goes near Tauri. `setRenderInvoker` is the seam the transport
 * already has, so both commands are scripted and every assertion includes what
 * was actually asked for.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it } from "vitest";
import { setRenderInvoker } from "@/lib/message-body";
import type { Message } from "@/types";
import { KeymapProvider } from "@/hooks/useKeymap";
import { MessageBody } from "./MessageBody";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  setRenderInvoker(null);
  globalThis.IS_REACT_ACT_ENVIRONMENT = undefined;
});

const message: Message = {
  id: 42,
  threadId: 7,
  accountId: 1,
  from: { name: "Tawny", email: "tawny@example.com" },
  to: [],
  cc: [],
  timestamp: 1_700_000_000_000,
  bodyText: "The quarterly numbers are attached.",
  snippet: "The quarterly numbers are attached.",
  attachments: [],
  isDraft: false,
};

/** The shape `render_message_body` returns, with the fields a test cares about. */
function wire(over: Record<string, unknown>) {
  return {
    messageId: 42,
    format: "text",
    remoteImagesAllowed: false,
    htmlEvicted: false,
    html: "",
    quotedHtml: "",
    hasQuoted: false,
    blockedRemoteImages: 0,
    blockedTrackers: 0,
    inlineCidImages: 0,
    inlineDataImages: 0,
    ...over,
  };
}

/** The text render an evicted message comes back with. */
const EVICTED = wire({
  format: "text",
  htmlEvicted: true,
  html: '<div style="white-space:pre-wrap">The quarterly numbers are attached.</div>',
});

/** What the fetch upgrades it to. */
const UPGRADED = wire({
  format: "html",
  htmlEvicted: false,
  html: "<p>The deck is attached, with the full breakdown.</p>",
});

/** The one iframe the body renders into, and the document it was handed. */
function frameHtml(): string {
  return container.querySelector("iframe")?.getAttribute("srcdoc") ?? "";
}

function mount() {
  act(() => {
    // The frame forwards keystrokes into the keymap — a message body that has
    // focus is otherwise a hole the whole keyboard falls into. So it needs the
    // provider the app always gives it.
    root.render(
      <KeymapProvider>
        <MessageBody message={message} live />
      </KeymapProvider>,
    );
  });
}

/** Let queued promise callbacks run, inside `act` so React flushes with them. */
async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

it("renders the text before the fetch resolves, and upgrades when it lands", async () => {
  const asked: string[] = [];
  let release: (value: unknown) => void = () => {};
  const restore = new Promise((resolve) => {
    release = resolve;
  });

  setRenderInvoker(async (command) => {
    asked.push(command);
    if (command === "render_message_body") return EVICTED as never;
    // Deliberately never resolves until the test says so: the point is what is
    // on screen while a fetch is outstanding.
    return (await restore) as never;
  });

  mount();
  await settle();

  expect(asked).toEqual(["render_message_body", "restore_message_body"]);
  expect(frameHtml()).toContain("The quarterly numbers are attached.");
  expect(frameHtml()).not.toContain("full breakdown");

  await act(async () => {
    release(UPGRADED);
    await restore;
  });
  await settle();

  expect(frameHtml()).toContain("The deck is attached, with the full breakdown.");
  // And no third call: the upgrade is the answer, not a reason to re-render.
  expect(asked).toEqual(["render_message_body", "restore_message_body"]);
});

it("makes no fetch for a message that was never evicted", async () => {
  // A plain-text message renders as text too. Asking Gmail about it would be a
  // round trip per open, forever, for a body that does not exist.
  const asked: string[] = [];
  setRenderInvoker(async (command) => {
    asked.push(command);
    return wire({ format: "text", html: "<div>plain</div>" }) as never;
  });

  mount();
  await settle();

  expect(asked).toEqual(["render_message_body"]);
  expect(frameHtml()).toContain("plain");
});

it("keeps the text and says so when the fetch fails", async () => {
  setRenderInvoker(async (command) => {
    if (command === "render_message_body") return EVICTED as never;
    throw { kind: "gone", message: "this message is no longer in Gmail" };
  });

  mount();
  await settle();

  // The body is never blank, and the failure is on screen rather than in a log.
  expect(frameHtml()).toContain("The quarterly numbers are attached.");
  expect(container.textContent).toContain("no longer in Gmail");
});

it("keeps the text when the network refuses", async () => {
  setRenderInvoker(async (command) => {
    if (command === "render_message_body") return EVICTED as never;
    throw new Error("could not reach Gmail: connection refused");
  });

  mount();
  await settle();

  expect(frameHtml()).toContain("The quarterly numbers are attached.");
  expect(container.textContent).toContain("could not reach Gmail");
});
