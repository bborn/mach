/**
 * What this guards is that the stance row did not cost anything to get.
 *
 * The row stands in for the reply strip on a conversation the agent wrote
 * suggestions for, and the first version of it simply replaced that strip:
 * reply-all and forward kept their `a` and `f` bindings and lost their only
 * visible affordance. That is a regression against a thing asked for by name —
 * reply, reply-all and forward reachable from anywhere in a thread — and it is
 * invisible in every test that only checks the stances, which is why it needs
 * one of its own.
 *
 * So the assertions are on the *elements*, via `react-dom/server`: real
 * buttons, real accessible names, no jsdom and nothing to click. A
 * `<div onClick>` renders identically to a `<button>` and only a Tab key
 * nobody presses would tell you the difference — and everything in this app is
 * reachable without a mouse.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Stance } from "@/lib/suggestions";
import { StanceRow } from "./StanceRow";

const BOTH: Stance[] = [
  { label: "Say you'll wait", body: "Happy to wait — no rush on my end." },
  { label: "Offer a call", body: "Would fifteen minutes on Thursday help?" },
];

function markup(stances: Stance[]) {
  return renderToStaticMarkup(
    <StanceRow
      stances={stances}
      onPick={() => {}}
      onWriteMyself={() => {}}
      onReplyAll={() => {}}
      onForward={() => {}}
    />,
  );
}

/** Every `data-stance` the row drew, in the order it drew them. */
function slots(html: string): string[] {
  return [...html.matchAll(/data-stance="([^"]+)"/g)].map((m) => m[1]);
}

describe("the stance row", () => {
  it("keeps reply-all and forward drawn alongside the stances", () => {
    const html = markup(BOTH);
    // The whole point: the model's opinions did not evict the plain acts.
    expect(html).toContain("reply all");
    expect(html).toContain("forward");
    expect(html).toContain("write it myself");
  });

  it("draws the plain acts after the stances, never before", () => {
    // Order carries the meaning — the suggestions are the offer, and the strip
    // is what was always there. Reversed, the row reads as a strip with some
    // robot chips bolted on the end.
    expect(slots(markup(BOTH))).toEqual(["0", "1", "mine", "all", "forward"]);
  });

  it("draws the plain acts when the agent found only one stance", () => {
    // The degenerate case is the same row, not a second presentation of it.
    expect(slots(markup(BOTH.slice(0, 1)))).toEqual(["0", "mine", "all", "forward"]);
  });

  it("makes every act a real button", () => {
    const html = markup(BOTH);
    // Five controls, five buttons: two stances and three plain acts. A
    // `<div onClick>` here would be a mouse-only affordance, which this app
    // does not have.
    expect([...html.matchAll(/<button/g)]).toHaveLength(5);
  });

  it("carries the whole reply on the stance, so picking one waits for nothing", () => {
    // `title` is the body that is already local. Its presence is the evidence
    // that the text arrived with the label rather than being fetched on press.
    expect(markup(BOTH)).toContain("Happy to wait");
  });
});
