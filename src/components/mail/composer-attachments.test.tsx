// @vitest-environment jsdom

/**
 * Files on their way out of the composer.
 *
 * Three routes lead here and they must arrive at the same place: a drop on the
 * composer, the paperclip, and ⇧⌘A. What each of them does with a path is Rust's
 * (`tests/compose.rs`); what this covers is the half that only exists on this
 * side — the list the owner reads, the choice between a picture in the message
 * and a file beside it, and the two rewrites that let a `cid:` be shown as an
 * image while the draft keeps the reference a recipient can resolve.
 *
 * Rendered with `react-dom/server`, like the receive side's `ThreadMessage`
 * tests: the claim being pinned is about *elements* — real buttons with real
 * names — and a `<div onClick>` looks identical on screen.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import {
  inlineImageDataUrl,
  inlineImageMarkup,
  inlineCidsIn,
  isInlinableImage,
  withCidReferences,
  withInlineImages,
  type Draft,
  type DraftAttachment,
  type DraftKind,
} from "@/lib/compose";
import { cleanFragment } from "@/lib/email-html";
import {
  isOverDropTarget,
  subscribeDragDrop,
  DROP_TARGET,
  type DragDropSignal,
} from "./composer-layout";
import { Composer } from "./Composer";

function file(over: Partial<DraftAttachment> = {}): DraftAttachment {
  return {
    id: "att-1",
    draftId: "d1",
    filename: "terms.pdf",
    mimeType: "application/pdf",
    sizeBytes: 158204,
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

function markup(over: Partial<Draft> = {}, props: Partial<Parameters<typeof Composer>[0]> = {}) {
  return renderToStaticMarkup(
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
        onSetInline={() => {}}
        {...props}
      />
    </KeymapProvider>,
  );
}

function parse(html: string): HTMLElement {
  const host = document.createElement("div");
  host.innerHTML = html;
  return host;
}

describe("the file a drop puts on the draft", () => {
  it("appears in the list, by name and by size", () => {
    const html = markup({ attachments: [file()] });
    expect(html).toContain("terms.pdf");
    expect(html).toContain("154 KB");
  });

  it("appears once per file, in the order they were chosen", () => {
    const host = parse(
      markup({
        attachments: [
          file({ id: "a", filename: "one.pdf" }),
          file({ id: "b", filename: "two.pdf" }),
        ],
      }),
    );
    const names = [...host.querySelectorAll("li span[title]")].map((s) => s.textContent);
    expect(names).toEqual(["one.pdf", "two.pdf"]);
  });

  it("is a list of list items, not a row of divs", () => {
    const host = parse(markup({ attachments: [file()] }));
    expect(host.querySelectorAll("ul li").length).toBe(1);
  });

  /*
   * Removal is a real `<button>` with an accessible name, which is the whole
   * of "removable by keyboard": a button is in the tab order, fires on Enter
   * and on Space, and announces what it will remove — none of which a
   * `<div onClick>` does, and all of which look the same on screen.
   */
  it("can be taken off by keyboard, because removal is a button with a name", () => {
    const host = parse(markup({ attachments: [file()] }));
    const remove = host.querySelector('button[aria-label="Remove terms.pdf"]');
    expect(remove).not.toBeNull();
    expect(remove!.getAttribute("tabindex")).toBeNull();
    expect(host.querySelector('[role="button"]')).toBeNull();
  });

  it("says nothing at all when there is nothing attached", () => {
    const host = parse(markup());
    expect(host.querySelector("ul")).toBeNull();
  });
});

describe("the choice between a picture in the message and a file beside it", () => {
  it("defaults to attached, and says so", () => {
    const host = parse(
      markup({ attachments: [file({ filename: "chart.png", mimeType: "image/png" })] }),
    );
    const toggle = host.querySelector('button[aria-label="Show chart.png in the message"]');
    expect(toggle).not.toBeNull();
    expect(toggle!.textContent).toBe("attached");
  });

  it("says the other thing once the image is in the message", () => {
    const host = parse(
      markup({
        attachments: [file({ filename: "chart.png", mimeType: "image/png", inline: true })],
      }),
    );
    const toggle = host.querySelector('button[aria-label="Attach chart.png as a file"]');
    expect(toggle).not.toBeNull();
    expect(toggle!.textContent).toBe("inline");
  });

  /*
   * The ceiling is the receive side's: an image drawn in the composer is base64
   * over IPC and a `data:` URL in the document, and base64 costs a third on
   * top. A 20 MB photograph is a 27 MB string built before the picture appears.
   * It is still attached and still sent — it is the *placing in the body* that
   * is not offered, and it is not offered rather than refused after the press.
   */
  it("is not offered on an image too large to draw", () => {
    const host = parse(
      markup({
        attachments: [
          file({ filename: "raw.png", mimeType: "image/png", sizeBytes: 20 * 1024 * 1024 }),
        ],
      }),
    );
    expect(host.querySelector('button[aria-label^="Show raw.png"]')).toBeNull();
    expect(host.querySelector('button[aria-label="Remove raw.png"]')).not.toBeNull();
  });

  // ...unless it is already there. A picture placed by some other route has to
  // be removable, and the control that removes it is this one.
  it("keeps the control on an oversized image that is already in the body", () => {
    const host = parse(
      markup({
        attachments: [
          file({
            filename: "raw.png",
            mimeType: "image/png",
            sizeBytes: 20 * 1024 * 1024,
            inline: true,
          }),
        ],
      }),
    );
    expect(host.querySelector('button[aria-label="Attach raw.png as a file"]')).not.toBeNull();
  });

  it("is offered on images and on nothing else", () => {
    expect(isInlinableImage(file({ mimeType: "image/png" }))).toBe(true);
    expect(isInlinableImage(file({ mimeType: "image/jpeg" }))).toBe(true);
    expect(isInlinableImage(file({ mimeType: "application/pdf" }))).toBe(false);
    // An image to a browser, a script host to everything that draws it.
    expect(isInlinableImage(file({ mimeType: "image/svg+xml" }))).toBe(false);
  });

  it("is not drawn when the surface cannot act on it", () => {
    const host = parse(
      markup(
        { attachments: [file({ filename: "chart.png", mimeType: "image/png" })] },
        { onSetInline: undefined },
      ),
    );
    expect(host.querySelectorAll("li button").length).toBe(1);
  });
});

describe("the drop target", () => {
  it("says where the files have to land while one is over the window", () => {
    expect(markup({}, { dragging: true })).toContain("Drop to attach");
    expect(markup()).not.toContain("Drop to attach");
  });

  it("marks its own root, so a drop can be tested against it", () => {
    const host = parse(markup());
    expect(host.firstElementChild!.hasAttribute(DROP_TARGET)).toBe(true);
  });

  /*
   * The regression this file exists for.
   *
   * The payload's position is typed `PhysicalPosition`, so the hit test divided
   * it by `devicePixelRatio` — and on macOS wry reports AppKit points, which are
   * already the units `getBoundingClientRect` answers in. See `DragPoint`.
   *
   * The rectangle below is the docked composer at 1440×900: the bottom of the
   * reading pane, in the right-hand half of the window. Halving a point inside
   * it lands in the thread list every time, whatever the drop, which is why
   * dragging a file onto a reply did nothing at all.
   */
  describe("hit-tested in the units the drag actually arrives in", () => {
    const ratio = window.devicePixelRatio;
    let host: HTMLDivElement;

    beforeEach(() => {
      Object.defineProperty(window, "devicePixelRatio", { value: 2, configurable: true });
      host = document.createElement("div");
      host.setAttribute(DROP_TARGET, "");
      host.getBoundingClientRect = () =>
        ({ left: 842, top: 500, right: 1440, bottom: 880, width: 598, height: 380 }) as DOMRect;
      document.body.append(host);
    });

    afterEach(() => {
      host.remove();
      Object.defineProperty(window, "devicePixelRatio", { value: ratio, configurable: true });
    });

    it("lands on a composer in the bottom-right of a Retina window", () => {
      expect(isOverDropTarget({ x: 1100, y: 700 })).toBe(true);
      expect(isOverDropTarget({ x: 900, y: 520 })).toBe(true);
      expect(isOverDropTarget({ x: 1430, y: 875 })).toBe(true);
    });

    it("does not land where the point would have been read as device pixels", () => {
      // (1100, 700) halved. Under the old conversion this was the point being
      // tested, and it is over the thread list.
      expect(isOverDropTarget({ x: 550, y: 350 })).toBe(false);
    });

    it("still refuses a drop outside the composer", () => {
      expect(isOverDropTarget({ x: 1100, y: 200 })).toBe(false);
      expect(isOverDropTarget({ x: 400, y: 700 })).toBe(false);
    });
  });

  it("is nowhere when no composer is open, so a drop lands on nothing", () => {
    expect(isOverDropTarget({ x: 10, y: 10 }, document)).toBe(false);
  });
});

/*
 * The subscription's lifetime, which is not the same question as whether the
 * handler is right.
 *
 * `onDragDropEvent` resolves a promise; React's cleanup runs synchronously. The
 * effect used to store its `unlisten` after two `await`s, so a cleanup that ran
 * first found nothing to call and the listener outlived the effect that made
 * it — one abandoned registration per keystroke, since the dependency was a
 * callback closed over the drafts.
 */
describe("the drag-drop subscription", () => {
  function deferred() {
    let settle!: (off: () => void) => void;
    const promise = new Promise<() => void>((resolve) => {
      settle = resolve;
    });
    return { promise, settle };
  }

  it("stops a listener whose registration finished after the cleanup ran", async () => {
    const unlisten = vi.fn();
    const { promise, settle } = deferred();
    const stop = subscribeDragDrop(() => {}, () => promise);

    // The effect re-ran — or the composer closed — before Tauri answered.
    stop();
    settle(unlisten);
    await promise;
    await Promise.resolve();

    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("stops it the ordinary way round too, and only once", async () => {
    const unlisten = vi.fn();
    const stop = subscribeDragDrop(() => {}, () => Promise.resolve(unlisten));
    await Promise.resolve();
    await Promise.resolve();

    stop();
    stop();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("passes the window's own signals straight through", async () => {
    const seen: DragDropSignal[] = [];
    let fire!: (signal: DragDropSignal) => void;
    subscribeDragDrop(
      (signal) => seen.push(signal),
      (handler) => {
        fire = handler;
        return Promise.resolve(() => {});
      },
    );
    await Promise.resolve();

    fire({ type: "drop", position: { x: 1, y: 2 }, paths: ["/tmp/a.png"] });
    expect(seen).toEqual([{ type: "drop", position: { x: 1, y: 2 }, paths: ["/tmp/a.png"] }]);
  });

  it("survives a registration that never succeeds", async () => {
    const stop = subscribeDragDrop(() => {}, () => Promise.reject(new Error("no webview")));
    await Promise.resolve();
    await Promise.resolve();
    expect(() => stop()).not.toThrow();
  });
});

/*
 * The two forms of a body with an image in it.
 *
 * The draft holds `cid:`, which is a reference to a part of the message and the
 * only form that means anything to a recipient. The editor holds a `data:` URL,
 * because a `cid:` resolves to nothing in a webview. Neither may leak into the
 * other: a `data:` URL in the draft row is a four-megabyte photograph rewritten
 * on every autosave, and a `cid:` in the editor is a broken image while writing.
 */
describe("an image in the body", () => {
  const CID = "att-9@mach.invalid";
  const URL = "data:image/png;base64,iVBORw0KGgo=";
  const resolved = new Map([[CID, URL]]);

  it("is written as a cid reference to the part that carries it", () => {
    const html = inlineImageMarkup(CID, "chart.png");
    expect(html).toContain(`src="cid:${CID}"`);
    expect(html).toContain(`data-mach-cid="${CID}"`);
    expect(html).toContain('alt="chart.png"');
  });

  it("survives the editor's own cleaning, which the data URL cannot", () => {
    // `insert` goes through this. If the marker were stripped there would be
    // nothing left to say which image the node was, and the reverse rewrite
    // could not put the `cid:` back.
    const fragment = cleanFragment(inlineImageMarkup(CID, "chart.png"), document);
    const image = fragment.querySelector("img")!;
    expect(image.getAttribute("data-mach-cid")).toBe(CID);
    expect(image.getAttribute("src")).toBe(`cid:${CID}`);
    // And a `data:` src is not on the allowlist, which is why the swap has to
    // happen in the DOM rather than through `setHTML`.
    const stripped = cleanFragment(`<img src="${URL}">`, document);
    expect(stripped.querySelector("img")!.getAttribute("src")).toBe(null);
  });

  it("shows as bytes in the editor and goes back to a cid on the way out", () => {
    const stored = `<div>Here:</div><div>${inlineImageMarkup(CID, "chart.png")}</div>`;
    const shown = withInlineImages(stored, resolved);
    expect(shown).toContain(`src="${URL}"`);
    expect(shown).not.toContain("cid:");

    const back = withCidReferences(shown);
    expect(back).toContain(`src="cid:${CID}"`);
    expect(back).not.toContain("data:image");
  });

  it("round-trips without disturbing the words around it", () => {
    const stored = `<div>Before</div>${inlineImageMarkup(CID, "c.png")}<div>After</div>`;
    const back = withCidReferences(withInlineImages(stored, resolved));
    expect(back).toContain("Before");
    expect(back).toContain("After");
    expect(inlineCidsIn(back)).toEqual([CID]);
  });

  it("keeps its cid when the bytes have not arrived yet", () => {
    const stored = inlineImageMarkup(CID, "c.png");
    // A broken image is honest. Silently dropping the tag would be a body that
    // differs from the one about to be sent.
    expect(withInlineImages(stored, new Map())).toContain(`cid:${CID}`);
  });

  it("is recognised from a bare cid, with no marker to go on", () => {
    // What a draft written by the agent, or adopted from another client, looks
    // like: no `data-mach-cid`, because nothing here put one there.
    const bare = `<img src="cid:${CID}">`;
    expect(inlineCidsIn(bare)).toEqual([CID]);
    expect(withInlineImages(bare, resolved)).toContain(`src="${URL}"`);
  });

  it("leaves a body with no images exactly as it was", () => {
    const plain = "<div>Nothing to see</div>";
    expect(withInlineImages(plain, resolved)).toBe(plain);
    expect(withCidReferences(plain)).toBe(plain);
    expect(inlineCidsIn(plain)).toEqual([]);
  });

  it("leaves an ordinary remote image alone in both directions", () => {
    const remote = '<img src="https://example.com/logo.png">';
    expect(withInlineImages(remote, resolved)).toBe(remote);
    expect(withCidReferences(remote)).toBe(remote);
  });

  it("builds a data URL from the bytes, and nothing from none", () => {
    expect(
      inlineImageDataUrl({
        attachmentId: "a",
        contentId: CID,
        mimeType: "image/png",
        filename: "c.png",
        base64: "iVBORw0KGgo=",
      }),
    ).toBe(URL);
    expect(
      inlineImageDataUrl({
        attachmentId: "a",
        contentId: CID,
        mimeType: "image/png",
        filename: "c.png",
        base64: "",
      }),
    ).toBeNull();
  });
});
