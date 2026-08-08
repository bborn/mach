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

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  clampFrameHeight,
  externalUrl,
  frameCsp,
  frameDocument,
  isEffectivelyEmpty,
  localTextRender,
  mapRenderedMessage,
  nextFrameHeight,
  revealBlockedImages,
  shouldAutoExpandQuote,
  FRAME_HEIGHT_EPSILON,
  FRAME_SANDBOX,
  MAX_FRAME_HEIGHT,
  MIN_FRAME_HEIGHT,
  type BlockedImage,
  type RenderedMessage,
} from "@/lib/message-body";
import { MessageBodyView } from "./MessageBody";
import { MessageFrame } from "./MessageFrame";

function rendered(over: Partial<RenderedMessage> = {}): RenderedMessage {
  return {
    messageId: 1,
    format: "html",
    remoteImagesAllowed: false,
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
    <MessageBodyView
      rendered={rendered(over)}
      subject="Tawny"
      allowRemoteImages={false}
      onLoadRemoteImages={() => {}}
      quotedOpen={false}
      onToggleQuoted={() => {}}
      {...props}
    />,
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
    <MessageFrame html="<p>hi</p>" allowRemoteImages={false} title="Message from Tawny" />,
  );

  it("is exactly allow-same-origin", () => {
    expect(attribute(markup, "sandbox")).toBe("allow-same-origin");
    expect(FRAME_SANDBOX).toBe("allow-same-origin");
  });

  it("never grants scripts, popups, top navigation, forms or modals", () => {
    const sandbox = attribute(markup, "sandbox") ?? "";
    for (const capability of [
      "allow-scripts",
      "allow-popups",
      "allow-top-navigation",
      "allow-top-navigation-by-user-activation",
      "allow-forms",
      "allow-modals",
      "allow-downloads",
      "allow-presentation",
      "allow-pointer-lock",
    ]) {
      expect(sandbox).not.toContain(capability);
    }
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
    const doc = frameDocument({ html: "<p>hi</p>", allowRemoteImages: false });
    expect(doc).toContain(
      `<meta http-equiv="Content-Security-Policy" content="${frameCsp(false)}">`,
    );
    expect(doc).toContain("<p>hi</p>");
  });

  it("is carried in the srcdoc the component actually renders", () => {
    const markup = renderToStaticMarkup(
      <MessageFrame html="<p>hi</p>" allowRemoteImages={false} title="t" />,
    );
    const srcdoc = attribute(markup, "srcdoc") ?? "";
    expect(srcdoc).toContain("Content-Security-Policy");
    expect(srcdoc).toContain("script-src 'none'");
    expect(srcdoc).toContain("img-src data:");
    expect(srcdoc).not.toContain("img-src data: https:");
  });

  it("cannot have a stylesheet broken out of by a malformed token", () => {
    const doc = frameDocument({
      html: "",
      allowRemoteImages: false,
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
// invariant 6: quoted history
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
    expect(open.match(/sandbox="allow-same-origin"/g)).toHaveLength(2);
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
  /** Run a measurement sequence the way `MessageFrame` does. */
  function settle(measurements: number[], start = MIN_FRAME_HEIGHT) {
    const applied = new Set<number>();
    let height = start;
    const history: number[] = [];
    for (const measured of measurements) {
      const next = nextFrameHeight(height, measured, applied);
      if (next !== height) applied.add(next);
      height = next;
      history.push(height);
    }
    return { height, history };
  }

  it("settles the two-value oscillation that caused the flicker", () => {
    // The real one, off a Runpod newsletter: the frame's own scrollbar rewraps
    // the body by 33px, so the measurement alternates forever.
    const flapping = Array.from({ length: 40 }, (_, i) => (i % 2 === 0 ? 2436 : 2403));
    const { height, history } = settle(flapping);
    // It moves at most twice and then never again.
    expect(new Set(history.slice(3))).toEqual(new Set([height]));
    // And it settles on the taller value: too short clips the message.
    expect(height).toBe(2436);
  });

  it("takes the first measurement whatever it is", () => {
    expect(settle([2436]).height).toBe(2436);
    expect(settle([120]).height).toBe(120);
  });

  it("still grows for content that genuinely got taller", () => {
    // A late-loading image is the normal case and must not be mistaken for a
    // cycle, even when the frame has already been that tall before.
    expect(settle([400, 300, 900]).height).toBe(900);
  });

  it("still shrinks to a height it has not seen", () => {
    expect(settle([900, 400]).height).toBe(400);
  });

  it("refuses to shrink back to a height it already applied", () => {
    const applied = new Set([400]);
    expect(nextFrameHeight(900, 400, applied)).toBe(900);
    // ...but the same shrink is fine when that height is new.
    expect(nextFrameHeight(900, 401, applied)).toBe(401);
  });

  it("ignores sub-pixel churn in both directions", () => {
    const applied = new Set<number>();
    expect(nextFrameHeight(500, 500 + FRAME_HEIGHT_EPSILON, applied)).toBe(500);
    expect(nextFrameHeight(500, 500 - FRAME_HEIGHT_EPSILON, applied)).toBe(500);
    expect(nextFrameHeight(500, 502, applied)).toBe(502);
  });

  it("keeps clamping a hostile measurement", () => {
    expect(nextFrameHeight(500, 10_000_000, new Set())).toBe(MAX_FRAME_HEIGHT);
    expect(nextFrameHeight(500, -1, new Set())).toBe(MIN_FRAME_HEIGHT);
    expect(nextFrameHeight(500, Number.NaN, new Set())).toBe(MIN_FRAME_HEIGHT);
  });

  it("cannot loop for any sequence, including an adversarial one", () => {
    // Whatever a hostile body does to the measurement, the applied heights must
    // be finite: every value is either new or a growth, never a revisit.
    const applied = new Set<number>();
    let height = MIN_FRAME_HEIGHT;
    const seen: number[] = [];
    for (let i = 0; i < 2000; i++) {
      const measured = [100, 900, 100, 900, 450, 900, 100][i % 7]!;
      const next = nextFrameHeight(height, measured, applied);
      if (next !== height) {
        applied.add(next);
        seen.push(next);
      }
      height = next;
    }
    expect(seen.length).toBeLessThan(10);
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
