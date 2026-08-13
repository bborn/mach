// @vitest-environment jsdom

/**
 * The card, and the one complaint it exists to answer.
 *
 * He asked the agent to show him an event and got the row out of his own
 * SQLite as a bulleted list: the title, the time, the Meet link, the four
 * guests, none of it clickable. The drawer had an artifact seam and read tools
 * were excluded from it, so the only affordance on offer was nothing.
 *
 * So what is pinned here is what a card *says* — the fields that used to be
 * prose — and that pressing it hands back the artifact with the instant the
 * calendar has to be scrolled to. Structure through the markup, behaviour
 * through a real click, because a `<div onClick>` renders identically to a
 * `<button>` and only a Tab key nobody presses would tell you the difference.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Artifact } from "@/lib/agent";
import { ArtifactCard } from "./ArtifactCard";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

/** His event, as `get_event` hands it over. */
const EVENT: Artifact = {
  kind: "event",
  eventId: 91,
  startMs: Date.UTC(2026, 7, 20, 17, 0),
  endMs: Date.UTC(2026, 7, 20, 17, 30),
  label: "30 min meeting between Bruno Bornsztein and Kerrie Kuiper",
  conferenceUrl: "https://meet.google.com/vht-epjb-pjd",
  guests: ["Bruno Bornsztein", "Kerrie Kuiper", "Greg Dillon", "Tim Conrad"],
  guestCount: 4,
  rsvp: "accepted",
};

const THREAD: Artifact = {
  kind: "thread",
  threadId: 41774,
  label: "Series A data room",
  from: "Tawny Chen",
  atMs: Date.UTC(2025, 7, 1, 12, 0),
  unread: true,
};

function markup(artifact: Artifact): string {
  return renderToStaticMarkup(<ArtifactCard artifact={artifact} onOpen={() => {}} />);
}

describe("what the card says", () => {
  it("draws the event as an event, not as a sentence about one", () => {
    const html = markup(EVENT);
    expect(html).toContain("30 min meeting between Bruno Bornsztein and Kerrie Kuiper");
    // When, in the app's own vocabulary — the day stated, because an event is
    // in the future and `listTime` ages a stamp backwards from now.
    expect(html).toContain("Thu Aug 20");
    // Who is coming, a few of them and a count for the rest.
    expect(html).toContain("Bruno Bornsztein, Kerrie Kuiper, Greg Dillon +1");
    // And the one field on an event people click.
    expect(html).toContain("Join");
  });

  it("says the verb before the thing, so pressing it is not a guess", () => {
    expect(markup(EVENT)).toContain(
      'aria-label="Show event: 30 min meeting between Bruno Bornsztein and Kerrie Kuiper"',
    );
    expect(markup(THREAD)).toContain('aria-label="Open conversation: Series A data room"');
  });

  it("draws a conversation with its sender and its date", () => {
    const html = markup(THREAD);
    expect(html).toContain("Series A data room");
    expect(html).toContain("Tawny Chen");
    // `listTime`, the same clock the thread list ages its dates with — a
    // conversation from another year is a short date.
    expect(html).toContain("8/1/25");
  });

  it("leaves out what an artifact does not carry rather than drawing an empty line", () => {
    // A build of Rust that predates a field, or an event with nobody on it.
    const bare: Artifact = { kind: "event", eventId: 3, startMs: EVENT.startMs, label: "Coffee" };
    const html = markup(bare);
    expect(html).toContain("Coffee");
    expect(html).not.toContain("Join");
    expect(html).not.toContain("undefined");
  });

  it("is two buttons, both of which the keyboard can reach", () => {
    // Never a link nested inside the card's own button: that is invalid markup
    // and unreachable without a mouse. Nothing here is `tabindex="-1"` either.
    const html = markup(EVENT);
    expect(html.match(/<button/g)).toHaveLength(2);
    expect(html).not.toContain("tabindex");
  });
});

describe("what pressing it does", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("opens the object, and the video call separately", () => {
    const open = vi.fn();
    const external = vi.fn();
    act(() => {
      root.render(
        <ArtifactCard artifact={EVENT} onOpen={open} onOpenExternal={external} />,
      );
    });

    const buttons = [...container.querySelectorAll("button")];
    act(() => buttons[0].click());
    expect(open).toHaveBeenCalledTimes(1);
    expect(external).not.toHaveBeenCalled();

    act(() => buttons[1].click());
    expect(external).toHaveBeenCalledWith("https://meet.google.com/vht-epjb-pjd");
    // Joining is not opening: the card must not also navigate the shell.
    expect(open).toHaveBeenCalledTimes(1);
  });

  it("refuses a video link that is not one", () => {
    // `joinUrl` is the guard the event modal already uses. A `javascript:` URI
    // in a synced event must not become a button.
    const external = vi.fn();
    act(() => {
      root.render(
        <ArtifactCard
          artifact={{ ...EVENT, conferenceUrl: "javascript:alert(1)" } as Artifact}
          onOpen={() => {}}
          onOpenExternal={external}
        />,
      );
    });
    expect(container.querySelectorAll("button")).toHaveLength(1);
  });
});
