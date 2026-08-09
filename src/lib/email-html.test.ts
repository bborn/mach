// @vitest-environment jsdom
//
// The only test file in this project that needs a DOM, and it needs one for a
// reason: the paste cleaner is a DOM walker, because the markup it exists to
// survive — Word's — is not something a regular expression should be pointed at.

import { describe, expect, it } from "vitest";
import {
  cleanHtml,
  cleanStyle,
  htmlFromPlainText,
  htmlToPlainText,
  isBlankHtml,
  isSafeStyleValue,
  withHtmlSignature,
  withoutSignature,
} from "./email-html";

const doc = () => document;

/* -------------------------------------------------------------------------- */
/* Paste                                                                       */
/* -------------------------------------------------------------------------- */

/**
 * What Microsoft Word actually puts on the clipboard. Trimmed for length; every
 * feature that matters is here — the `<style>` block, the MSO conditional
 * comment carrying a second one, `class=MsoNormal` on every paragraph, `mso-`
 * properties inside otherwise-legal style attributes, and `<o:p>` elements.
 */
const WORD_PASTE = `<html xmlns:o="urn:schemas-microsoft-com:office:office"
xmlns:w="urn:schemas-microsoft-com:office:word"><head><meta charset="utf-8">
<meta name=Generator content="Microsoft Word 15">
<style><!--
p.MsoNormal, li.MsoNormal, div.MsoNormal
{margin:0in; font-size:12.0pt; font-family:"Aptos",sans-serif;}
--></style>
<!--[if gte mso 9]><xml><o:OfficeDocumentSettings><o:AllowPNG/></o:OfficeDocumentSettings></xml><![endif]-->
</head><body lang=EN-US style='word-wrap:break-word'>
<p class=MsoNormal style='margin:0in;mso-line-height-alt:12.0pt'><b><span
style='font-size:14.0pt;font-family:"Aptos",sans-serif;color:#0F4761;mso-ligatures:standardcontextual'>Quarterly
numbers</span></b><o:p></o:p></p>
<p class=MsoNormal>Revenue is <span style='mso-bidi-font-weight:bold'>up</span>.<o:p></o:p></p>
</body></html>`;

/** What Google Docs puts on the clipboard. */
const DOCS_PASTE = `<meta charset="utf-8"><b style="font-weight:normal;" id="docs-internal-guid-8f3">
<p dir="ltr" style="line-height:1.38;margin-top:0pt;margin-bottom:0pt;"><span style="font-size:11pt;font-family:Arial,sans-serif;color:#000000;background-color:transparent;font-weight:400;font-style:normal;font-variant:normal;text-decoration:none;vertical-align:baseline;white-space:pre;white-space:pre-wrap;">Ship date is </span><span style="font-size:11pt;font-family:Arial,sans-serif;color:#000000;background-color:transparent;font-weight:700;font-style:normal;font-variant:normal;text-decoration:none;vertical-align:baseline;white-space:pre;white-space:pre-wrap;">Friday</span></p>
<ul style="margin-top:0pt;margin-bottom:0pt;padding-inline-start:48px;"><li dir="ltr" style="list-style-type:disc;font-size:11pt;font-family:Arial,sans-serif;color:#000000;background-color:transparent;font-weight:400;" aria-level="1"><p dir="ltr" style="line-height:1.38;margin-top:0pt;margin-bottom:0pt;" role="presentation"><span style="font-size:11pt;">copy review</span></p></li></ul></b>`;

describe("paste", () => {
  it("keeps Word's words and none of Word's markup", () => {
    const cleaned = cleanHtml(WORD_PASTE, doc());

    expect(cleaned).not.toContain("MsoNormal");
    expect(cleaned).not.toContain("mso-");
    expect(cleaned).not.toContain("<style");
    expect(cleaned).not.toContain("<o:p");
    expect(cleaned).not.toContain("class=");
    expect(cleaned).not.toContain("<!--");
    expect(cleaned).not.toContain("AllowPNG");
    // The message itself survives, emphasis included.
    expect(cleaned).toContain("<b>");
    expect(htmlToPlainText(cleaned)).toContain("Quarterly numbers");
    expect(htmlToPlainText(cleaned)).toContain("Revenue is up.");
  });

  it("keeps Google Docs' structure and drops its stylesheet-in-an-attribute", () => {
    const cleaned = cleanHtml(DOCS_PASTE, doc());

    expect(cleaned).not.toContain("docs-internal-guid");
    expect(cleaned).not.toContain("aria-level");
    expect(cleaned).not.toContain("vertical-align");
    // Pasted typography does not travel; weight does.
    expect(cleaned).not.toContain("font-family");
    expect(cleaned).not.toContain("font-size");
    expect(cleaned).not.toContain("padding-inline-start");
    expect(cleaned).toContain("<ul");
    expect(cleaned).toContain("<li");
    // font-weight is on the list, so bold pasted from Docs stays bold.
    expect(cleaned).toContain("font-weight: 700");
    const text = htmlToPlainText(cleaned);
    expect(text).toContain("Ship date is Friday");
    expect(text).toContain("- copy review");
  });

  it("drops script, event handlers and unsafe link schemes", () => {
    const hostile =
      '<div onclick="steal()">hi<script>alert(1)</script>' +
      '<a href="javascript:alert(1)">click</a>' +
      '<img src="https://x/y.png" onerror="alert(1)"></div>';
    const cleaned = cleanHtml(hostile, doc());

    expect(cleaned).not.toContain("onclick");
    expect(cleaned).not.toContain("onerror");
    expect(cleaned).not.toContain("script");
    expect(cleaned).not.toContain("javascript:");
    // The anchor stays as an anchor with no target, so the words are not lost.
    expect(cleaned).toContain("click");
    expect(cleaned).toContain('src="https://x/y.png"');
  });

  it("unwraps a span that carries nothing, which is most of what Word emits", () => {
    expect(cleanHtml("<p>Revenue is <span>up</span>.</p>", doc())).toBe("<p>Revenue is up.</p>");
  });

  it("unwraps a tag it does not know rather than dropping what is inside", () => {
    expect(cleanHtml("<section><p>kept</p></section>", doc())).toBe("<p>kept</p>");
    expect(cleanHtml('<font color="red">kept</font>', doc())).toBe("kept");
  });
});

describe("styles", () => {
  it("keeps the properties Outlook honours and drops the rest", () => {
    expect(cleanStyle("font-weight:700;display:flex;position:absolute")).toBe("font-weight: 700");
    expect(cleanStyle('font-family:"Calibri",sans-serif;font-size:14pt;color:#333')).toBe(
      "color: #333",
    );
    expect(cleanStyle("color:#333;text-align:center")).toBe("color: #333; text-align: center");
  });

  it("refuses anything a mail client from another decade cannot parse", () => {
    expect(isSafeStyleValue("var(--brand)")).toBe(false);
    expect(isSafeStyleValue("calc(100% - 2px)")).toBe(false);
    expect(isSafeStyleValue("oklch(0.7 0.1 200)")).toBe(false);
    expect(isSafeStyleValue("url(https://x/y.png)")).toBe(false);
    expect(isSafeStyleValue("red !important")).toBe(false);
    expect(isSafeStyleValue("#0F4761")).toBe(true);
  });
});

/* -------------------------------------------------------------------------- */
/* The plain-text twin                                                         */
/* -------------------------------------------------------------------------- */

/**
 * The table both implementations answer to.
 *
 * The same cases are in `src-tauri/tests/compose.rs`. Two implementations of one
 * derivation exist because the editor needs the answer without a round trip and
 * Rust needs it to put on the wire; if they drift, both suites fail.
 */
export const TEXT_CASES: ReadonlyArray<readonly [string, string]> = [
  ["<div>Hello.</div>", "Hello."],
  ["<div>one</div><div>two</div>", "one\ntwo"],
  ["<p>one</p><p>two</p>", "one\n\ntwo"],
  ["<div>one<br>two</div>", "one\ntwo"],
  ["<div><b>bold</b> and <i>italic</i></div>", "bold and italic"],
  ["<ul><li>one</li><li>two</li></ul>", "- one\n- two"],
  ["<ol><li>one</li><li>two</li></ol>", "1. one\n2. two"],
  ["<blockquote><div>quoted</div></blockquote>", "> quoted"],
  [
    '<div>see <a href="https://example.com/a">the page</a></div>',
    "see the page <https://example.com/a>",
  ],
  [
    '<div><a href="https://example.com">https://example.com</a></div>',
    "https://example.com",
  ],
  ["<div>a &amp; b &nbsp;c</div>", "a & b c"],
  ["<div><br></div>", ""],
];

describe("htmlToPlainText", () => {
  it("renders exactly these cases", () => {
    for (const [html, expected] of TEXT_CASES) {
      expect(htmlToPlainText(html)).toBe(expected);
    }
  });

  it("keeps a quote marker on every line of the quote", () => {
    const html = "<blockquote><p>one</p><p>two</p></blockquote>";
    expect(htmlToPlainText(html)).toBe("> one\n>\n> two");
  });

  it("does not reproduce emphasis as punctuation", () => {
    // A plain-text part with asterisks in it cannot be told apart from a
    // plain-text part where somebody typed asterisks.
    expect(htmlToPlainText("<div><b>no</b> stars</div>")).not.toContain("*");
  });
});

describe("isBlankHtml", () => {
  it("knows what an untouched editor contains", () => {
    expect(isBlankHtml("")).toBe(true);
    expect(isBlankHtml("<div><br></div>")).toBe(true);
    expect(isBlankHtml("<div>&nbsp;</div>")).toBe(true);
    expect(isBlankHtml("<div>x</div>")).toBe(false);
  });

  it("counts a picture as content, because it is one", () => {
    expect(isBlankHtml('<div><img src="https://x/y.png"></div>')).toBe(false);
  });
});

describe("signatures", () => {
  it("appends once, however many times it is asked", () => {
    const once = withHtmlSignature("<div>Hi</div>", "Bruno\nMach");
    expect(withHtmlSignature(once, "Bruno\nMach")).toBe(once);
    expect(htmlToPlainText(once)).toContain("-- \nBruno\nMach");
  });

  it("can be taken back off, so an untouched composer reads as empty", () => {
    const signed = withHtmlSignature("", "Bruno");
    expect(isBlankHtml(withoutSignature(signed))).toBe(true);
  });

  it("escapes text on its way into HTML", () => {
    expect(htmlFromPlainText("a < b & c")).toBe("<div>a &lt; b &amp; c</div>");
  });
});
