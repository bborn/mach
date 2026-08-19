/**
 * The attachment chip row, tested as markup.
 *
 * The claim worth pinning here is the keyboard one. "Everything is usable
 * without a mouse" is easy to believe and easy to lose: a `<div onClick>` looks
 * identical on screen, and the only thing that would have told you is a tab
 * key. So these assert on the *elements* — real buttons, real accessible names,
 * a real list — via `react-dom/server`, with no jsdom and nothing to click.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Attachment, Message } from "@/types";
import { AttachmentRow, preview, ThreadMessage } from "./ThreadMessage";

function attachment(over: Partial<Attachment> = {}): Attachment {
  return {
    id: 1,
    messageId: 10,
    filename: "Q3-numbers.pdf",
    mimeType: "application/pdf",
    sizeBytes: 158204,
    ...over,
  };
}

function row(attachments: Attachment[], live = true): string {
  return renderToStaticMarkup(<AttachmentRow attachments={attachments} live={live} />);
}

describe("the attachment row", () => {
  it("renders one filename, one type icon and one human-readable size per attachment", () => {
    const html = row([attachment(), attachment({ id: 2, filename: "q3.csv", sizeBytes: 2048 })]);
    expect(html).toContain("Q3-numbers.pdf");
    expect(html).toContain("154 KB");
    expect(html).toContain("q3.csv");
    expect(html).toContain("2 KB");
    // Two chips, each with its own icon.
    expect(html.split("<svg").length - 1).toBeGreaterThanOrEqual(4);
  });

  it("is reachable and operable from the keyboard, because it is buttons", () => {
    const html = row([attachment()]);
    // Two real buttons — open and save. A real `<button>` is in the tab order,
    // responds to Enter and Space, and announces itself, all without a single
    // `tabIndex` or `onKeyDown`.
    expect(html.split('<button type="button"').length - 1).toBe(2);
    expect(html).not.toContain('role="button"');
    expect(html).not.toContain("tabindex");
  });

  it("gives every control an accessible name that includes the size", () => {
    const html = row([attachment()]);
    expect(html).toContain('aria-label="Open Q3-numbers.pdf, 154 KB"');
    expect(html).toContain('aria-label="Save Q3-numbers.pdf"');
    // And the group itself is named, so a screen reader can skip it.
    expect(html).toContain('aria-label="Attachments"');
  });

  it("says a program is a program before anybody clicks it", () => {
    const html = row([
      attachment({ filename: "invoice.pdf.exe", mimeType: "application/octet-stream" }),
    ]);
    expect(html).toContain("a program Mach will not open");
    // Saving is still offered — that is the whole split. Opening starts a
    // process; saving hands the file to Finder and to Gatekeeper.
    expect(html).toContain('aria-label="Save invoice.pdf.exe"');
  });

  it("disables the controls when there is no backend to fetch from", () => {
    const html = row([attachment()], false);
    expect(html.split('disabled=""').length - 1).toBe(2);
    expect(html).toContain("Needs the app");
    expect(row([attachment()], true)).not.toContain('disabled=""');
  });

  it("shows the sender's filename and never invents one", () => {
    // The row displays what the store holds. Sanitising happens in Rust at the
    // moment the name becomes a path, and the name the file ends up with comes
    // back on `AttachmentFile.filename` — this row is not the place that
    // decides it.
    const html = row([attachment({ filename: "évidence — été.pdf" })]);
    expect(html).toContain("évidence");
  });

  // A bounce report's `text/rfc822-headers`, Gmail's AMP alternative body and
  // the JSON behind an emoji reaction all arrive as parts with no filename —
  // thirteen of them in the mailbox this was tested against. The chip used to
  // render as an icon and a size with nothing in between.
  it("gives a nameless part the name it will actually be saved under", () => {
    const html = row([
      attachment({ filename: "", mimeType: "text/rfc822-headers", sizeBytes: 3349 }),
    ]);
    expect(html).toContain("attachment");
    expect(html).toContain('aria-label="Save attachment"');
    expect(html).toContain('aria-label="Open attachment, 3 KB"');
  });

  it("tells three copies of one filename apart", () => {
    const html = row([
      attachment({ id: 1, filename: "Plowing 2025.pdf" }),
      attachment({ id: 2, filename: "Plowing 2025.pdf" }),
      attachment({ id: 3, filename: "Plowing 2025.pdf" }),
    ]);
    expect(html).toContain("Plowing 2025 (2).pdf");
    expect(html).toContain("Plowing 2025 (3).pdf");
  });

  it("renders nothing but the list when there is no status to report", () => {
    const html = row([attachment()]);
    expect(html).not.toContain('role="status"');
  });
});

/* -------------------------------------------------------------------------- */

function message(over: Partial<Message> = {}): Message {
  return {
    id: 10,
    threadId: 1,
    accountId: 1,
    from: { name: "Bruno Bornsztein", email: "bruno@example.com" },
    to: [{ name: "Marcus Oyelaran", email: "marcus@lumen.example" }],
    cc: [],
    timestamp: Date.UTC(2026, 7, 9, 12, 2),
    snippet: "",
    bodyText: "Numbers look right to me — shipping Thursday.",
    attachments: [],
    isDraft: false,
    ...over,
  };
}

function threadMessage(over: Partial<Message> = {}, expanded = false): string {
  return renderToStaticMarkup(
    <ThreadMessage
      message={message(over)}
      live={false}
      expanded={expanded}
      onToggle={() => {}}
      onOpenDraft={() => {}}
    />,
  );
}

/**
 * The claim: a message you have not sent cannot be mistaken for one you have.
 *
 * The bug this pins down shipped: the mirror put the draft in the conversation,
 * the Drafts mailbox found it, Gmail synced it — and the row in the thread was
 * pixel-identical to a sent reply, while the agent went on describing the
 * thread as carrying a DRAFT label. The app and the agent contradicting each
 * other in front of the owner is the failure; a colour swap would not have
 * fixed it.
 */
describe("a draft inside a conversation", () => {
  it("says the word, so it survives greyscale and a screen reader", () => {
    const html = threadMessage({ isDraft: true });
    expect(html).toContain(">Draft<");
    // Colour is reinforcement on top of the word, never the thing carrying it.
    expect(html).toContain("text-danger");
  });

  it("marks nothing on an ordinary message", () => {
    const html = threadMessage();
    expect(html).not.toContain(">Draft<");
    expect(html).not.toContain("text-danger");
    expect(html).not.toContain("data-draft");
  });

  it("is a real button, so the keyboard reaches it with no help", () => {
    const html = threadMessage({ isDraft: true });
    // Tab order, Enter and Space, and a focus ring, all for free. A div with an
    // onClick looks the same and gives none of them.
    expect(html).toContain('<button type="button"');
    expect(html).not.toContain('role="button"');
    expect(html).not.toContain("tabindex");
    expect(html).toContain('title="Edit draft"');
  });

  it("does not pretend to expand, because editing is the only thing to do with it", () => {
    // `aria-expanded` on a control that opens a composer would announce a
    // disclosure that never discloses.
    expect(threadMessage({ isDraft: true }, true)).not.toContain("aria-expanded");
    expect(threadMessage({}, true)).toContain('aria-expanded="true"');
  });

  it("shows the draft's own text rather than an address, expanded or not", () => {
    const html = threadMessage({ isDraft: true }, true);
    expect(html).toContain("Numbers look right to me");
    expect(html).not.toContain("bruno@example.com");
  });
});

/* -------------------------------------------------------------------------- */

/**
 * The line a collapsed row shows.
 *
 * The complaint: four rows of a conversation all reading
 * `body { margin: 0; padding: 0; -webkit-text-size-adjust: 100%…`. The sender's
 * mailer had put its `<style>` block into the `text/plain` alternative, so the
 * body genuinely began with 554 characters of CSS — and the preview was a slice
 * of the body. Gmail's snippet, which Mach was already fetching and throwing
 * away, described every one of those messages correctly.
 */
describe("the collapsed preview", () => {
  it("uses Gmail's snippet, not a body that opens with a stylesheet", () => {
    const line = preview(
      message({
        bodyText:
          "body { margin: 0; padding: 0; -webkit-text-size-adjust: 100%; } " +
          "img { border: 0 } Sooner or later every client asks",
        snippet: "Sooner or later every client asks whether the campaign paid off.",
      }),
    );

    expect(line).toBe("Sooner or later every client asks whether the campaign paid off.");
    expect(line).not.toContain("margin");
  });

  it("falls back to the body for a draft Gmail has never seen", () => {
    expect(preview(message({ bodyText: "half a reply", snippet: "" }))).toBe("half a reply");
    expect(preview(message({ bodyText: "half a reply", snippet: "   " }))).toBe("half a reply");
  });

  it("collapses whitespace and stops at 200 characters", () => {
    const line = preview(message({ snippet: `a${"  \n  "}b`, bodyText: "" }));
    expect(line).toBe("a b");
    expect(preview(message({ snippet: "x".repeat(400), bodyText: "" }))).toHaveLength(200);
  });
});

/**
 * The fold is a transition, and a collapsed message still costs nothing.
 *
 * Two claims that pull against each other, which is why both are pinned.
 *
 * The row has to stay mounted while it is shut, or there is nothing for the
 * close to animate on — an element React has removed cannot transition. But
 * `MessageBody` is a sandboxed frame that parses and renders the message, and
 * a forty-message thread with forty of them mounted is forty frames working on
 * mail nobody is looking at. The fold has always unmounted it and must go on
 * doing so; the mount is merely *held* for the length of the transition.
 *
 * Measured on a real page rather than asserted here: the track runs
 * 0 → 129px on an ease-out over 200ms and back down without a snap, and the
 * frame count returns to one afterwards.
 */
describe("the fold", () => {
  it("keeps a shut message in the DOM so the close has something to animate", () => {
    const shut = threadMessage({}, false);
    expect(shut).toContain("grid-rows-[0fr]");
    expect(shut).toContain("transition-[grid-template-rows]");
    expect(shut).toContain("motion-reduce:transition-none");
  });

  it("opens the same track rather than swapping in a different one", () => {
    expect(threadMessage({}, true)).toContain("grid-rows-[1fr]");
  });

  it("hides a shut message from the eye and the screen reader", () => {
    const shut = threadMessage({}, false);
    expect(shut).toContain("invisible");
    expect(shut).toMatch(/aria-hidden="true"[^>]*class="[^"]*overflow-hidden/);
    expect(threadMessage({}, true)).not.toContain("invisible");
  });

  it("draws no body for a message that is shut", () => {
    // The frame is the expensive part and the reason the fold unmounts at all.
    expect(threadMessage({}, false)).not.toContain("<iframe");
  });

  it("gives a draft no fold at all — its row opens the composer", () => {
    expect(threadMessage({ isDraft: true }, false)).not.toContain("grid-rows-[0fr]");
  });
});
