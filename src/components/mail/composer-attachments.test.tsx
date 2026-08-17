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
  type AttachResult,
  type Draft,
  type DraftAttachment,
  type DraftKind,
} from "@/lib/compose";
import { cleanFragment } from "@/lib/email-html";
import {
  dropRegionAt,
  isOverDropTarget,
  subscribeDragDrop,
  DROP_BODY,
  DROP_TARGET,
  type DragDropSignal,
} from "./composer-layout";
import {
  filenameOf,
  looksInlinable,
  pendingAttachments,
  runAttach,
  type PendingAttachment,
} from "./composer-attach";
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
  /*
   * The button says what pressing it does, not where the file already is.
   *
   * It read "attached" on a chip that *was* the attachment — the software
   * describing a state the row it sits in already shows, which CLAUDE.md bans
   * by name. The icon carries where the file stands, and an inline image
   * carries it more plainly still by being visible in the message above.
   */
  it("offers the other place, rather than announcing this one", () => {
    const host = parse(
      markup({ attachments: [file({ filename: "chart.png", mimeType: "image/png" })] }),
    );
    const toggle = host.querySelector('button[aria-label="Show chart.png in the message"]');
    expect(toggle).not.toBeNull();
    expect(toggle!.textContent).toBe("In message");
    expect(host.textContent).not.toContain("attached");
  });

  it("says the other thing once the image is in the message", () => {
    const host = parse(
      markup({
        attachments: [file({ filename: "chart.png", mimeType: "image/png", inline: true })],
      }),
    );
    const toggle = host.querySelector('button[aria-label="Attach chart.png as a file"]');
    expect(toggle).not.toBeNull();
    expect(toggle!.textContent).toBe("As file");
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

  /*
   * The user is choosing between two outcomes before letting go, so the hint
   * has to name the one currently selected. It used to be one highlight and
   * one sentence whatever the pointer was over.
   */
  describe("and what letting go there would do", () => {
    it("offers the message when an image is over the writing area", () => {
      expect(
        markup({}, { dragging: true, dropTarget: "body", draggingInlinable: true }),
      ).toContain("Drop in the message");
    });

    it("offers an attachment everywhere else on the composer", () => {
      expect(
        markup({}, { dragging: true, dropTarget: "composer", draggingInlinable: true }),
      ).toContain("Drop to attach");
    });

    /*
     * A `.zip` over the writing area is going to be attached — Rust sniffs the
     * bytes and refuses to place anything that is not a raster image — so the
     * hint must not have promised a picture. The filename is a guess, and it is
     * only ever used here; the decision itself never reads it.
     */
    it("does not promise the message to something that cannot go in one", () => {
      expect(
        markup({}, { dragging: true, dropTarget: "body", draggingInlinable: false }),
      ).toContain("Drop to attach");
      expect(looksInlinable(["/tmp/holiday.png"])).toBe(true);
      expect(looksInlinable(["/tmp/deck.pdf"])).toBe(false);
      expect(looksInlinable(["/tmp/a.png", "/tmp/b.zip"])).toBe(false);
      // Not in `sniff_raster_image`'s list, so Rust would attach it.
      expect(looksInlinable(["/tmp/IMG_0001.heic"])).toBe(false);
      expect(looksInlinable(["/tmp/logo.svg"])).toBe(false);
      expect(looksInlinable([])).toBe(false);
    });
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
 * Which half of the composer the file was let go on.
 *
 * The whole of inline drag-and-drop rests on this: the writing area means "in
 * the message", everything else on the composer means "beside it". It is
 * measured at Retina scale for the same reason the test above is — the point
 * arrives in AppKit points, and the day somebody reintroduces a
 * `devicePixelRatio` conversion the body rectangle is the first thing to move
 * out from under the pointer.
 *
 * The rectangles are a popped-out composer in a 1440×900 window: the panel
 * centred, the writing area filling most of it, and the footer along the
 * bottom edge where a drop must attach rather than place.
 */
describe("where inside the composer a file was let go", () => {
  const ratio = window.devicePixelRatio;
  let composer: HTMLDivElement;
  let body: HTMLDivElement;

  beforeEach(() => {
    Object.defineProperty(window, "devicePixelRatio", { value: 2, configurable: true });
    composer = document.createElement("div");
    composer.setAttribute(DROP_TARGET, "");
    composer.getBoundingClientRect = () =>
      ({ left: 304, top: 90, right: 1136, bottom: 810, width: 832, height: 720 }) as DOMRect;

    body = document.createElement("div");
    body.setAttribute(DROP_BODY, "");
    body.getBoundingClientRect = () =>
      ({ left: 324, top: 210, right: 1116, bottom: 750, width: 792, height: 540 }) as DOMRect;

    composer.append(body);
    document.body.append(composer);
  });

  afterEach(() => {
    composer.remove();
    Object.defineProperty(window, "devicePixelRatio", { value: ratio, configurable: true });
  });

  it("is the message when the pointer is on the writing area", () => {
    expect(dropRegionAt({ x: 700, y: 400 })).toBe("body");
    expect(dropRegionAt({ x: 330, y: 215 })).toBe("body");
    expect(dropRegionAt({ x: 1110, y: 745 })).toBe("body");
  });

  it("is the composer on the fields above it and the footer below it", () => {
    // The subject and address rows.
    expect(dropRegionAt({ x: 700, y: 150 })).toBe("composer");
    // The footer, where the send button is.
    expect(dropRegionAt({ x: 700, y: 790 })).toBe("composer");
    // The margin beside the writing area.
    expect(dropRegionAt({ x: 310, y: 400 })).toBe("composer");
  });

  it("is nothing at all outside the composer", () => {
    expect(dropRegionAt({ x: 100, y: 400 })).toBeNull();
    expect(dropRegionAt({ x: 700, y: 40 })).toBeNull();
  });

  /*
   * The regression the file above exists for, asked of the region rather than
   * of the boolean — and it bites harder here. A `devicePixelRatio` conversion
   * used to make a drop on the composer miss it altogether; now it would also
   * turn "put this picture in my message" into "attach it", silently, because
   * the halved point still lands on the composer and just not on the writing
   * area. Both readings below are of the *same gesture*.
   */
  it("reads the point as AppKit points on a Retina display", () => {
    expect(dropRegionAt({ x: 620, y: 500 })).toBe("body");
    expect(dropRegionAt({ x: 310, y: 250 })).toBe("composer");
    // Deeper into the message, where halving leaves the composer entirely.
    expect(dropRegionAt({ x: 1000, y: 700 })).toBe("body");
    expect(dropRegionAt({ x: 500, y: 350 })).toBe("body");
    expect(dropRegionAt({ x: 250, y: 175 })).toBeNull();
  });

  /*
   * A composer being unmounted, or one behind another, reports a zero
   * rectangle. Without the guard the window's origin is inside every one of
   * them and the first would claim every drop.
   */
  it("ignores a writing area that is not laid out", () => {
    body.getBoundingClientRect = () =>
      ({ left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }) as DOMRect;
    expect(dropRegionAt({ x: 700, y: 400 })).toBe("composer");
  });

  /*
   * More than one composer can be on screen. The writing area is looked for
   * inside the composer that was hit, so the one in front cannot lose a drop on
   * its footer to the editor of the one behind it.
   */
  it("never lets one composer's editor claim another composer's drop", () => {
    const second = document.createElement("div");
    second.setAttribute(DROP_TARGET, "");
    second.getBoundingClientRect = () =>
      ({ left: 0, top: 0, right: 1440, bottom: 900, width: 1440, height: 900 }) as DOMRect;
    const otherBody = document.createElement("div");
    otherBody.setAttribute(DROP_BODY, "");
    otherBody.getBoundingClientRect = () =>
      ({ left: 0, top: 0, right: 1440, bottom: 900, width: 1440, height: 900 }) as DOMRect;
    second.append(otherBody);
    document.body.append(second);

    // On the first composer's footer, and inside the second's editor.
    expect(dropRegionAt({ x: 700, y: 790 })).toBe("composer");
    second.remove();
  });
});

/*
 * The gap between letting go and the file being on the draft.
 *
 * Attaching is a round trip, and until this existed the composer drew nothing
 * at all until it came back: drop a file, watch nothing, and some time later a
 * chip. The name is in hand the instant the drop arrives, so the chip is too.
 *
 * `runAttach` is driven directly rather than through a render, because what is
 * being pinned is *when* each callback fires relative to the promise — which is
 * not something a snapshot of the DOM can see.
 */
describe("a file let go but not yet answered for", () => {
  function deferred<T>() {
    let settle!: (value: T) => void;
    let fail!: (error: unknown) => void;
    const promise = new Promise<T>((resolve, reject) => {
      settle = resolve;
      fail = reject;
    });
    return { promise, settle, fail };
  }

  function result(over: Partial<AttachResult> = {}): AttachResult {
    return { attachments: [], added: [], refused: [], ...over };
  }

  /** Everything `runAttach` was told to do, in order. */
  function recorder() {
    const shown: PendingAttachment[][] = [];
    const settled: PendingAttachment[][] = [];
    const applied: DraftAttachment[][] = [];
    const failed: string[] = [];
    return {
      shown,
      settled,
      applied,
      failed,
      show: (chips: PendingAttachment[]) => shown.push(chips),
      settle: (chips: PendingAttachment[]) => settled.push(chips),
      applied_: (files: DraftAttachment[]) => applied.push(files),
      failed_: (message: string) => failed.push(message),
    };
  }

  it("is on screen by name before Rust has been asked anything", async () => {
    const attach = deferred<AttachResult>();
    const log = recorder();
    const run = runAttach({
      paths: ["/Users/bruno/Desktop/q3 numbers.csv"],
      inline: false,
      save: () => Promise.resolve(),
      attach: () => attach.promise,
      show: log.show,
      settle: log.settle,
      applied: log.applied_,
      failed: log.failed_,
    });

    // Nothing has resolved. The chip is already up.
    expect(log.shown).toHaveLength(1);
    expect(log.shown[0][0].filename).toBe("q3 numbers.csv");
    expect(log.settled).toHaveLength(0);
    expect(log.applied).toHaveLength(0);

    attach.settle(result());
    await run;
  });

  it("gives way to the real one when the store answers", async () => {
    const log = recorder();
    const real = file({ id: "att-1", filename: "q3 numbers.csv" });
    await runAttach({
      paths: ["/tmp/q3 numbers.csv"],
      inline: false,
      save: () => Promise.resolve(),
      attach: () => Promise.resolve(result({ attachments: [real], added: [real] })),
      show: log.show,
      settle: log.settle,
      applied: log.applied_,
      failed: log.failed_,
    });

    expect(log.applied).toEqual([[real]]);
    expect(log.settled).toEqual(log.shown);
    expect(log.failed).toEqual([]);
  });

  /*
   * The failure this exists to prevent. A chip that goes on spinning for a file
   * that never landed is worse than the lag it was added to cover, so it comes
   * down on the way out of every branch.
   */
  it("comes down when the attach throws, and says what happened", async () => {
    const log = recorder();
    await runAttach({
      paths: ["/tmp/q3.csv"],
      inline: false,
      save: () => Promise.resolve(),
      attach: () => Promise.reject(new Error("the store is not writable")),
      show: log.show,
      settle: log.settle,
      applied: log.applied_,
      failed: log.failed_,
    });

    expect(log.settled).toEqual(log.shown);
    expect(log.applied).toHaveLength(0);
    expect(log.failed).toEqual(["the store is not writable"]);
  });

  it("comes down when the save throws too", async () => {
    const log = recorder();
    await runAttach({
      paths: ["/tmp/q3.csv"],
      inline: false,
      save: () => Promise.reject(new Error("no draft row")),
      attach: () => Promise.resolve(result()),
      show: log.show,
      settle: log.settle,
      applied: log.applied_,
      failed: log.failed_,
    });

    expect(log.settled).toEqual(log.shown);
    expect(log.failed).toEqual(["no draft row"]);
  });

  /*
   * The refusals are the case that would have been silent: nothing throws, the
   * call succeeds, and the file is simply not in the answer. The chip has to go
   * and the name has to be said.
   */
  it("comes down when the file is refused, and the refusal names it", async () => {
    const log = recorder();
    await runAttach({
      paths: ["/tmp/raw.psd"],
      inline: false,
      save: () => Promise.resolve(),
      attach: () =>
        Promise.resolve(
          result({ refused: ["raw.psd is 40.0 MB — larger than the 25 MB Gmail will send"] }),
        ),
      show: log.show,
      settle: log.settle,
      applied: log.applied_,
      failed: log.failed_,
    });

    expect(log.settled).toEqual(log.shown);
    expect(log.failed[0]).toContain("raw.psd");
  });

  it("keeps two drops apart, so one settling does not clear the other", async () => {
    const first = deferred<AttachResult>();
    const second = deferred<AttachResult>();
    const log = recorder();
    const options = {
      inline: false,
      save: () => Promise.resolve(),
      show: log.show,
      settle: log.settle,
      applied: log.applied_,
      failed: log.failed_,
    };
    const a = runAttach({ ...options, paths: ["/tmp/a.pdf"], attach: () => first.promise });
    const b = runAttach({ ...options, paths: ["/tmp/b.pdf"], attach: () => second.promise });

    first.settle(result());
    await a;
    expect(log.settled).toEqual([log.shown[0]]);
    expect(log.shown[0][0].key).not.toBe(log.shown[1][0].key);

    second.settle(result());
    await b;
    expect(log.settled).toEqual(log.shown);
  });

  it("only reaches the editor for images that actually went in the body", async () => {
    const placed: DraftAttachment[][] = [];
    const log = recorder();
    const inline = file({ id: "a", filename: "chart.png", inline: true, contentId: "a@m" });
    const beside = file({ id: "b", filename: "terms.pdf", inline: false });
    await runAttach({
      paths: ["/tmp/chart.png", "/tmp/terms.pdf"],
      inline: true,
      save: () => Promise.resolve(),
      attach: () =>
        Promise.resolve(result({ attachments: [inline, beside], added: [inline, beside] })),
      show: log.show,
      settle: log.settle,
      applied: log.applied_,
      placed: (files) => void placed.push(files),
      failed: log.failed_,
    });

    expect(placed).toEqual([[inline]]);
  });

  it("names the chip from the path, whichever separator it came with", () => {
    expect(filenameOf("/Users/bruno/Desktop/q3 numbers.csv")).toBe("q3 numbers.csv");
    expect(filenameOf("chart.png")).toBe("chart.png");
    expect(filenameOf("/tmp/deck/")).toBe("deck");
  });

  it("gives every chip its own identity, even for the same file twice", () => {
    const chips = pendingAttachments(["/tmp/a.png", "/tmp/a.png"], true);
    expect(chips.map((chip) => chip.filename)).toEqual(["a.png", "a.png"]);
    expect(chips[0].key).not.toBe(chips[1].key);
    expect(chips.every((chip) => chip.inline)).toBe(true);
  });
});

describe("the chip a pending file is drawn as", () => {
  const waiting: PendingAttachment[] = [
    { key: "p1", filename: "quarterly report.pdf", inline: false },
  ];

  it("is in the list, by name, before the store has answered", () => {
    const host = parse(markup({}, { pending: waiting }));
    expect(host.querySelectorAll("ul li")).toHaveLength(1);
    expect(host.textContent).toContain("quarterly report.pdf");
  });

  it("says it is still happening, rather than looking finished", () => {
    const host = parse(markup({}, { pending: waiting }));
    expect(host.querySelector("li[aria-busy]")).not.toBeNull();
    expect(host.querySelector("li .animate-spin")).not.toBeNull();
  });

  /*
   * No remove control: the id Rust will give this file does not exist yet, so
   * there is nothing to send `attachRemove` and a × would be a button that
   * cannot work.
   */
  it("offers nothing that needs an id the file does not have yet", () => {
    const host = parse(markup({}, { pending: waiting }));
    expect(host.querySelectorAll("li button")).toHaveLength(0);
    expect(host.querySelector('[aria-label^="Remove"]')).toBeNull();
  });

  it("stands beside the files that have already landed", () => {
    const host = parse(markup({ attachments: [file()] }, { pending: waiting }));
    const names = [...host.querySelectorAll("ul li span[title]")].map((s) => s.textContent);
    expect(names).toEqual(["terms.pdf", "quarterly report.pdf"]);
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
