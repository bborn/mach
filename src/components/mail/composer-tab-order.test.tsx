// @vitest-environment jsdom

/**
 * The composer's tab order, asserted as a sequence.
 *
 * The complaint this exists for: ⇥ out of the To field walked the seven
 * rich-text buttons — bold, italic, underline, two lists, quote, link — one at
 * a time before reaching the message. The buttons are out of the sequence now
 * (`tabIndex={-1}` in `RichTextEditor`), and every action they offer has a key:
 * Squire binds ⌘B, ⌘I, ⌘U, ⌘⇧8, ⌘⇧9, ⌘] and ⌘[, and the editor registers ⌘K
 * for the link.
 *
 * The whole sequence is asserted, not just the pair, because the failure this
 * guards against is not "somebody deleted the tabIndex". It is somebody adding
 * an eighth toolbar button, or a colour picker, or an emoji tray, and putting
 * it where the other seven are — which reintroduces exactly the reported
 * defect and is invisible in a diff that only looks like a new button.
 *
 * Rendered with `react-dom/server` into a jsdom document. No effects run, so
 * this is the DOM the browser first lays hands on — which is the thing tab
 * order is a property of. Two caveats, both handled below:
 *
 *  * the writing area is a `contenteditable`, and `RichTextEditor` sets that
 *    attribute in an effect. It is matched here by `[role="textbox"]`, which
 *    the element carries in the markup. Chrome confirms it is a tab stop.
 *  * `tabIndex` on a contenteditable reads back as -1 in Chrome even while the
 *    element is in the sequence, so the property is no use as a filter.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import {
  COMPOSER_ROOT,
  keyboardInComposer,
  type Draft,
  type DraftAttachment,
  type DraftKind,
} from "@/lib/compose";
import { Composer } from "./Composer";

function file(over: Partial<DraftAttachment> = {}): DraftAttachment {
  return {
    id: "att-1",
    draftId: "d1",
    filename: "terms.pdf",
    mimeType: "application/pdf",
    sizeBytes: 4096,
    inline: false,
    contentId: "att-1@mach.invalid",
    ...over,
  };
}

function draft(over: Partial<Draft> = {}): Draft {
  return {
    id: "d1",
    accountId: 1,
    kind: "new" as DraftKind,
    to: [],
    cc: [],
    bcc: [],
    subject: "",
    body: "",
    bodyFormat: "html",
    updatedAt: 0,
    ...over,
  };
}

/**
 * Everything the browser would stop on, in document order.
 *
 * One selector, so the result comes back in document order — which is the tab
 * order, since nothing in the composer carries a positive `tabindex` and
 * nothing may: a positive one orders itself against the whole page.
 */
const TABBABLE = [
  'input:not([disabled]):not([tabindex="-1"])',
  'button:not([disabled]):not([tabindex="-1"])',
  'textarea:not([disabled]):not([tabindex="-1"])',
  'a[href]:not([tabindex="-1"])',
  '[role="textbox"]:not([tabindex="-1"])',
  '[tabindex="0"]',
].join(", ");

/** What a stop is called, preferring the name a screen reader would read. */
function name(element: Element): string {
  const aria = element.getAttribute("aria-label");
  if (aria) return aria;
  const labelled = element.closest("label")?.querySelector("span")?.textContent;
  if (labelled) return labelled.trim();
  // The key legend a footer button carries is decoration here — "⌃⇧O pop out"
  // and "pop out" are the same stop, and only one of them survives a rebind.
  const copy = element.cloneNode(true) as Element;
  for (const kbd of copy.querySelectorAll("kbd")) kbd.remove();
  const text = copy.textContent?.replace(/\s+/g, " ").trim();
  if (text) return text;
  return element.id || element.tagName.toLowerCase();
}

function stops(over: Partial<Draft> = {}, props: Partial<Parameters<typeof Composer>[0]> = {}) {
  const host = document.createElement("div");
  host.innerHTML = renderToStaticMarkup(
    <KeymapProvider>
      <Composer
        draft={draft(over)}
        html=""
        bodyHeight={340}
        onChange={() => {}}
        onBodyChange={() => {}}
        onSend={() => {}}
        onClose={() => {}}
        onDiscard={() => {}}
        onAttach={() => {}}
        onRemoveAttachment={() => {}}
        {...props}
      />
    </KeymapProvider>,
  );
  return [...host.querySelectorAll(TABBABLE)].map(name);
}

describe("the composer's tab order", () => {
  it("goes from the last address field to the message", () => {
    expect(stops()).toEqual([
      "Subject",
      "cc / bcc",
      "To",
      "Message",
      "attach",
      "discard",
    ]);
  });

  // Revealing Cc takes the button that revealed it away, so this is the whole
  // sequence and not the first one plus two.
  it("keeps Cc and Bcc between To and the message when they are shown", () => {
    expect(stops({ cc: [{ email: "sam@example.com" }] })).toEqual([
      "Subject",
      "To",
      "Cc",
      "Bcc",
      "Message",
      "attach",
      "discard",
    ]);
  });

  it("drops the subject field for a reply, whose subject is derived", () => {
    expect(stops({ kind: "reply", subject: "Re: the data room" })).toEqual([
      "cc / bcc",
      "To",
      "Message",
      "attach",
      "discard",
    ]);
  });

  /*
   * The files sit between the message and the footer, which is where they are
   * drawn — the sequence is the visual order or it is not a tab order.
   *
   * Each file is one stop, and an image is two: an image can be a file beside
   * the message or a picture inside it, and that choice must be reachable
   * without a mouse. Drag and drop cannot be the only way to attach and the
   * chip cannot be the only way to choose; the toggle is a real button, one ⇥
   * and one ⏎ away.
   */
  it("puts each file between the message and the footer", () => {
    expect(stops({ attachments: [file()] })).toEqual([
      "Subject",
      "cc / bcc",
      "To",
      "Message",
      "Remove terms.pdf",
      "attach",
      "discard",
    ]);
  });

  it("gives an image the inline choice as well, ahead of its remove", () => {
    expect(
      stops(
        { attachments: [file({ filename: "chart.png", mimeType: "image/png" })] },
        { onSetInline: () => {} },
      ),
    ).toEqual([
      "Subject",
      "cc / bcc",
      "To",
      "Message",
      "Show chart.png in the message",
      "Remove chart.png",
      "attach",
      "discard",
    ]);
  });

  it("names the inline control for what pressing it would do", () => {
    expect(
      stops(
        {
          attachments: [
            file({ filename: "chart.png", mimeType: "image/png", inline: true }),
          ],
        },
        { onSetInline: () => {} },
      ),
    ).toContain("Attach chart.png as a file");
  });

  // An SVG is an image to a browser and a script host to everything that draws
  // it, so it can be attached and never placed in a body — and a control that
  // cannot do anything should not be a stop on the way to one that can.
  it("offers no inline choice on a file that cannot go in the body", () => {
    const listed = stops(
      {
        attachments: [
          file({ filename: "logo.svg", mimeType: "image/svg+xml" }),
          file({ id: "att-2", filename: "terms.pdf" }),
        ],
      },
      { onSetInline: () => {} },
    );
    expect(listed).toEqual([
      "Subject",
      "cc / bcc",
      "To",
      "Message",
      "Remove logo.svg",
      "Remove terms.pdf",
      "attach",
      "discard",
    ]);
  });

  it("puts pop out last, after the message it moves", () => {
    expect(stops({}, { onPopOut: () => {} })).toEqual([
      "Subject",
      "cc / bcc",
      "To",
      "Message",
      "attach",
      "discard",
      "pop out",
    ]);
  });

  it("offers no stop at all on the formatting buttons", () => {
    const listed = stops();
    for (const tool of [
      "Bold",
      "Italic",
      "Underline",
      "Bulleted list",
      "Numbered list",
      "Quote",
      "Link",
    ]) {
      expect(listed).not.toContain(tool);
    }
  });

  it("still draws the formatting buttons, each naming its key", () => {
    const host = document.createElement("div");
    host.innerHTML = renderToStaticMarkup(
      <KeymapProvider>
        <Composer
          draft={draft()}
          html=""
          bodyHeight={340}
          onChange={() => {}}
          onBodyChange={() => {}}
          onSend={() => {}}
          onClose={() => {}}
          onDiscard={() => {}}
          onAttach={() => {}}
          onRemoveAttachment={() => {}}
        />
      </KeymapProvider>,
    );
    // A button held out of the tab sequence has to carry its key, or the
    // action it stands for is reachable by mouse and by nothing else.
    const tools = [...host.querySelectorAll("button[aria-pressed]")];
    expect(tools.map((b) => b.getAttribute("title"))).toEqual([
      "Bold (⌘B)",
      "Italic (⌘I)",
      "Underline (⌘U)",
      "Bulleted list (⌘⇧8)",
      "Numbered list (⌘⇧9)",
      "Quote (⌘] / ⌘[)",
      "Link (⌘K)",
    ]);
    for (const tool of tools) expect(tool.getAttribute("tabindex")).toBe("-1");
  });

  /*
   * Mail mode binds ⇥ to "sidebar or list", and the keymap only stands that
   * binding down while the target is a *field* — so every composer stop that
   * is a button used to throw the keyboard into the rail. The marker is how
   * the binding knows to decline; without it, ⇧⇥ out of To reached cc / bcc
   * and then left the composer, one stop short of the Subject field.
   */
  it("marks its root, so the shell knows the keyboard is inside it", () => {
    const host = document.createElement("div");
    host.innerHTML = renderToStaticMarkup(
      <KeymapProvider>
        <Composer
          draft={draft()}
          html=""
          bodyHeight={340}
          onChange={() => {}}
          onBodyChange={() => {}}
          onSend={() => {}}
          onClose={() => {}}
          onDiscard={() => {}}
          onAttach={() => {}}
          onRemoveAttachment={() => {}}
        />
      </KeymapProvider>,
    );
    const root = host.firstElementChild!;
    expect(root.hasAttribute(COMPOSER_ROOT)).toBe(true);

    document.body.append(host);
    try {
      const subject = host.querySelector<HTMLInputElement>("#composer-subject")!;
      subject.focus();
      expect(keyboardInComposer()).toBe(true);

      const outside = document.createElement("button");
      document.body.append(outside);
      outside.focus();
      expect(keyboardInComposer()).toBe(false);
      outside.remove();
    } finally {
      host.remove();
    }
  });

  it("puts nothing on a positive tabindex, which would reorder the page", () => {
    const host = document.createElement("div");
    host.innerHTML = renderToStaticMarkup(
      <KeymapProvider>
        <Composer
          draft={draft()}
          html=""
          bodyHeight={340}
          onChange={() => {}}
          onBodyChange={() => {}}
          onSend={() => {}}
          onClose={() => {}}
          onDiscard={() => {}}
          onAttach={() => {}}
          onRemoveAttachment={() => {}}
        />
      </KeymapProvider>,
    );
    for (const element of host.querySelectorAll("[tabindex]")) {
      expect(Number(element.getAttribute("tabindex"))).toBeLessThanOrEqual(0);
    }
  });
});
