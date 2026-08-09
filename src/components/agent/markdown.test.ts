/**
 * The Markdown the drawer honours, and the far longer list of things it must
 * leave alone.
 *
 * The complaint that produced this file was one word: the agent said it had
 * written a `**draft**` and the drawer printed the asterisks. Most of what is
 * pinned here is the other direction — arithmetic, snake_case column names and
 * a half-streamed sentence all have to survive a parser that is looking for
 * exactly those characters.
 */

import { describe, expect, it } from "vitest";
import { parseInline, parseMarkdown } from "./markdown";

/** The text of one line, with the markers gone. */
function text(line: { segments: { text: string }[] }): string {
  return line.segments.map((segment) => segment.text).join("");
}

describe("parseInline", () => {
  it("reads bold, and keeps the word", () => {
    expect(parseInline("I wrote a **draft**")).toEqual([
      { kind: "text", text: "I wrote a " },
      { kind: "strong", text: "draft" },
    ]);
  });

  it("reads italic and inline code", () => {
    expect(parseInline("*maybe* try `agent_start`")).toEqual([
      { kind: "em", text: "maybe" },
      { kind: "text", text: " try " },
      { kind: "code", text: "agent_start" },
    ]);
  });

  it("does not read two italics where there is one bold", () => {
    expect(parseInline("**a**")).toEqual([{ kind: "strong", text: "a" }]);
    expect(parseInline("**one** and **two**")).toEqual([
      { kind: "strong", text: "one" },
      { kind: "text", text: " and " },
      { kind: "strong", text: "two" },
    ]);
  });

  it("leaves arithmetic alone", () => {
    // Emphasis may not open or close on a space, which is the whole guard.
    expect(parseInline("3 * 4 * 5")).toEqual([{ kind: "text", text: "3 * 4 * 5" }]);
  });

  it("leaves underscores alone, because identifiers have them", () => {
    expect(parseInline("history_id and thread_labels")).toEqual([
      { kind: "text", text: "history_id and thread_labels" },
    ]);
  });

  it("leaves an unclosed marker literal, which is what streaming looks like", () => {
    expect(parseInline("I wrote a **dra")).toEqual([{ kind: "text", text: "I wrote a **dra" }]);
    expect(parseInline("a `code")).toEqual([{ kind: "text", text: "a `code" }]);
  });

  it("does not emphasise nothing", () => {
    expect(parseInline("****")).toEqual([{ kind: "text", text: "****" }]);
    expect(parseInline("**")).toEqual([{ kind: "text", text: "**" }]);
    expect(parseInline("")).toEqual([]);
  });

  it("does not read markers inside code", () => {
    expect(parseInline("`a ** b`")).toEqual([{ kind: "code", text: "a ** b" }]);
  });
});

describe("parseMarkdown", () => {
  it("keeps blank lines, because the layout is the answer's own", () => {
    const lines = parseMarkdown("one\n\ntwo");
    expect(lines).toHaveLength(3);
    expect(lines[1]!.segments).toEqual([]);
  });

  it("takes the hashes off a heading and marks it as one", () => {
    const [line] = parseMarkdown("## What I did");
    expect(line!.kind).toBe("heading");
    expect(text(line!)).toBe("What I did");
  });

  it("turns a list marker into a bullet", () => {
    for (const source of ["- archived it", "* archived it", "+ archived it"]) {
      const [line] = parseMarkdown(source);
      expect(line!.kind).toBe("bullet");
      expect(text(line!)).toBe("• archived it");
    }
  });

  it("keeps a bullet's own indent", () => {
    const [line] = parseMarkdown("  - nested");
    expect(text(line!)).toBe("  • nested");
  });

  it("reads emphasis inside a bullet", () => {
    const [line] = parseMarkdown("- wrote a **draft**");
    expect(line!.segments).toContainEqual({ kind: "strong", text: "draft" });
  });

  it("does not mistake an emphasised line for a bullet", () => {
    const [line] = parseMarkdown("*careful*");
    expect(line!.kind).toBe("text");
    expect(line!.segments).toEqual([{ kind: "em", text: "careful" }]);
  });

  it("leaves a numbered list as the text it already reads as", () => {
    const [line] = parseMarkdown("1. first");
    expect(line!.kind).toBe("text");
    expect(text(line!)).toBe("1. first");
  });

  it("never loses a character it did not mark up", () => {
    const source = "Sent **it** to dana@example.com — see `outbox`, then 3 * 4.";
    expect(text(parseMarkdown(source)[0]!)).toBe(
      "Sent it to dana@example.com — see outbox, then 3 * 4.",
    );
  });
});
