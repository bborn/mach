/**
 * The WebView half of `docs/message-rendering-invariants.md`, tested as
 * controls rather than as appearance.
 *
 * The sandbox attribute and the frame CSP are the two things standing between
 * a hostile message and an app holding five mailboxes, and both are one typo
 * away from doing nothing at all while still looking right on screen. So they
 * are asserted on the *rendered markup*, via `react-dom/server` — no jsdom, no
 * WebView, no eyeballing.
 */

import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  clampFrameHeight,
  containWideContent,
  externalUrl,
  frameCsp,
  frameDocument,
  frameGround,
  isEffectivelyEmpty,
  localTextRender,
  mapRenderedMessage,
  nextFrameHeight,
  nextFrameSize,
  reportLinkFailure,
  resetFrameSize,
  revealBlockedImages,
  shouldAutoExpandQuote,
  subscribeLinkFailures,
  FRAME_HEIGHT_EPSILON,
  FRAME_SANDBOX,
  INITIAL_FRAME_SIZE,
  LINK_FAILED_EVENT,
  MAX_FRAME_HEIGHT,
  MIN_FRAME_HEIGHT,
  type BlockedImage,
  type FrameSize,
  type FrameTokens,
  type RenderedMessage,
  type WideCandidate,
} from "@/lib/message-body";
import { KeymapProvider } from "@/hooks/useKeymap";
import { MessageBodyView } from "./MessageBody";
import { MessageFrame } from "./MessageFrame";

function rendered(over: Partial<RenderedMessage> = {}): RenderedMessage {
  return {
    messageId: 1,
    format: "html",
    remoteImagesAllowed: false,
    htmlEvicted: false,
    html: "<p>hello</p>",
    quotedHtml: "",
    hasQuoted: false,
    blockedRemoteImages: 0,
    blockedTrackers: 0,
    inlineCidImages: 0,
    inlineDataImages: 0,
    ...over,
  };
}

function view(over: Partial<RenderedMessage> = {}, props: Partial<Parameters<typeof MessageBodyView>[0]> = {}) {
  return renderToStaticMarkup(
    // The frames inside read the keymap — they forward the keystrokes an iframe
    // would otherwise swallow. See `frame-keys.test.ts`.
    <KeymapProvider>
      <MessageBodyView
        rendered={rendered(over)}
        subject="Tawny"
        allowRemoteImages={false}
        onLoadRemoteImages={() => {}}
        quotedOpen={false}
        onToggleQuoted={() => {}}
        {...props}
      />
    </KeymapProvider>,
  );
}

/**
 * The value of one attribute in rendered markup, HTML entities decoded.
 *
 * Case-insensitive because React's server renderer emits the React prop name
 * (`srcDoc`, `referrerPolicy`) rather than the DOM attribute name. HTML
 * attribute names are case-insensitive, so the browser sees `srcdoc` either
 * way — but a case-sensitive assertion here would be testing React's spelling
 * rather than ours.
 */
function attribute(markup: string, name: string): string | null {
  const match = new RegExp(`${name}="([^"]*)"`, "i").exec(markup);
  if (!match) return null;
  return match[1]!
    .replace(/&quot;/g, '"')
    .replace(/&#x27;/g, "'")
    .replace(/&#39;/g, "'")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&");
}

// ---------------------------------------------------------------------------
// invariant 1: the sandbox
// ---------------------------------------------------------------------------

describe("the message frame's sandbox", () => {
  const markup = renderToStaticMarkup(
    // The frame reads the keymap: it forwards the keystrokes that would
    // otherwise be swallowed by the iframe. See `frame-keys.test.ts`.
    <KeymapProvider>
      <MessageFrame
        html="<p>hi</p>"
        allowRemoteImages={false}
        format="html"
        title="Message from Tawny"
      />
    </KeymapProvider>,
  );

  it("is exactly allow-same-origin allow-popups", () => {
    expect(attribute(markup, "sandbox")).toBe("allow-same-origin allow-popups");
    expect(FRAME_SANDBOX).toBe("allow-same-origin allow-popups");
  });

  it("never grants scripts, top navigation, forms or modals", () => {
    const sandbox = attribute(markup, "sandbox") ?? "";
    for (const capability of [
      "allow-scripts",
      "allow-top-navigation",
      "allow-top-navigation-by-user-activation",
      "allow-forms",
      "allow-modals",
      "allow-downloads",
      "allow-presentation",
      "allow-pointer-lock",
      // The one that would make a popup a way *out* of the sandbox rather than
      // a navigation the app cancels.
      "allow-popups-to-escape-sandbox",
    ]) {
      expect(sandbox).not.toContain(capability);
    }
  });

  /*
   * `allow-popups` is the fix for a link that could not be clicked, and it is
   * the kind of thing a later tidy-up removes as "obviously unnecessary on a
   * frame with no scripts". It is load-bearing: without it WebKit refuses the
   * `target="_blank"` navigation the sanitizer forces onto every anchor, before
   * anything outside the engine — including the guard that opens links — is
   * consulted. See FRAME_SANDBOX.
   */
  it("grants allow-popups, which is what lets a link reach the navigation guard", () => {
    expect(FRAME_SANDBOX.split(" ")).toContain("allow-popups");
  });

  it("is present on the element itself, not only in the constant", () => {
    // A `sandbox` that never reached the DOM is the failure mode this catches:
    // an unsandboxed iframe renders identically.
    expect(markup).toMatch(/<iframe[^>]*\ssandbox="/);
    expect(markup).toMatch(/referrerpolicy="no-referrer"/i);
  });
});

// ---------------------------------------------------------------------------
// invariant 2: the CSP
// ---------------------------------------------------------------------------

describe("the message frame's CSP", () => {
  it("is the documented minimum, with images limited to data: by default", () => {
    expect(frameCsp(false)).toBe(
      "default-src 'none'; script-src 'none'; base-uri 'none'; form-action 'none'; " +
        "frame-src 'none'; object-src 'none'; style-src 'unsafe-inline'; img-src data:",
    );
  });

  it("widens img-src to https: only when remote images are allowed", () => {
    expect(frameCsp(true)).toContain("img-src data: https:");
    expect(frameCsp(false)).not.toContain("https:");
    // Opting into images opts into nothing else.
    const [allowed, blocked] = [frameCsp(true), frameCsp(false)];
    expect(allowed.replace("img-src data: https:", "img-src data:")).toBe(blocked);
  });

  it("reaches the frame as a meta policy, since srcdoc has no headers", () => {
    const doc = frameDocument({ html: "<p>hi</p>", allowRemoteImages: false, format: "html" });
    expect(doc).toContain(
      `<meta http-equiv="Content-Security-Policy" content="${frameCsp(false)}">`,
    );
    expect(doc).toContain("<p>hi</p>");
  });

  it("is carried in the srcdoc the component actually renders", () => {
    const markup = renderToStaticMarkup(
      <KeymapProvider>
        <MessageFrame html="<p>hi</p>" allowRemoteImages={false} format="html" title="t" />
      </KeymapProvider>,
    );
    const srcdoc = attribute(markup, "srcdoc") ?? "";
    expect(srcdoc).toContain("Content-Security-Policy");
    expect(srcdoc).toContain("script-src 'none'");
    expect(srcdoc).toContain("img-src data:");
    expect(srcdoc).not.toContain("img-src data: https:");
  });

  it("cannot have a stylesheet broken out of by a malformed token", () => {
    const doc = frameDocument({
      // Plain text, because that is the ground the app's tokens reach at all.
      html: "",
      allowRemoteImages: false,
      format: "text",
      tokens: { "--foreground": "red}body{display:none", "--accent": "oklch(0.55 0.18 255)" },
    });
    // The claim is that the token did not escape its declaration and start a
    // rule of its own — not that the string `display:none` is absent, which the
    // stylesheet legitimately uses to hide tracking pixels.
    expect(doc).not.toContain("body{display:none");
    expect(doc).not.toContain("red}");
    expect(doc).toContain("--accent:oklch(0.55 0.18 255)");
  });
});

// ---------------------------------------------------------------------------
// remote images
// ---------------------------------------------------------------------------

describe("the blocked-images bar", () => {
  it("does not appear when nothing was blocked", () => {
    const markup = view({ blockedRemoteImages: 0 });
    expect(markup).not.toContain("Load images");
    expect(markup).not.toContain("blocked");
  });

  it("appears with the count when images were blocked", () => {
    const markup = view({ blockedRemoteImages: 3 });
    expect(markup).toContain("Load images");
    expect(markup).toContain("3");
    expect(markup).toContain("remote image");
  });

  it("goes away once the reader has opted in", () => {
    const markup = view({ blockedRemoteImages: 3 }, { allowRemoteImages: true });
    expect(markup).not.toContain("Load images");
  });

  it("says 'image' rather than 'images' for one", () => {
    expect(view({ blockedRemoteImages: 1 })).toContain("1 remote image blocked");
  });

  it("reveals a blocked src as a DOM property, never as markup", () => {
    const images: BlockedImage[] = [
      { dataset: { machBlockedSrc: "https://cdn.test/a.png" }, src: "", removeAttribute: () => {} },
      { dataset: { machBlockedSrc: "javascript:alert(1)" }, src: "", removeAttribute: () => {} },
      { dataset: {}, src: "", removeAttribute: () => {} },
    ];
    const revealed = revealBlockedImages({ querySelectorAll: () => images });

    expect(revealed).toBe(1);
    expect(images[0]!.src).toBe("https://cdn.test/a.png");
    expect(images[1]!.src).toBe("");
    expect(images[2]!.src).toBe("");
  });
});

// ---------------------------------------------------------------------------
// invariant 7: quoted history
// ---------------------------------------------------------------------------

describe("quoted history", () => {
  it("stays collapsed when there is new content above it", () => {
    expect(shouldAutoExpandQuote({ html: "<p>My answer.</p>", hasQuoted: true })).toBe(false);
  });

  it("auto-expands when the whole body is quoted — a bare forward", () => {
    expect(shouldAutoExpandQuote({ html: "", hasQuoted: true })).toBe(true);
    expect(shouldAutoExpandQuote({ html: "<div> </div>", hasQuoted: true })).toBe(true);
    expect(shouldAutoExpandQuote({ html: "<div>&nbsp;<br></div>", hasQuoted: true })).toBe(true);
  });

  it("does not treat an image-only body as empty", () => {
    // Otherwise a sender hides text behind the collapse and shows only a pixel.
    expect(isEffectivelyEmpty('<img src="x">')).toBe(false);
    expect(isEffectivelyEmpty("<table><tr><td></td></tr></table>")).toBe(false);
    expect(shouldAutoExpandQuote({ html: '<div><img src="x"></div>', hasQuoted: true })).toBe(
      false,
    );
  });

  it("offers no affordance at all when there is no quote", () => {
    expect(shouldAutoExpandQuote({ html: "", hasQuoted: false })).toBe(false);
    expect(view({ hasQuoted: false })).not.toContain("Quoted text");
  });

  it("renders the quoted half in its own frame only once opened", () => {
    const collapsed = view({ hasQuoted: true, quotedHtml: "<p>older</p>" });
    expect(collapsed).toContain("Quoted text");
    expect(collapsed).not.toContain("older");

    const open = view(
      { hasQuoted: true, quotedHtml: "<p>older</p>" },
      { quotedOpen: true },
    );
    expect(open).toContain("Hide quoted text");
    expect(open).toContain("older");
    // Second frame, same sandbox.
    expect(open.match(/sandbox="allow-same-origin allow-popups"/g)).toHaveLength(2);
  });
});

// ---------------------------------------------------------------------------
// navigation
// ---------------------------------------------------------------------------

describe("link handling", () => {
  it("accepts only the four schemes the sanitizer allows", () => {
    expect(externalUrl("https://example.com/x")).toBe("https://example.com/x");
    expect(externalUrl("mailto:a@b.test")).toBe("mailto:a@b.test");
    expect(externalUrl("tel:+15551234")).toBe("tel:+15551234");
    for (const hostile of [
      "javascript:alert(1)",
      "data:text/html,<script>x</script>",
      "file:///etc/passwd",
      "vbscript:x",
      "/relative",
      "",
      null,
      undefined,
    ]) {
      expect(externalUrl(hostile)).toBeNull();
    }
  });

  /*
   * The link that was reported twice.
   *
   * "Update payment method" in a Stripe billing mail: an `<a>` styled as a
   * button, wrapping a `<span>`, on a click-tracking URL that has a whole
   * second URL percent-encoded inside its path. Every one of those was a
   * candidate cause and none of them was — the href survives the sanitizer
   * unchanged and parses — but a URL this shape is exactly what a later
   * "tighten the parsing" change would break, so it is pinned here verbatim.
   */
  const STRIPE_BUTTON_HREF =
    "https://59.email.stripe.com/CL0/https:%2F%2Fbilling.stripe.com%2Fp%2Flogin%2F00g5kTbIE9S95448ww%3Freferer=upcoming_invoice/1/0101019fe6bf4100-2bdba2b0-dbea-4014-8a08-aca9987e05d5-000000/vtJFBTXU0xwGwuHByHespJ0_YUwby5osV8XV3vnCkA8=452";

  it("opens a tracking URL with a second URL encoded inside it", () => {
    expect(externalUrl(STRIPE_BUTTON_HREF)).toBe(STRIPE_BUTTON_HREF);
  });

  it("classifies that URL the way the Rust navigation guard has to", () => {
    // The guard's rule is the mirror of this one: four schemes, and a host that
    // is not the app itself. `is_external_link` in `ipc::render` is tested on
    // the Rust side; this is the frontend's half of the same agreement.
    const parsed = new URL(STRIPE_BUTTON_HREF);
    expect(parsed.protocol).toBe("https:");
    expect(parsed.host).toBe("59.email.stripe.com");
  });
});

// ---------------------------------------------------------------------------
// a failed link is not silent
// ---------------------------------------------------------------------------

describe("link failures", () => {
  it("reaches a listener from inside the window", () => {
    const seen: string[] = [];
    const off = subscribeLinkFailures((message) => seen.push(message));
    reportLinkFailure("Could not open that link: nope");
    off();
    reportLinkFailure("after unsubscribing");
    expect(seen).toEqual(["Could not open that link: nope"]);
  });

  it("reaches the same listener from Rust, which is where the app opens links", () => {
    const seen: string[] = [];
    let deliver: ((payload: { message?: string } | null) => void) | null = null;
    const off = subscribeLinkFailures(
      (message) => seen.push(message),
      async (event, handler) => {
        expect(event).toBe(LINK_FAILED_EVENT);
        deliver = handler;
        return () => {
          deliver = null;
        };
      },
    );
    return Promise.resolve().then(() => {
      deliver?.({ message: "Could not open that link: no browser" });
      deliver?.(null);
      deliver?.({});
      off();
      expect(seen).toEqual(["Could not open that link: no browser"]);
    });
  });
});

// ---------------------------------------------------------------------------
// height
// ---------------------------------------------------------------------------

describe("frame height", () => {
  it("cannot be talked into an unbounded layout", () => {
    expect(clampFrameHeight(1e9)).toBe(MAX_FRAME_HEIGHT);
    expect(clampFrameHeight(-5)).toBeGreaterThan(0);
    expect(clampFrameHeight(Number.NaN)).toBeGreaterThan(0);
    expect(clampFrameHeight(640)).toBe(640);
  });
});

// ---------------------------------------------------------------------------
// the payload
// ---------------------------------------------------------------------------

describe("the wire payload", () => {
  it("survives a backend that omitted half of it", () => {
    const mapped = mapRenderedMessage({}, 7);
    expect(mapped).toEqual({
      messageId: 7,
      format: "empty",
      remoteImagesAllowed: false,
      // Absent means "not evicted", which is the answer that costs nothing: a
      // backend that forgot the field must not make the reading pane ask Gmail
      // for a body on every open.
      htmlEvicted: false,
      html: "",
      quotedHtml: "",
      hasQuoted: false,
      blockedRemoteImages: 0,
      blockedTrackers: 0,
      inlineCidImages: 0,
      inlineDataImages: 0,
    });
  });

  it("reads the camelCase keys Rust actually sends", () => {
    const mapped = mapRenderedMessage(
      {
        messageId: 12,
        format: "html",
        remoteImagesAllowed: true,
        html: "<p>x</p>",
        quotedHtml: "<p>y</p>",
        hasQuoted: true,
        blockedRemoteImages: 2,
        inlineCidImages: 1,
        inlineDataImages: 4,
      },
      12,
    );
    expect(mapped.format).toBe("html");
    expect(mapped.blockedRemoteImages).toBe(2);
    expect(mapped.hasQuoted).toBe(true);
  });

  it("escapes rather than trusts the browser-only fallback body", () => {
    const local = localTextRender(3, "<script>alert(1)</script> hi");
    expect(local.html).toContain("&lt;script&gt;");
    expect(local.html).not.toContain("<script");
    expect(localTextRender(3, "   ").format).toBe("empty");
  });
});

// ---------------------------------------------------------------------------
// frame height
//
// The reading pane jittered because measuring the frame changed the thing being
// measured. `nextFrameHeight` is the rule that makes that terminate; these are
// the cases that matter, written as the sequences the observer actually feeds it.
// ---------------------------------------------------------------------------

describe("the frame height rule", () => {
  /** One width, held constant — what a settling document produces. */
  const WIDTH = 600;

  /**
   * Run a measurement sequence the way `MessageFrame` does, at one width.
   *
   * Every measurement goes through the `setSize` updater **twice** from the
   * same base state, because that is what React does: `main.tsx` mounts the app
   * in `StrictMode`, which double-invokes every state updater, and the value it
   * commits is the last one returned. A rule that reads a peak it also writes
   * gives two different answers to those two calls — see [`nextFrameSize`].
   */
  function settle(measurements: number[], start = MIN_FRAME_HEIGHT) {
    let size: FrameSize = { height: start, peak: MIN_FRAME_HEIGHT, width: WIDTH };
    const history: number[] = [];
    for (const measured of measurements) {
      const once = nextFrameSize(size, measured, WIDTH);
      const twice = nextFrameSize(size, measured, WIDTH);
      // The whole point: the second call must not see anything the first did.
      expect(twice).toEqual(once);
      size = twice;
      history.push(size.height);
    }
    return { height: size.height, history };
  }

  it("settles the two-value oscillation that caused the flicker", () => {
    // The real one, off a Runpod newsletter: the frame's own scrollbar rewraps
    // the body by 33px, so the measurement alternates forever.
    const flapping = Array.from({ length: 40 }, (_, i) => (i % 2 === 0 ? 2436 : 2403));
    const { height, history } = settle(flapping);
    // It moves once and then never again.
    expect(new Set(history.slice(1))).toEqual(new Set([height]));
    // And it settles on the taller value: too short clips the message.
    expect(height).toBe(2436);
  });

  it("takes the first measurement whatever it is", () => {
    expect(settle([2436]).height).toBe(2436);
    expect(settle([120]).height).toBe(120);
  });

  it("still grows for content that genuinely got taller", () => {
    // A late-loading image is the normal case, and growth is the one direction
    // that never has to be second-guessed.
    expect(settle([400, 300, 900]).height).toBe(900);
  });

  it("refuses every shrink while the width is unchanged", () => {
    // Too tall leaves a band of empty space at the foot of a message. Too short
    // clips it, and re-measuring to fix that is the loop.
    expect(settle([900, 400]).height).toBe(900);
    expect(nextFrameHeight(900, 400, 900)).toBe(900);
    expect(nextFrameHeight(900, 899, 900)).toBe(900);
  });

  it("comes back down once the frame has been laid out at a new width", () => {
    // Dragging the splitter wider makes a message genuinely shorter, and the
    // peak from the old width is not evidence about the new one. `MessageFrame`
    // resets it; here that is the peak going back to the floor.
    expect(nextFrameHeight(900, 400, MIN_FRAME_HEIGHT)).toBe(400);
  });

  it("ignores sub-pixel churn in both directions", () => {
    expect(nextFrameHeight(500, 500 + FRAME_HEIGHT_EPSILON, 500)).toBe(500);
    expect(nextFrameHeight(500, 500 - FRAME_HEIGHT_EPSILON, 500)).toBe(500);
    expect(nextFrameHeight(500, 502, 500)).toBe(502);
  });

  it("keeps clamping a hostile measurement", () => {
    expect(nextFrameHeight(500, 10_000_000, 500)).toBe(MAX_FRAME_HEIGHT);
    // Nonsense in the other direction cannot collapse an open message either:
    // it clamps to the floor, and the floor is never taller than the peak.
    expect(nextFrameHeight(500, -1, 500)).toBe(500);
    expect(nextFrameHeight(500, Number.NaN, 500)).toBe(500);
    // Only from the floor itself is there nothing to protect.
    expect(nextFrameHeight(MIN_FRAME_HEIGHT, -1, MIN_FRAME_HEIGHT)).toBe(MIN_FRAME_HEIGHT);
  });

  it("converges for every sequence, including an adversarial one", () => {
    // The property that matters: whatever a hostile body reports, the height
    // changes finitely many times, because every change is strictly upward.
    let peak = MIN_FRAME_HEIGHT;
    let height = MIN_FRAME_HEIGHT;
    const applied: number[] = [];
    for (let i = 0; i < 5000; i++) {
      const measured = [100, 900, 100, 900, 450, 2400, 100, 899, 2400][i % 9]!;
      const next = nextFrameHeight(height, measured, peak);
      if (next !== height) {
        peak = Math.max(peak, next);
        applied.push(next);
      }
      height = next;
    }
    expect(applied).toEqual([100, 900, 2400]);
    // Strictly increasing, which is why it must stop.
    expect([...applied].sort((a, b) => a - b)).toEqual(applied);
    expect(new Set(applied).size).toBe(applied.length);
  });

  it("cannot be driven up and down by alternating widths", () => {
    // A reader dragging the splitter resets the peak on every step, so this is
    // the worst case for the one direction that is allowed to shrink. It is
    // still not a loop: each height follows the width it was measured at, and
    // the width is never a function of the height.
    let height = MIN_FRAME_HEIGHT;
    for (let i = 0; i < 200; i++) {
      const measured = i % 2 === 0 ? 900 : 400;
      // The width changed, so `MessageFrame` puts the peak back on the floor.
      height = nextFrameHeight(height, measured, MIN_FRAME_HEIGHT);
      expect(height).toBe(measured);
    }
  });
});

// ---------------------------------------------------------------------------
// the last line of a GitHub notification
//
// The frame settled one or two lines shorter than its content and the final
// line was cut through the middle. The arithmetic was right; the update was
// applied twice. React calls a state updater more than once for one update —
// always twice under `StrictMode`, which is how `main.tsx` mounts the app — and
// the old updater raised the peak as a side effect, so the second call saw a
// peak it had just raised, decided its own growth was not growth, and returned
// the height it started with. React commits the last answer.
//
// These are that update, in the numbers WKWebView reported.
// ---------------------------------------------------------------------------

describe("a measurement React applies twice", () => {
  /** The frame as it stood after first paint, before the pane grew a scrollbar. */
  const settled: FrameSize = { height: 2550, peak: 2550, width: 579 };

  it("keeps the growth the reading pane's own scrollbar caused", () => {
    // The message is tall enough that the pane scrolls, which takes 10px off
    // the frame and rewraps the body two lines taller. Nothing re-measures
    // afterwards, because nothing changes afterwards: this one update is the
    // only chance to be right.
    const once = nextFrameSize(settled, 2596, 569);
    expect(once.height).toBe(2596);

    // Called again with the state it was called with the first time.
    expect(nextFrameSize(settled, 2596, 569)).toEqual(once);

    // And again with what the first call returned, which is what the next
    // observer callback hands it.
    expect(nextFrameSize(once, 2596, 569)).toBe(once);
  });

  it("gives the same answer however many times it is called", () => {
    // Growth, a refused shrink, a width change, and sub-pixel churn — the four
    // outcomes, each one asked for ten times over.
    const cases: Array<[FrameSize, number, number]> = [
      [settled, 2596, 569],
      [settled, 1200, 579],
      [settled, 1200, 700],
      [settled, 2551, 579],
      [{ height: 900, peak: 900, width: 400 }, 4000, 400],
      [INITIAL_FRAME_SIZE, 2596, 569],
    ];
    for (const [state, measured, width] of cases) {
      const first = nextFrameSize(state, measured, width);
      for (let i = 0; i < 10; i++) {
        expect(nextFrameSize(state, measured, width)).toEqual(first);
      }
      // Idempotent on its own output too, so a re-measure that found nothing
      // new re-renders nothing.
      expect(nextFrameSize(first, measured, width)).toBe(first);
    }
  });

  it("still refuses to shrink at a width it has already been measured at", () => {
    // The oscillation this rule exists for: a frame a pixel short grows a
    // scrollbar, the scrollbar rewraps the body taller, and the height that
    // fixes it is the height that caused it. Growth is still the only
    // direction at a fixed width.
    const grown = nextFrameSize(settled, 2596, 569);
    expect(nextFrameSize(grown, 2550, 569).height).toBe(2596);
    expect(nextFrameSize(grown, 4, 569).height).toBe(2596);
  });

  it("takes a new width as a new layout, and only then comes back down", () => {
    // The reader drags the splitter. The peak from the old width is not
    // evidence about the new one.
    const wider = nextFrameSize(settled, 1800, 900);
    expect(wider).toEqual({ height: 1800, peak: 1800, width: 900 });
    // Twice, again: this is the path that resets the peak, so it is the path
    // most easily broken by an updater that remembers anything.
    expect(nextFrameSize(settled, 1800, 900)).toEqual(wider);
  });

  it("does not lock a frame at a height it never applied", () => {
    // A first measurement at a new width that lands inside the epsilon is not
    // an applied height, so it must not become the peak — otherwise the frame
    // could never come down at the width the reader just dragged it to.
    const nudged = nextFrameSize(settled, 2550, 700);
    expect(nudged.height).toBe(2550);
    expect(nextFrameSize(nudged, 1400, 700).height).toBe(1400);
  });

  it("forgets everything when the document is replaced", () => {
    // A new message is new content. The height already applied is kept until
    // the new document is measured — it is wrong, but by less than 28px would
    // be — and the peak that would refuse a shorter message is dropped.
    const fresh = resetFrameSize({ height: 2596, peak: 2596, width: 569 });
    expect(fresh).toEqual({ height: 2596, peak: MIN_FRAME_HEIGHT, width: 0 });
    expect(resetFrameSize(fresh)).toBe(fresh);
    // A one-line reply in the frame a newsletter just left.
    expect(nextFrameSize(fresh, 120, 569).height).toBe(120);
  });
});

// ---------------------------------------------------------------------------
// width
//
// A twelve-column table does not fit in a reading pane, and the question is
// only where the sideways scrolling goes. These are the decisions
// `containWideContent` makes; `MessageFrame` supplies the live DOM.
// ---------------------------------------------------------------------------

describe("containing content too wide for the pane", () => {
  interface Fake extends WideCandidate {
    wraps: number;
  }

  function fake(over: Partial<WideCandidate> & { scrollWidth: number }): Fake {
    const candidate: Fake = {
      parentTagName: "DIV",
      handled: false,
      wraps: 0,
      wrap() {
        candidate.wraps += 1;
      },
      ...over,
    } as Fake;
    return candidate;
  }

  const PANE = 595;

  it("leaves an ordinary message completely alone", () => {
    const fits = [fake({ scrollWidth: 400 }), fake({ scrollWidth: PANE })];
    expect(containWideContent(fits, PANE)).toBe(0);
    expect(fits.every((c) => c.wraps === 0)).toBe(true);
  });

  it("forgives a sub-pixel overshoot rather than wrapping for half a pixel", () => {
    expect(containWideContent([fake({ scrollWidth: PANE + 1 })], PANE)).toBe(0);
    expect(containWideContent([fake({ scrollWidth: PANE + 2 })], PANE)).toBe(1);
  });

  it("wraps the table that is genuinely too wide", () => {
    // The real one: the Metabase alert's results table measures 2943px against
    // a 595px pane.
    const table = fake({ scrollWidth: 2943 });
    expect(containWideContent([table], PANE)).toBe(1);
    expect(table.wraps).toBe(1);
  });

  it("wraps the inner table and leaves the layout tables around it", () => {
    // Mail is nested tables. Wrapping the outer one would scroll the whole
    // message sideways, which is the thing being fixed — so the candidates
    // arrive deepest first, and the ancestors are back under the width by the
    // time they are asked. That only works because the width is read live.
    let innerWrapped = false;
    let outerWraps = 0;
    const inner: WideCandidate = {
      scrollWidth: 2943,
      parentTagName: "TD",
      handled: false,
      wrap: () => {
        innerWrapped = true;
      },
    };
    const outer: WideCandidate = {
      get scrollWidth() {
        return innerWrapped ? PANE : 2943;
      },
      parentTagName: "BODY",
      handled: false,
      wrap: () => {
        outerWraps += 1;
      },
    };
    expect(containWideContent([inner, outer], PANE)).toBe(1);
    expect(innerWrapped).toBe(true);
    expect(outerWraps).toBe(0);
  });

  it("never puts a wrapper where the parser would move it", () => {
    for (const parentTagName of ["TABLE", "THEAD", "TBODY", "TFOOT", "TR"]) {
      const stray = fake({ scrollWidth: 4000, parentTagName });
      expect(containWideContent([stray], PANE)).toBe(0);
    }
  });

  it("does not wrap what is already a scroller", () => {
    // A `<pre>` scrolls itself, and reports its *content* width — so it would
    // otherwise look permanently too wide and collect a wrapper on every pass.
    const already = fake({ scrollWidth: 4000, handled: true });
    expect(containWideContent([already], PANE)).toBe(0);
    expect(already.wraps).toBe(0);
  });

  it("is idempotent, because it runs again on every resize", () => {
    // A wrapped element goes on measuring wide — it is the content of the
    // scroller now — so without `handled` every resize would nest another one.
    let wraps = 0;
    const table: WideCandidate = {
      scrollWidth: 2943,
      parentTagName: "DIV",
      get handled() {
        return wraps > 0;
      },
      wrap: () => {
        wraps += 1;
      },
    };
    for (let i = 0; i < 20; i++) containWideContent([table], PANE);
    expect(wraps).toBe(1);
  });

  it("does nothing at all before the frame has a width", () => {
    // The first pass can land before layout. Zero would make everything look
    // too wide and wrap the entire message.
    const table = fake({ scrollWidth: 2943 });
    expect(containWideContent([table], 0)).toBe(0);
    expect(containWideContent([table], Number.NaN)).toBe(0);
    expect(table.wraps).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// the frame stylesheet
// ---------------------------------------------------------------------------

describe("the frame stylesheet", () => {
  const css = frameDocument({ html: "", allowRemoteImages: false, format: "html" });

  it("gives the frame nothing to scroll vertically", () => {
    // The reading pane is the only vertical scroll. A frame that can scroll
    // itself is the second scrollbar the reader was complaining about, and it
    // arrives whether or not the content is tall: a horizontal scrollbar eats
    // enough of the viewport to summon one.
    expect(css).toContain("html:not(.mach-capped){overflow-x:auto;overflow-y:hidden");
  });

  it("still lets a clamped frame scroll, so nothing is unreachable", () => {
    // Past MAX_FRAME_HEIGHT the height no longer tracks the content, and this
    // is the only way to the rest of the message.
    expect(css).not.toContain("html{overflow");
    expect(css).toMatch(/html:not\(\.mach-capped\)/);
  });

  it("styles the scroller the width pass adds", () => {
    expect(css).toContain("[data-mach-scroll]{max-width:100%;overflow-x:auto;overflow-y:hidden}");
    // ...and lets the wide thing inside it actually be wide.
    expect(css).toContain("[data-mach-scroll]>table{max-width:none}");
  });

  it("keeps a code block's lines instead of reflowing them", () => {
    expect(css).toContain("pre{white-space:pre;max-width:100%;overflow-x:auto;overflow-y:hidden}");
  });

  /*
   * The collapsed column.
   *
   * `word-break:break-word` reads like a stronger `overflow-wrap:break-word`
   * and is a different property: it also tells table layout that this content
   * can be one character wide, which lets a sender's `width:24px` win over the
   * word in the cell. A GitHub Actions notification rendered its "Status"
   * header vertically, one letter per line, because of it — 25.9px wide and
   * 176px tall, measured.
   */
  it("does not make one character the narrowest a table column can be", () => {
    expect(css).toContain("overflow-wrap:break-word");
    expect(css).not.toContain("word-break");
  });
});

// ---------------------------------------------------------------------------
// the ground
// ---------------------------------------------------------------------------

/*
 * Dark mode used to make remote mail unreadable.
 *
 * The app's `--foreground` was injected into the sender's document, so
 * everything the sender did not colour itself inherited near-white — on
 * backgrounds that still came from the sender's CSS, which is white. A GitHub
 * Actions notification arrived with its heading, its "All jobs have failed"
 * line and its "Status / Job / Annotations" table header invisible, while the
 * footer and the links, which that mail colours explicitly, were fine.
 */
describe("the frame's ground", () => {
  const DARK: FrameTokens = {
    "--foreground": "oklch(0.985 0 0)",
    "--muted-foreground": "oklch(0.708 0 0)",
    "--faint-foreground": "oklch(0.556 0 0)",
    "--background": "oklch(0.145 0 0)",
    "--border": "oklch(0.269 0 0)",
    "--accent": "oklch(0.62 0.19 255)",
  };

  const html = frameDocument({
    html: "<p>hi</p>",
    allowRemoteImages: false,
    format: "html",
    tokens: DARK,
  });
  const text = frameDocument({
    html: "<div>hi</div>",
    allowRemoteImages: false,
    format: "text",
    tokens: DARK,
    // The tokens and the scheme they belong to, together, the way
    // `MessageFrame` reads them off the app root. Passing DARK on its own would
    // describe a window that does not exist.
    scheme: "dark",
  });

  it("never puts the app's dark ink into a sender's document", () => {
    // By declaration rather than by value: two of the dark values are also
    // light values, under different names.
    for (const [name, value] of Object.entries(DARK)) {
      expect(html).not.toContain(`${name}:${value}`);
    }
  });

  it("gives a sender's document the light palette and an opaque light page", () => {
    expect(html).toContain("--foreground:oklch(0.145 0 0)");
    expect(html).toContain("--background:oklch(1 0 0)");
    expect(html).toContain("background:var(--background,#fff)");
    expect(html).not.toContain("background:transparent");
  });

  it("fixes a sender's document to the colour scheme the mail was written for", () => {
    // The app is dark here and the sender's document is still light, scheme
    // included: the mail was written for a white page and the engine's own
    // colours have to land on one.
    expect(html).toContain("color-scheme:light}");
    expect(html).not.toContain("color-scheme:dark");
  });

  it("leaves plain text following the app theme", () => {
    // This half is our own document, not the sender's: text we turned into
    // HTML, with no colour of its own to conflict with.
    expect(text).toContain("--foreground:oklch(0.985 0 0)");
    expect(text).toContain("--background:oklch(0.145 0 0)");
    expect(text).toContain("color-scheme:dark");
    expect(text).toContain("background:transparent");
  });

  /*
   * This is the assertion the old pair got wrong, so it is spelled out on its
   * own.
   *
   * It used to say `color-scheme: light dark`, which reads as "follow whatever
   * is in force" and is not what it does inside a frame: the engine resolves
   * the pair from `prefers-color-scheme`, which is the *desktop's* preference.
   * The app's is a setting of its own, and on a Linux desktop the two diverge
   * by default — theme `light`, GNOME `prefer-dark` — so the app's light tokens
   * were painted onto a canvas WebKitGTK had resolved to dark, on a ground
   * whose background is transparent and therefore has nothing else to say what
   * colour it is. Plain-text mail was unreadable.
   *
   * A `theme` ground names one scheme, always, and it is the app's. See
   * [`FrameScheme`].
   */
  it("never asks a frame to resolve the scheme for itself", () => {
    expect(text).not.toContain("light dark");
    const asLight = frameDocument({
      html: "<div>hi</div>",
      allowRemoteImages: false,
      format: "text",
      scheme: "light",
    });
    expect(asLight).toContain("color-scheme:light}");
    expect(asLight).not.toContain("light dark");
  });

  it("treats a snippet as text and an empty body as text", () => {
    expect(frameGround("html")).toBe("light");
    expect(frameGround("text")).toBe("theme");
    expect(frameGround("snippet")).toBe("theme");
    expect(frameGround("empty")).toBe("theme");
  });

  /*
   * Both frames say `light` here and that is the point rather than a weakness:
   * these tests run with no `document`, so `MessageFrame` has no app root to
   * read and falls back to the light stylesheet — which is what the two grounds
   * agree on when the app is light. What separates them is the background, so
   * that is what this asserts. The scheme itself is pinned above, against a
   * `frameDocument` told which app it is rendering for.
   */
  it("reaches the frame the reading pane actually renders", () => {
    const asHtml = attribute(view({ format: "html" }), "srcdoc") ?? "";
    const asText = attribute(view({ format: "text" }), "srcdoc") ?? "";
    expect(asHtml).toContain("color-scheme:light}");
    expect(asHtml).toContain("background:var(--background,#fff)");
    expect(asText).toContain("color-scheme:light}");
    expect(asText).toContain("background:transparent");
  });

  it("gives the quoted history the same ground as the message it came from", () => {
    const markup = view(
      { format: "html", hasQuoted: true, quotedHtml: "<p>older</p>" },
      { quotedOpen: true },
    );
    const frames = [...markup.matchAll(/srcdoc="([^"]*)"/gi)].map((m) => m[1]!);
    expect(frames).toHaveLength(2);
    for (const frame of frames) expect(frame).toContain("color-scheme:light}");
  });

  /*
   * LIGHT_GROUND is a hand copy of :root in globals.css, made by hand because
   * it cannot be read off the app: when the window is dark, `.dark` has
   * already replaced those tokens, and the light values are no longer in the
   * cascade to find. Nothing keeps a hand copy honest but a test — retune
   * `--foreground` or `--border` in globals.css and this is what would
   * otherwise stay silently wrong, forever, in every HTML message.
   *
   * The stylesheet declares far more tokens than the frame uses, so this only
   * walks LIGHT_GROUND's own keys against `:root` — never the reverse — and
   * it parses `:root` specifically. `.dark` redefines several of the same
   * names to different values, so finding one by name alone, anywhere in the
   * file, would risk a match that happens to pass while proving nothing.
   */
  it("keeps LIGHT_GROUND pinned to :root in globals.css", () => {
    const source = readFileSync(new URL("../../lib/message-body.ts", import.meta.url), "utf8");
    const literal = source.match(/const LIGHT_GROUND: FrameTokens = \{([\s\S]*?)\n\};/);
    expect(literal, "LIGHT_GROUND not found in src/lib/message-body.ts").not.toBeNull();
    const declared = [...literal![1].matchAll(/"(--[\w-]+)":\s*"([^"]+)"/g)].map(
      ([, name, value]) => [name, value] as const,
    );
    expect(declared.length).toBeGreaterThan(0);

    const css = readFileSync(new URL("../../styles/globals.css", import.meta.url), "utf8");
    const root = css.match(/:root\s*\{([\s\S]*?)\n\}/);
    expect(root, ":root block not found in src/styles/globals.css").not.toBeNull();

    for (const [name, litValue] of declared) {
      const declaration = root![1].match(new RegExp(`${name}:\\s*([^;]+);`));
      expect(declaration, `${name} is in LIGHT_GROUND but not declared in :root`).not.toBeNull();
      const cssValue = declaration![1].trim();
      expect(
        litValue,
        `${name} has drifted: LIGHT_GROUND in src/lib/message-body.ts has "${litValue}", ` +
          `:root in src/styles/globals.css has "${cssValue}". Update LIGHT_GROUND to match.`,
      ).toBe(cssValue);
    }
  });
});

// ---------------------------------------------------------------------------
// trackers
// ---------------------------------------------------------------------------

describe("trackers", () => {
  it("drops them silently — no bar, nothing to click", () => {
    // Feedback from daily use: "don’t show ‘trackers’ blocked. idc." Trackers are gone and are
    // not coming back, so there is no decision to surface. The count still
    // rides on the payload for anything that wants it later.
    // Assert on the bar's own wording rather than the word "tracker", which
    // legitimately appears in the frame stylesheet that hides the pixels.
    const markup = view({ blockedTrackers: 7 });
    expect(markup).not.toMatch(/\d+\s*trackers?\s*blocked/i);
    expect(markup).not.toContain("7 trackers");
  });
});
