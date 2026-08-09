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
import type { Attachment } from "@/types";
import { AttachmentRow } from "./ThreadMessage";

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
