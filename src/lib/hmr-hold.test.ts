/**
 * What the dev server is allowed to do to a window somebody is reading mail in.
 *
 * The HMR runtime is not here — no socket, no module graph, no browser — and
 * none of it needs to be. Everything the plugin decides is a function of the
 * payload Vite was about to send, and that payload is four fields of JSON. What
 * cannot be tested this way is whether wrapping `hot.send` catches every reload
 * Vite can start; that is a claim about Vite, and it was checked against a
 * running server instead.
 */

import { describe, expect, it } from "vitest";
import {
  TAKE_PAYLOAD,
  classify,
  isHeldKind,
  isStyleUpdate,
  type HotUpdate,
  type UpdatePayload,
} from "./hmr-hold";

function js(path: string): HotUpdate {
  return { type: "js-update", path, acceptedPath: path };
}

function css(path: string): HotUpdate {
  return { type: "css-update", path, acceptedPath: path };
}

describe("which messages are worth stopping", () => {
  it("stops a full reload, which is the one that loses the most", () => {
    expect(classify({ type: "full-reload", path: "*" })).toEqual({ send: null, keep: true });
  });

  it("stops a module update, because a swapped component is still a surprise", () => {
    const verdict = classify({ type: "update", updates: [js("/src/App.tsx")] });
    expect(verdict).toEqual({ send: null, keep: true });
  });

  it("lets a stylesheet through, which costs no state and no re-mount", () => {
    const payload = { type: "update", updates: [css("/src/styles/globals.css")] } as const;
    expect(classify(payload)).toEqual({ send: payload, keep: false });
  });

  it("splits a payload carrying both, rather than choosing for the whole", () => {
    const style = css("/src/styles/globals.css");
    const code = js("/src/components/mail/ThreadList.tsx");
    const { send, keep } = classify({ type: "update", updates: [style, code] });
    expect(send).toEqual({ type: "update", updates: [style] });
    expect(keep).toBe(true);
  });

  it("forwards the entries Vite built rather than copies of them", () => {
    // The three fields this module reads are not the six Vite sends, and the
    // other three are what its client needs to fetch the update.
    const style = { ...css("/src/styles/globals.css"), timestamp: 17, firstInvalidatedBy: null };
    const sent = classify({ type: "update", updates: [style] }).send;
    expect(sent?.type).toBe("update");
    expect((sent as UpdatePayload).updates[0]).toBe(style);
  });

  it("reads a stylesheet by its name as well as by its label", () => {
    // Tailwind's sheet reaches the window as a `js-update` for the module that
    // injects it. Trusting the label alone would hold that back and put a
    // toast on screen for a colour change.
    expect(isStyleUpdate(js("/src/styles/globals.css"))).toBe(true);
    expect(isStyleUpdate({ ...js("/x"), acceptedPath: "/src/styles/globals.css?t=1" })).toBe(true);
    expect(isStyleUpdate(js("/src/lib/colors.ts"))).toBe(false);
  });

  it("has no opinion about anything else Vite sends", () => {
    // An error still interrupts. A failure that is invisible is the specific
    // thing this project has paid the most for.
    expect(isHeldKind({ type: "error" })).toBe(false);
    expect(isHeldKind({ type: "prune" })).toBe(false);
    expect(isHeldKind({ type: "connected" })).toBe(false);
    expect(isHeldKind({ type: "update" })).toBe(true);
    expect(isHeldKind({ type: "full-reload" })).toBe(true);
  });
});

describe("taking what waited", () => {
  it("is a reload, whatever the change was", () => {
    // Not the module swap Vite had planned. Replaying a held `update` starts
    // an invalidate handshake the hold then interrupts, and the window ends up
    // half swapped — see the note in `hmr-hold.ts`.
    expect(TAKE_PAYLOAD).toEqual({ type: "full-reload", path: "*" });
  });
});
