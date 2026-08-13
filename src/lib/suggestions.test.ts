import { describe, expect, it } from "vitest";
import {
  AS_WRITTEN_RETENTION,
  MAX_KEYED_STANCES,
  SUGGESTION_KEYS,
  caretAfter,
  loadSuggestion,
  loadSuggestionStats,
  recordOutcome,
  retention,
  sendOutcome,
  stanceHtml,
  EMPTY_STATS,
} from "./suggestions";
import { htmlToPlainText } from "./email-html";

const stance = { label: "Say you'll be there", body: "Tuesday works. Two o'clock, same place." };

describe("promoting a stance into a composer", () => {
  it("becomes the same kind of HTML a signature does", () => {
    const html = stanceHtml(stance);
    expect(html).toContain("<div>");
    expect(htmlToPlainText(html).trim()).toBe(stance.body);
  });

  it("escapes what the sender wrote", () => {
    const html = stanceHtml({ label: "Say yes", body: "a < b & c > d" });
    expect(html).not.toContain("<b ");
    expect(htmlToPlainText(html).trim()).toBe("a < b & c > d");
  });

  it("keeps blank lines between paragraphs", () => {
    // The composer draws a paragraph break as an empty block, which is what a
    // hand-typed one is too — so a stance and a typed reply hold the same
    // markup and the editor has nothing to reconcile. (`htmlToPlainText` folds
    // the blank away on the way back out; it is the editor's rendering, not the
    // round trip, that this is about.)
    const html = stanceHtml({ label: "Say yes", body: "First.\n\nSecond." });
    expect(html).toBe("<div>First.</div><div><br></div><div>Second.</div>");
  });

  it("puts the caret at the end of what was written", () => {
    // The offset is into the *text*, because that is what `caret-offset.ts`
    // counts — not into the markup, which has divs in it.
    expect(caretAfter(stance)).toBe(stance.body.length);
    expect(caretAfter({ label: "x", body: "  padded  " })).toBe("padded".length);
  });
});

describe("did he send it as written", () => {
  it("counts an untouched stance as written", () => {
    expect(retention(stance.body, stanceHtml(stance))).toBe(1);
    expect(sendOutcome(stance.body, stanceHtml(stance))).toBe("sentAsWritten");
  });

  it("forgives a signature the composer added", () => {
    const sent = stanceHtml(stance) + "<div><br></div><div>-- </div><div>Bruno</div>";
    expect(sendOutcome(stance.body, sent)).toBe("sentAsWritten");
  });

  it("forgives a sentence added on the end", () => {
    const sent = stanceHtml({
      label: stance.label,
      body: `${stance.body}\n\nI'll bring the numbers.`,
    });
    expect(sendOutcome(stance.body, sent)).toBe("sentAsWritten");
  });

  it("forgives a typo fix and a reordering", () => {
    const sent = stanceHtml({
      label: stance.label,
      body: "Two o'clock, same place — Tuesday works.",
    });
    expect(sendOutcome(stance.body, sent)).toBe("sentAsWritten");
  });

  it("calls a rewrite a rewrite", () => {
    const sent = stanceHtml({
      label: stance.label,
      body: "Sorry, I can't make it this week at all. How about the following Monday?",
    });
    expect(sendOutcome(stance.body, sent)).toBe("sentEdited");
  });

  it("does not credit the quoted message underneath", () => {
    // A reply carries what it answers. If the quote counted, every send would
    // look like a success as soon as the stance echoed a few of their words.
    const quoted =
      "<div>No.</div><div><br></div><div>&gt; Tuesday works. Two o'clock, same place.</div>";
    expect(sendOutcome(stance.body, quoted)).toBe("sentEdited");
  });

  it("an emptied composer retained nothing", () => {
    expect(retention(stance.body, "")).toBe(0);
    expect(sendOutcome(stance.body, "")).toBe("sentEdited");
  });

  it("an empty stance cannot be retained", () => {
    expect(retention("", "<div>anything</div>")).toBe(0);
  });

  it("draws the line where it says it does", () => {
    // Ten words; eight kept is exactly the threshold and counts as written.
    const body = "one two three four five six seven eight nine ten";
    const eight = "<div>one two three four five six seven eight</div>";
    const seven = "<div>one two three four five six seven</div>";
    expect(retention(body, eight)).toBeCloseTo(AS_WRITTEN_RETENTION);
    expect(sendOutcome(body, eight)).toBe("sentAsWritten");
    expect(sendOutcome(body, seven)).toBe("sentEdited");
  });
});

describe("outside Tauri", () => {
  /*
   * A browser tab against Vite has no sync loop, so it has nothing to have
   * suggested — and nothing here may throw at it. The dock renders the ordinary
   * reply strip and never knows the difference.
   */
  it("has no suggestions and never throws", async () => {
    await expect(loadSuggestion(1)).resolves.toBeNull();
    await expect(loadSuggestionStats()).resolves.toEqual(EMPTY_STATS);
    expect(() => recordOutcome("picked", { stanceIndex: 0 })).not.toThrow();
  });
});

describe("the keys", () => {
  it("number the stances from one", () => {
    expect(SUGGESTION_KEYS.stance(0)).toBe("1");
    expect(SUGGESTION_KEYS.stance(1)).toBe("2");
  });

  it("keep the empty composer on a key of its own", () => {
    expect(SUGGESTION_KEYS.mine).toBe("0");
    const stanceDigits = Array.from({ length: MAX_KEYED_STANCES }, (_, i) =>
      SUGGESTION_KEYS.stance(i),
    );
    expect(stanceDigits).not.toContain(SUGGESTION_KEYS.mine);
  });
});
