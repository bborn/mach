/**
 * The WebView half of the hostile-message suite.
 *
 * `src-tauri/tests/render_hostile.rs` asks what the sanitizer emits. This asks
 * what the frame the emission lands in is allowed to do with it, and it asks
 * the one question the sanitizer structurally cannot answer: a phishing link is
 * valid HTML pointing at a real host, so nothing upstream of the reader's eye
 * can tell it from a link they wanted.
 */

import { describe, expect, it } from "vitest";

import {
  claimedHost,
  discloseLinkTargets,
  frameCsp,
  frameDocument,
  linkClaim,
  registrableDomain,
  CLAIM_ATTR,
  FRAME_SANDBOX,
  type AnchorCandidate,
} from "@/lib/message-body";

// ---------------------------------------------------------------------------
// Containment, stated rather than assumed
// ---------------------------------------------------------------------------

describe("what the message frame is allowed to do", () => {
  /*
   * Nothing in a message body can reach `window.parent`, the Tauri IPC bridge
   * or `localStorage`, and the reason is not that the sanitizer removed the
   * code that would try. It is that there is no way to run code at all, and
   * that holds independently three times over:
   *
   *  1. the sandbox has no `allow-scripts`, so the document's scripting is
   *     disabled at the engine level — which is also why no listener attached
   *     from the parent ever fires in WebKit (see `FRAME_SANDBOX`);
   *  2. the frame CSP is `script-src 'none'` with `default-src 'none'` under
   *     it, so even a script that got into the markup has no source it may run;
   *  3. the sanitizer emits no script, no event handler and no `javascript:`.
   *
   * Any one of those failing leaves the other two. The first is the one that
   * survives an ammonia CVE, which is why it is written first.
   */
  it("cannot run anything, by three independent mechanisms", () => {
    expect(FRAME_SANDBOX.split(" ")).not.toContain("allow-scripts");
    const csp = frameCsp(true);
    expect(csp).toContain("default-src 'none'");
    expect(csp).toContain("script-src 'none'");
  });

  it("cannot navigate itself, submit anywhere, or nest a frame", () => {
    const csp = frameCsp(true);
    expect(csp).toContain("base-uri 'none'");
    expect(csp).toContain("form-action 'none'");
    expect(csp).toContain("frame-src 'none'");
    expect(csp).toContain("object-src 'none'");
    for (const never of [
      "allow-top-navigation",
      "allow-forms",
      "allow-modals",
      "allow-downloads",
      "allow-popups-to-escape-sandbox",
      "allow-pointer-lock",
      "allow-presentation",
    ]) {
      expect(FRAME_SANDBOX).not.toContain(never);
    }
  });

  /*
   * The directives that are *absent* matter as much as the ones present, and
   * only because the policy starts from `default-src 'none'`. A policy that
   * started anywhere else would leave `connect-src`, `font-src` and `media-src`
   * at their defaults, and each of those is a request leaving the machine.
   */
  it("leaves every unnamed directive falling back to none", () => {
    const csp = frameCsp(false);
    for (const unnamed of ["connect-src", "font-src", "media-src", "child-src", "worker-src"]) {
      expect(csp).not.toContain(unnamed);
    }
    expect(csp.startsWith("default-src 'none'")).toBe(true);
    expect(csp).toContain("img-src data:");
    expect(csp).not.toContain("https:");
  });

  it("is the only place a body can name a remote host, and only once asked", () => {
    const body = `<img data-mach-blocked-src="https://evil.example/p.gif" src="data:image/gif;base64,R0lGODlh">`;
    const blocked = frameDocument({ html: body, allowRemoteImages: false, format: "html" });
    // The URL is in the document — it has to be, or "load images" would have
    // nothing to load — and the policy is what makes it unfetchable.
    expect(blocked).toContain("evil.example");
    expect(blocked).toContain("img-src data:");
    expect(blocked).not.toContain("img-src data: https:");
  });

  it("sends no referrer, by the attribute and by the document", () => {
    const doc = frameDocument({ html: "<p>hi</p>", allowRemoteImages: true, format: "html" });
    expect(doc).toContain('<meta name="referrer" content="no-referrer">');
  });
});

// ---------------------------------------------------------------------------
// Where a link actually goes
// ---------------------------------------------------------------------------

describe("registrable domain", () => {
  it("treats subdomains of one site as one site", () => {
    expect(registrableDomain("click.mail.example.com")).toBe("example.com");
    expect(registrableDomain("example.com")).toBe("example.com");
    expect(registrableDomain("www.example.com")).toBe("example.com");
    expect(registrableDomain("EXAMPLE.COM.")).toBe("example.com");
  });

  /*
   * The case that makes a two-label rule wrong rather than merely approximate:
   * under `.co.uk` the last two labels are the suffix, so `hsbc.co.uk` and
   * `evil.co.uk` would compare equal and the disclosure would never fire on a
   * whole country's banks.
   */
  it("does not read a country's second level as a site", () => {
    expect(registrableDomain("hsbc.co.uk")).toBe("hsbc.co.uk");
    expect(registrableDomain("secure.hsbc.co.uk")).toBe("hsbc.co.uk");
    expect(registrableDomain("evil.co.uk")).not.toBe(registrableDomain("hsbc.co.uk"));
    expect(registrableDomain("shop.com.au")).toBe("shop.com.au");
  });
});

describe("what a link's text claims", () => {
  it("reads an explicit URL as a claim", () => {
    expect(claimedHost("https://paypal.com/signin")).toBe("paypal.com");
    expect(claimedHost("http://www.paypal.com")).toBe("paypal.com");
    expect(claimedHost("  https://paypal.com  ")).toBe("paypal.com");
    expect(claimedHost("www.paypal.com")).toBe("paypal.com");
  });

  it("reads a bare domain as a claim when it is plausibly one", () => {
    expect(claimedHost("paypal.com")).toBe("paypal.com");
    expect(claimedHost("secure.hsbc.co.uk")).toBe("secure.hsbc.co.uk");
    expect(claimedHost("Apple.COM")).toBe("apple.com");
  });

  /*
   * The false-alarm cases, which are the ones that decide whether this feature
   * is worth having. Ordinary link text is full of things with a dot in them,
   * and every one of these read as a hostname would put a warning on a link
   * that was telling the truth.
   */
  it("does not mistake ordinary prose for a domain name", () => {
    for (const text of [
      "Update your payment method",
      "Click here",
      "README.md",
      "setup.sh",
      "invoice.pdf",
      "v1.2.3",
      "Acme Inc.",
      "see p. 4",
      "",
      "   ",
      "a.b",
      // Closed up into one token this parses as a punycode host ending in
      // `.com`, which is why whitespace disqualifies a bare candidate.
      "Buy now — only at shop.com",
    ]) {
      expect(claimedHost(text)).toBeNull();
    }
  });

  it("still reads a long URL that wrapped across lines", () => {
    expect(claimedHost("https://example.com/a/very/long\n/path?x=1")).toBe("example.com");
  });

  /*
   * The other half of the homograph case. Here the imitation is in the *text*
   * and the destination is an ordinary domain, so the punycode rule below never
   * fires — this is caught as a plain mismatch, because the URL parser turns
   * the Cyrillic into `xn--` and the two hosts then visibly differ.
   */
  it("normalizes a homograph in the text the same way the href was normalized", () => {
    expect(claimedHost("аpple.com")).toBe("xn--pple-43d.com");
  });
});

describe("disclosing where a link goes", () => {
  it("says nothing about the links in ordinary mail", () => {
    for (const [text, href] of [
      ["Update your payment method", "https://click.mailer.test/ls/x?u=abc"],
      ["View order", "https://shop.example.com/orders"],
      ["example.com", "https://example.com/x"],
      ["example.com", "https://www.example.com/x"],
      ["www.example.com", "https://click.example.com/track/1"],
      ["https://example.com/pricing", "https://example.com/pricing?utm=1"],
      ["support@example.com", "mailto:support@example.com"],
      ["+1 555 0100", "tel:+15550100"],
      ["", "https://example.com/logo"],
    ] as const) {
      expect(linkClaim(text, href)).toBeNull();
    }
  });

  it("names the real host when the text names a different one", () => {
    expect(linkClaim("paypal.com", "https://paypa1-secure.test/login")).toEqual({
      host: "paypa1-secure.test",
      reason: "mismatch",
    });
    expect(linkClaim("https://www.hsbc.co.uk/", "https://hsbc.co.uk.evil.test/")).toEqual({
      host: "hsbc.co.uk.evil.test",
      reason: "mismatch",
    });
  });

  /*
   * The homograph case, and why it needs no text rule at all. The Rust `href`
   * filter emits the URL parser's own serialization, which is IDNA-encoded — so
   * a Cyrillic `а` in the sender's domain arrives here as `xn--`. The anchor's
   * *text* is still the Unicode, and it is pixel-identical to the name it is
   * imitating. There is no reading of that text that would warn the reader, so
   * the host is disclosed whatever the text says.
   */
  it("always discloses a punycode host, whatever the text says", () => {
    expect(linkClaim("apple.com", "https://xn--pple-43d.com/login")).toEqual({
      host: "xn--pple-43d.com",
      reason: "punycode",
    });
    expect(linkClaim("Sign in", "https://xn--pple-43d.com/login")?.reason).toBe("punycode");
  });

  it("refuses anything the click path would refuse anyway", () => {
    for (const href of ["javascript:alert(1)", "data:text/html,x", "/relative", "", null]) {
      expect(linkClaim("paypal.com", href)).toBeNull();
    }
  });
});

// ---------------------------------------------------------------------------
// The pass over the document
// ---------------------------------------------------------------------------

/** One anchor, as the DOM pass in `MessageFrame` presents it. */
function anchor(text: string, href: string | null): AnchorCandidate & { shown: string[] } {
  const shown: string[] = [];
  return {
    text,
    href,
    shown,
    get disclosed() {
      return shown.length > 0;
    },
    disclose(host: string) {
      shown.push(host);
    },
  };
}

describe("the disclosure pass", () => {
  it("annotates the lying link and leaves the rest of the message alone", () => {
    const links = [
      anchor("Update your payment method", "https://click.mailer.test/x"),
      anchor("paypal.com", "https://paypa1-secure.test/login"),
      anchor("example.com", "https://mail.example.com/x"),
    ];
    expect(discloseLinkTargets(links)).toBe(1);
    expect(links.map((l) => l.shown)).toEqual([[], ["paypa1-secure.test"], []]);
  });

  /*
   * `MessageFrame` runs its DOM passes again on every resize, so a second run
   * must add nothing. The real `disclosed` reads the anchor's next sibling.
   */
  it("is idempotent, because it runs again whenever the pane is resized", () => {
    const links = [anchor("paypal.com", "https://paypa1-secure.test/login")];
    expect(discloseLinkTargets(links)).toBe(1);
    expect(discloseLinkTargets(links)).toBe(0);
    expect(links[0].shown).toEqual(["paypa1-secure.test"]);
  });

  /*
   * The disclosure is styled by one attribute, and a sender who could set that
   * attribute could put a reassuring host next to their own link. They cannot:
   * `data-mach*` is refused by name in the Rust attribute filter, and no
   * `data-` attribute is on the allowlist in the first place. Pinned on both
   * sides — `render_hostile.rs` holds the Rust half.
   */
  it("hangs off an attribute no sender can write", () => {
    expect(CLAIM_ATTR.startsWith("data-mach")).toBe(true);
  });

  it("is styled by the frame's own stylesheet, not the sender's", () => {
    const doc = frameDocument({ html: "<p>hi</p>", allowRemoteImages: true, format: "html" });
    expect(doc).toContain(`[${CLAIM_ATTR}]{`);
  });
});

/*
 * The anchors the sanitizer defused. It keeps their text — the text is the
 * message — and takes the `href`, so what is left is blue, underlined, and does
 * nothing when clicked. That is the dead click invariant 9 exists to forbid.
 */
describe("a link the sanitizer refused", () => {
  it("does not still look like a link", () => {
    const doc = frameDocument({
      html: '<a target="_blank" rel="noopener noreferrer nofollow">javascript link</a>',
      allowRemoteImages: true,
      format: "html",
    });
    expect(doc).toContain("a:not([href]){color:inherit;text-decoration:none}");
    expect(doc).toContain("javascript link");
  });
});
