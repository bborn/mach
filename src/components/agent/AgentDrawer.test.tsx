/**
 * The drawer, tested as markup — the same trick `ThreadRow.test.tsx` uses: no
 * jsdom, nothing to click, and the output is the markup itself.
 *
 * Two claims worth pinning, both of which came out of one complaint about the
 * bottom of the window:
 *
 *  * **the rules are gone.** Not "look thinner" — gone. A regression here is
 *    somebody re-fencing the title off from the conversation it titles, which
 *    is easy to do by habit and invisible in a diff of one line;
 *  * **the asterisks are gone too**, and the word they were marking up is
 *    still there.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { AgentSession } from "@/lib/agent";
import { AgentDrawer } from "./AgentDrawer";

function session(over: Partial<AgentSession> = {}): AgentSession {
  return {
    id: "s1",
    title: "Reply to Dana about the data room",
    status: "done",
    createdAt: 0,
    context: [{ id: "c1", label: "Re: Series A data room", kind: "thread" }],
    entries: [
      { role: "user", text: "reply saying tuesday works" },
      { role: "tool", id: "t1", name: "draft_reply", summary: "Wrote a reply", state: "ok" },
      { role: "agent", text: "I put a **draft** in the conversation." },
    ],
    ...over,
  };
}

function markup(over: Partial<AgentSession> = {}): string {
  return renderToStaticMarkup(
    <AgentDrawer
      session={session(over)}
      height={320}
      onMinimise={() => {}}
      onClose={() => {}}
      onOpenArtifact={() => {}}
    />,
  );
}

describe("the drawer's rules", () => {
  it("draws none of its own except the one over the input", () => {
    const html = markup();
    // `border-t border-border` on the footer is the one that survives: it
    // divides a scrolling record from the box you type into. Everything else
    // — the title, the context chips, the panel's own top edge — is separated
    // by space, or by the container in App.tsx.
    expect(html.match(/border-t border-border/g)).toHaveLength(1);
    expect(html).not.toMatch(/\bborder-b\b/);
  });

  it("keeps the warning edge on an approval, which is an alarm and not a fence", () => {
    const html = markup({
      status: "awaitingApproval",
      pending: { toolUseId: "u1", name: "send", summary: "Send to dana@example.com", input: {} },
    });
    expect(html).toContain("border-warning");
  });

  it("is exactly as tall as it was told", () => {
    expect(markup()).toContain("height:320px");
    // …and nothing in here fixes a height of its own any more.
    expect(markup()).not.toContain("h-80");
  });
});

describe("the drawer's prose", () => {
  it("renders the agent's markdown instead of printing it", () => {
    const html = markup();
    expect(html).toContain("<strong");
    expect(html).toContain("draft");
    expect(html).not.toContain("**");
  });

  it("prints what the owner typed exactly as he typed it", () => {
    // His words are not markup: an asterisk he typed is an asterisk.
    const html = markup({ entries: [{ role: "user", text: "find the *other* thread" }] });
    expect(html).toContain("find the *other* thread");
  });

  it("renders the answer that is still arriving", () => {
    const html = markup({ streaming: "I am writing the **reply** now" });
    expect(html).toContain("<strong");
    expect(html).not.toContain("**reply**");
  });
});

describe("what a tool surfaced", () => {
  /** A `get_event` line, as the drawer receives it. */
  const withEvent: Partial<AgentSession> = {
    entries: [
      { role: "user", text: "show me the event" },
      {
        role: "tool",
        id: "t1",
        name: "get_event",
        summary: "Read “30 min meeting between Bruno and Kerrie”",
        state: "ok",
        artifact: {
          kind: "event",
          eventId: 91,
          startMs: Date.UTC(2026, 7, 20, 17, 0),
          endMs: Date.UTC(2026, 7, 20, 17, 30),
          label: "30 min meeting between Bruno and Kerrie",
          conferenceUrl: "https://meet.google.com/vht-epjb-pjd",
          guests: ["Bruno Bornsztein", "Kerrie Kuiper"],
        },
      },
    ],
  };

  it("draws it as a card under the tool line rather than a button on it", () => {
    const html = markup(withEvent);
    // The line still says what ran; the card says what it found.
    expect(html).toContain("Read “30 min meeting between Bruno and Kerrie”");
    expect(html).toContain("Thu Aug 20");
    expect(html).toContain("Bruno Bornsztein, Kerrie Kuiper");
    expect(html).toContain("Join");
    // The old affordance was this, and only this.
    expect(html).not.toContain(">Show event<");
  });

  it("still draws none of its own rules around it", () => {
    // The card is bordered — it is a box in a drawer that has no others — and
    // that must not become a second horizontal rule in the record.
    expect(markup(withEvent).match(/border-t border-border/g)).toHaveLength(1);
    expect(markup(withEvent)).not.toMatch(/\bborder-b\b/);
  });
});
