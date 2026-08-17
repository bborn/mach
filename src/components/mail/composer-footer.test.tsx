// @vitest-environment jsdom

/**
 * The footer stays on screen.
 *
 * `c` opened a panel with no send, no schedule, no paperclip and no discard.
 * All five were in the DOM the whole time: the overlay's panel is
 * `max-h-[68vh]` with `overflow: hidden`, and the composer inside it was a
 * plain block whose height was the sum of its parts — one of which, the
 * editor, is sized from the *window* rather than from the panel. At 1440×900
 * that came to 800px of composer inside a 612px panel, and the legend sat 173px
 * below the panel's bottom edge, painted nowhere.
 *
 * Measured in Blink, before and after, at seven window heights from 500 to
 * 1400: clipped at every one of them, by 45px at the shortest and 230px at
 * 1080. So a test that asks whether the footer is *in the document* would have
 * passed on every one of those. What decides it is which box gives up space
 * when there is not enough, and that is a structural fact this can hold:
 *
 *   * the footer, and every other row, keeps its size (`shrink-0`);
 *   * exactly one row — the message — does not, and can go to nothing;
 *   * the chain from the panel down to that row is a flex column that is
 *     allowed to shrink (`min-h-0`), or the constraint never reaches it.
 *
 * jsdom has no layout engine, so the assertions are about the classes that
 * produce the layout rather than about pixels. The pixels were checked in a
 * real engine and in the real window; see the commit.
 *
 * It happened again, from the other direction, and the fix is at the bottom of
 * this file. The footer became a row of real buttons, the composer stopped
 * fitting, flex started shrinking the message — and the message's *inner*
 * scroll box, sized `h-full`, went on resolving that percentage against the
 * height originally asked for. So the writing area was taller than the box
 * holding it and painted over the footer. The first fix made the constraint
 * reach the message; this one makes the message actually take the answer.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import type { Draft, DraftKind } from "@/lib/compose";
import { COMPOSER_BODY, COMPOSER_FIXED_ROW } from "./composer-layout";
import { Composer, type ComposerPresentation } from "./Composer";

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

function render(
  presentation: ComposerPresentation,
  over: Partial<Draft> = {},
  props: Partial<Parameters<typeof Composer>[0]> = {},
): HTMLElement {
  const host = document.createElement("div");
  host.innerHTML = renderToStaticMarkup(
    <KeymapProvider>
      <Composer
        draft={draft(over)}
        html=""
        bodyHeight={640}
        presentation={presentation}
        onChange={() => {}}
        onBodyChange={() => {}}
        onSend={() => {}}
        onClose={() => {}}
        onDiscard={() => {}}
        onAttach={() => {}}
        onRemoveAttachment={() => {}}
        onSetInline={() => {}}
        {...props}
      />
    </KeymapProvider>,
  );
  return host;
}

const classes = (el: Element) => (el.getAttribute("class") ?? "").split(/\s+/);
const has = (el: Element, token: string) => classes(el).includes(token);

/** The composer's own root — the element the overlay's panel holds. */
function root(host: HTMLElement): HTMLElement {
  const found = host.querySelector<HTMLElement>("[data-mach-composer]");
  if (!found) throw new Error("no composer rendered");
  return found;
}

/**
 * The footer, found by the control it exists for rather than by a marker.
 *
 * `discard` is one of the acts in that row with consequences outside the
 * panel, and it is a real button either way — so this finds the same element
 * before the fix and after it, which is what makes the failure meaningful.
 *
 * The key legend it now carries is decoration here: every control in the row
 * wears its own chip, and "⇧⌘⌫ discard" is the same control as "discard".
 */
function footer(host: HTMLElement): HTMLElement {
  const button = [...host.querySelectorAll("button")].find((candidate) => {
    const copy = candidate.cloneNode(true) as Element;
    for (const kbd of copy.querySelectorAll("kbd")) kbd.remove();
    return copy.textContent?.trim() === "discard";
  });
  if (!button?.parentElement) throw new Error("no discard button in the composer");
  return button.parentElement;
}

/** The box carrying the message's pixel height. */
function body(host: HTMLElement): HTMLElement {
  const area = host.querySelector<HTMLElement>('[role="textbox"][aria-label="Message"]');
  if (!area?.parentElement) throw new Error("no writing area in the composer");
  return area.parentElement;
}

describe("the composer's footer, inside the overlay's panel", () => {
  it("is not the box that gives up space", () => {
    const host = render("overlay");
    expect(has(footer(host), COMPOSER_FIXED_ROW)).toBe(true);
  });

  it("names itself, so the layout can be asked about it", () => {
    const host = render("overlay");
    expect(footer(host).hasAttribute("data-mach-composer-footer")).toBe(true);
  });

  /*
   * The half that is easy to leave out. A flex item's automatic minimum size
   * is its content, so a column without `min-h-0` refuses to shrink and
   * overflows its parent instead — which is the original bug again, one level
   * further down. Every link in the chain has to allow it or the panel's
   * maximum never reaches the message.
   */
  it("sits under a chain of columns the panel's height can reach", () => {
    const host = render("overlay");
    const top = root(host);
    for (let node = footer(host).parentElement; node; node = node.parentElement) {
      expect(has(node, "flex")).toBe(true);
      expect(has(node, "flex-col")).toBe(true);
      expect(has(node, COMPOSER_BODY)).toBe(true);
      if (node === top) break;
    }
    expect(has(top, "flex")).toBe(true);
    expect(has(top, COMPOSER_BODY)).toBe(true);
  });

  /*
   * One row may shrink and it is the message. Stated over the rendered
   * children rather than over the source, so a row added later without
   * `shrink-0` — a banner, a second legend — fails here rather than by
   * pushing the paperclip off the bottom of somebody's window.
   */
  it("shares its column with rows that all keep their size, bar the message", () => {
    const host = render("overlay", { attachments: [] });
    const column = footer(host).parentElement!;
    const flexible = [...column.children].filter((row) => !has(row, COMPOSER_FIXED_ROW));
    expect(flexible.length).toBe(1);
    expect(flexible[0].contains(body(host))).toBe(true);
  });

  it("keeps its size while an attachment list and the schedule row are up", () => {
    const host = render("overlay", {
      attachments: [
        {
          id: "a1",
          draftId: "d1",
          filename: "terms.pdf",
          mimeType: "application/pdf",
          sizeBytes: 1024,
          inline: false,
          contentId: "a1@mach.invalid",
        },
      ],
      // A draft being dragged over draws the drop target as well.
    }, { dragging: true });
    const column = footer(host).parentElement!;
    const flexible = [...column.children].filter((row) => !has(row, COMPOSER_FIXED_ROW));
    expect(flexible.length).toBe(1);
  });
});

describe("the message, which is what gives way", () => {
  /*
   * The pixel height has to be on a box that may come back down. It was on the
   * contenteditable itself, which is a fixed height and a scroll container: no
   * amount of shrinking above it moved that number, so the overflow went to
   * the bottom of the composer, where the footer is.
   */
  it("carries its height on a box that can shrink to nothing", () => {
    const host = render("overlay");
    const sized = body(host);
    expect(sized.getAttribute("style") ?? "").toMatch(/height:\s*640px/);
    expect(has(sized, COMPOSER_BODY)).toBe(true);
  });

  it("fills that box rather than setting its own height", () => {
    const host = render("overlay");
    const area = host.querySelector<HTMLElement>('[role="textbox"][aria-label="Message"]')!;
    expect(area.getAttribute("style")).toBeNull();
  });

  /*
   * The regression the footer's button bar caused, and the reason this file's
   * previous version of the test above asserted `h-full` — which was the bug.
   *
   * `height` on the sized box is a *preference*: a number from the window
   * (`popOutComposerHeight`) or from the drag handle (`clampComposerHeight`).
   * Flex is then allowed to shrink it, and does, the moment the furniture
   * around it grows — which is what turning the footer's thin spans into real
   * 24px buttons (`19f7a0a`) did. A percentage height inside that box resolves
   * against the number that was *asked for*, not the one flex settled on, so
   * the writing area stayed at its full height inside a shorter parent and the
   * overflow was painted over everything below it. In the popped-out composer
   * the message ran across the send button and on to the panel's bottom edge.
   *
   * `absolute inset-0` is resolved against the box's used size, so there is no
   * second number that can go stale. Nothing about the footer's height is
   * pinned here on purpose: pinning it would only move the staleness. What is
   * pinned is that the two boxes cannot disagree in the first place.
   */
  it("takes its size from the box as laid out, not from the number requested", () => {
    const host = render("overlay");
    const area = host.querySelector<HTMLElement>('[role="textbox"][aria-label="Message"]')!;
    expect(has(area, "absolute")).toBe(true);
    expect(has(area, "inset-0")).toBe(true);
    // Any of these re-reads the requested height and reintroduces the overlap.
    for (const stale of ["h-full", "h-screen", "min-h-full", "max-h-full"]) {
      expect(classes(area)).not.toContain(stale);
    }
    // …and the box it fills has to be the positioning context, or `inset-0`
    // would resolve against the window instead.
    expect(has(body(host), "relative")).toBe(true);
  });

  /*
   * The other half of the same relationship, from the footer's side: the row
   * that was blamed for the overlap must be the one that never gives way, and
   * it must be a sibling of the message in the column the panel's height
   * reaches. Together with the test above, a future footer that grows can push
   * the message shorter and can no longer land on top of it.
   */
  it("gives up its space to nothing, while the message gives up as much as asked", () => {
    const host = render("overlay");
    const column = footer(host).parentElement!;
    expect(has(footer(host), COMPOSER_FIXED_ROW)).toBe(true);
    expect(column.contains(body(host))).toBe(true);
    expect(has(body(host), COMPOSER_FIXED_ROW)).toBe(false);
  });
});

/*
 * The docked composer is the same component and must not have moved. It is
 * `shrink-0` inside the mail column — the reading pane above it is what gives
 * way there — and `clampComposerHeight` already keeps it inside the window.
 */
describe("the docked composer", () => {
  it("still keeps its whole height, and the conversation gives way instead", () => {
    const host = render("dock", { kind: "reply" as DraftKind });
    expect(has(root(host), COMPOSER_FIXED_ROW)).toBe(true);
  });

  it("has its footer under the same rule", () => {
    const host = render("dock", { kind: "reply" as DraftKind });
    expect(has(footer(host), COMPOSER_FIXED_ROW)).toBe(true);
  });
});
