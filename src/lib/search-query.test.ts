import { describe, expect, it } from "vitest";
import {
  EMPTY_SEARCH,
  isTrivialSearch,
  matchesSearchNode,
  parseSearchQuery,
  type ParseOptions,
  type SearchNode,
} from "./search-query";

/** A fixed clock, so every relative-date assertion is a literal. */
const NOW = new Date(2026, 1, 15, 12, 0, 0, 0).getTime();
const DAY = 86_400_000;

function parse(input: string, options: ParseOptions = {}) {
  return parseSearchQuery(input, { now: NOW, prefixLastTerm: false, ...options });
}

function node(input: string, options: ParseOptions = {}): SearchNode | null {
  return parse(input, options).node;
}

describe("bare terms", () => {
  it("falls through to full text", () => {
    expect(node("invoice")).toEqual({ type: "text", value: "invoice", prefix: false });
  });

  it("ANDs several words together", () => {
    expect(node("invoice acme")).toEqual({
      type: "and",
      nodes: [
        { type: "text", value: "invoice", prefix: false },
        { type: "text", value: "acme", prefix: false },
      ],
    });
  });

  it("keeps a quoted phrase whole", () => {
    expect(node('"quarterly report"')).toEqual({
      type: "text",
      value: "quarterly report",
      prefix: false,
    });
  });

  it("treats lowercase 'or' as a word, as Gmail does", () => {
    const parsed = node("cats or dogs");
    expect(parsed).toEqual({
      type: "and",
      nodes: [
        { type: "text", value: "cats", prefix: false },
        { type: "text", value: "or", prefix: false },
        { type: "text", value: "dogs", prefix: false },
      ],
    });
  });

  it("prefixes the trailing word while it is being typed", () => {
    expect(node("veloci", { prefixLastTerm: true })).toEqual({
      type: "text",
      value: "veloci",
      prefix: true,
    });
    // A space says the word is finished.
    expect(node("veloci ", { prefixLastTerm: true })).toEqual({
      type: "text",
      value: "veloci",
      prefix: false,
    });
    // Only the *last* one.
    expect(node("acme veloci", { prefixLastTerm: true })).toEqual({
      type: "and",
      nodes: [
        { type: "text", value: "acme", prefix: false },
        { type: "text", value: "veloci", prefix: true },
      ],
    });
  });

  it("never prefixes a closed phrase", () => {
    expect(node('"quarterly report"', { prefixLastTerm: true })).toEqual({
      type: "text",
      value: "quarterly report",
      prefix: false,
    });
  });
});

describe("address operators", () => {
  it("reads from/to/cc/bcc", () => {
    for (const field of ["from", "to", "cc", "bcc"] as const) {
      expect(node(`${field}:bob@example.com`)).toEqual({
        type: "field",
        field,
        value: "bob@example.com",
      });
    }
  });

  it("unwraps angle brackets and lowercases", () => {
    expect(node("from:<Bob@Example.COM>")).toEqual({
      type: "field",
      field: "from",
      value: "bob@example.com",
    });
  });

  it("takes a quoted value with spaces in it", () => {
    expect(node('from:"Bob Loblaw"')).toEqual({
      type: "field",
      field: "from",
      value: "bob loblaw",
    });
  });

  it("drops an operator with no value yet", () => {
    expect(node("from:")).toBeNull();
    expect(node("from: invoice")).toEqual({ type: "text", value: "invoice", prefix: false });
  });
});

describe("subject", () => {
  it("is its own field so it can use the FTS subject column", () => {
    expect(node("subject:invoice")).toEqual({
      type: "field",
      field: "subject",
      value: "invoice",
      prefix: false,
    });
  });

  it("prefixes while typing", () => {
    expect(node("subject:invo", { prefixLastTerm: true })).toEqual({
      type: "field",
      field: "subject",
      value: "invo",
      prefix: true,
    });
  });
});

describe("flags and mailboxes", () => {
  it("reads is: and has:", () => {
    expect(node("is:unread")).toEqual({ type: "flag", flag: "unread" });
    expect(node("is:read")).toEqual({ type: "flag", flag: "read" });
    expect(node("is:starred")).toEqual({ type: "flag", flag: "starred" });
    expect(node("has:attachment")).toEqual({ type: "flag", flag: "attachment" });
  });

  it("maps in: onto Gmail's system label ids", () => {
    expect(node("in:inbox")).toEqual({ type: "field", field: "label", value: "INBOX" });
    expect(node("in:trash")).toEqual({ type: "field", field: "label", value: "TRASH" });
    expect(node("in:spam")).toEqual({ type: "field", field: "label", value: "SPAM" });
  });

  it("treats an unknown in: as a user label", () => {
    expect(node("in:receipts")).toEqual({ type: "field", field: "label", value: "receipts" });
  });

  it("makes in:anywhere the identity rather than dropping it", () => {
    expect(node("in:anywhere")).toEqual({ type: "all" });
    expect(isTrivialSearch(parse("in:anywhere"))).toBe(true);
    // Dropping it inside an OR would have narrowed the query instead of widening it.
    expect(node("acme OR in:anywhere")).toEqual({
      type: "or",
      nodes: [{ type: "text", value: "acme", prefix: false }, { type: "all" }],
    });
  });

  it("reads label: by name", () => {
    expect(node("label:receipts")).toEqual({ type: "field", field: "label", value: "receipts" });
  });

  it("reads filename:", () => {
    expect(node("filename:pdf")).toEqual({ type: "field", field: "filename", value: "pdf" });
  });

  it("records an unrecognised is:/has: as unknown rather than filtering on it", () => {
    const parsed = parse("is:chartreuse");
    expect(parsed.node).toBeNull();
    expect(parsed.unknown).toEqual(["is:chartreuse"]);
  });
});

describe("dates", () => {
  it("reads before: and after: as local midnight", () => {
    const feb = new Date(2026, 1, 1).getTime();
    expect(node("after:2026/02/01")).toEqual({ type: "date", bound: "after", ts: feb });
    expect(node("before:2026-02-01")).toEqual({ type: "date", bound: "before", ts: feb });
  });

  it("reads the relative forms", () => {
    expect(node("newer_than:7d")).toEqual({ type: "date", bound: "after", ts: NOW - 7 * DAY });
    expect(node("older_than:2m")).toEqual({ type: "date", bound: "before", ts: NOW - 60 * DAY });
    expect(node("older_than:1y")).toEqual({ type: "date", bound: "before", ts: NOW - 365 * DAY });
  });

  it("also accepts a relative value on before:/after:", () => {
    expect(node("after:7d")).toEqual({ type: "date", bound: "after", ts: NOW - 7 * DAY });
  });

  it("does not invent a date out of nonsense", () => {
    const parsed = parse("before:soon");
    expect(parsed.node).toBeNull();
    expect(parsed.unknown).toEqual(["before:soon"]);
    expect(node("after:2026/13/40")).toBeNull();
  });
});

describe("boolean structure", () => {
  it("reads OR", () => {
    expect(node("acme OR globex")).toEqual({
      type: "or",
      nodes: [
        { type: "text", value: "acme", prefix: false },
        { type: "text", value: "globex", prefix: false },
      ],
    });
  });

  it("binds AND tighter than OR", () => {
    expect(node("a b OR c")).toEqual({
      type: "or",
      nodes: [
        {
          type: "and",
          nodes: [
            { type: "text", value: "a", prefix: false },
            { type: "text", value: "b", prefix: false },
          ],
        },
        { type: "text", value: "c", prefix: false },
      ],
    });
  });

  it("groups with parentheses", () => {
    expect(node("(a OR b) c")).toEqual({
      type: "and",
      nodes: [
        {
          type: "or",
          nodes: [
            { type: "text", value: "a", prefix: false },
            { type: "text", value: "b", prefix: false },
          ],
        },
        { type: "text", value: "c", prefix: false },
      ],
    });
  });

  it("negates with - and with NOT", () => {
    const negated = { type: "not", node: { type: "text", value: "spam", prefix: false } };
    expect(node("-spam")).toEqual(negated);
    expect(node("NOT spam")).toEqual(negated);
  });

  it("negates an operator and a group", () => {
    expect(node("-from:bob@example.com")).toEqual({
      type: "not",
      node: { type: "field", field: "from", value: "bob@example.com" },
    });
    expect(node("-(a OR b)")).toEqual({
      type: "not",
      node: {
        type: "or",
        nodes: [
          { type: "text", value: "a", prefix: false },
          { type: "text", value: "b", prefix: false },
        ],
      },
    });
  });

  it("understands a realistic query end to end", () => {
    expect(node('from:stripe has:attachment -is:read subject:"invoice" newer_than:30d')).toEqual({
      type: "and",
      nodes: [
        { type: "field", field: "from", value: "stripe" },
        { type: "flag", flag: "attachment" },
        { type: "not", node: { type: "flag", flag: "read" } },
        { type: "field", field: "subject", value: "invoice", prefix: false },
        { type: "date", bound: "after", ts: NOW - 30 * DAY },
      ],
    });
  });
});

describe("totality — half-typed input must never throw", () => {
  const nasty = [
    "",
    " ",
    "-",
    "- ",
    "--",
    '"',
    '"unfinished',
    'from:"unfinished',
    "(",
    "(((",
    ")",
    ")))",
    "()",
    "( )",
    "OR",
    "OR OR",
    "a OR",
    "OR a",
    "AND",
    "NOT",
    "NOT NOT NOT",
    "from:",
    "from::",
    ":",
    "::::",
    "is:",
    "has:",
    "before:",
    "newer_than:",
    "newer_than:x",
    "label:",
    "subject:",
    "a -(b OR (c AND",
    "🙂",
    "from:🙂",
    '"a" "b',
    "\\",
    "%_%",
    "*",
    "**",
    "NEAR",
    "a NEAR b",
    "foo:bar",
    "-:-",
    "\t\n",
    "a".repeat(5000),
    // Recursive descent meets a keyboard: without a depth cap these are a
    // stack overflow, which in a React render is a white window.
    "(".repeat(5000),
    "(".repeat(5000) + "acme",
    "-".repeat(5000) + "acme",
    "a OR ".repeat(2000),
  ];

  for (const input of nasty) {
    it(`survives ${JSON.stringify(input)}`, () => {
      expect(() => parseSearchQuery(input, { now: NOW })).not.toThrow();
      const parsed = parseSearchQuery(input, { now: NOW });
      expect(parsed.input).toBe(input);
      expect(Array.isArray(parsed.chips)).toBe(true);
    });
  }

  it("keeps typing an operator usable at every keystroke", () => {
    const target = "from:bob@example.com is:unread";
    for (let i = 0; i <= target.length; i += 1) {
      const parsed = parseSearchQuery(target.slice(0, i), { now: NOW });
      expect(parsed.input).toBe(target.slice(0, i));
    }
  });

  it("reads an unterminated quote as the phrase so far", () => {
    expect(node('"quarterly rep')).toEqual({
      type: "text",
      value: "quarterly rep",
      prefix: false,
    });
    // …and as a prefix while typing, which is what keeps rows on screen.
    expect(node('"quarterly rep', { prefixLastTerm: true })).toEqual({
      type: "text",
      value: "quarterly rep",
      prefix: true,
    });
  });

  it("closes unbalanced parentheses rather than losing the query", () => {
    expect(node("(acme OR globex")).toEqual({
      type: "or",
      nodes: [
        { type: "text", value: "acme", prefix: false },
        { type: "text", value: "globex", prefix: false },
      ],
    });
    expect(node("acme)")).toEqual({ type: "text", value: "acme", prefix: false });
  });

  it("passes FTS5 operators through as ordinary words", () => {
    // These mean something to FTS5 and nothing to the user. The parser keeps
    // them as text; escaping them is `fts_escape`'s job on the Rust side.
    expect(node("NEAR")).toEqual({ type: "text", value: "NEAR", prefix: false });
    expect(node('a* "b" OR')).toEqual({
      type: "and",
      nodes: [
        { type: "text", value: "a*", prefix: false },
        { type: "text", value: "b", prefix: false },
      ],
    });
  });

  it("has an empty value that means nothing to run", () => {
    expect(EMPTY_SEARCH.node).toBeNull();
    expect(isTrivialSearch(EMPTY_SEARCH)).toBe(true);
    expect(parse("").node).toBeNull();
    expect(isTrivialSearch(parse("  "))).toBe(true);
    expect(isTrivialSearch(parse("acme"))).toBe(false);
  });
});

describe("the interpretation the query bar shows", () => {
  it("says the operators back in words", () => {
    expect(parse("from:stripe is:unread").chips).toEqual(["from stripe", "unread"]);
  });

  it("names a date instead of echoing epoch millis", () => {
    expect(parse("after:2026/02/01").chips).toEqual(["since 1 Feb 2026"]);
  });

  it("folds a negated group into one phrase", () => {
    expect(parse("-(urgent OR asap)").chips).toEqual(["not urgent asap"]);
  });

  it("quotes a phrase so it is visibly a phrase", () => {
    expect(parse('"quarterly report"').chips).toEqual(["“quarterly report”"]);
  });
});

describe("the fixture evaluator (browser dev only)", () => {
  const subject = {
    thread: {
      subject: "Invoice for February",
      snippet: "the velocipede has shipped",
      participants: [{ name: "Billing", email: "billing@stripe.com" }],
      timestamp: NOW,
      unread: true,
      starred: false,
      hasAttachment: true,
      labelIds: ["INBOX", "Label_7"],
    },
    messages: [
      {
        from: { name: "Billing", email: "billing@stripe.com" },
        to: [{ name: "Alex", email: "alex@example.com" }],
        cc: [],
        subject: "Invoice for February",
        bodyText: "the velocipede has shipped",
        attachments: [{ filename: "invoice-feb.pdf" }],
      },
    ],
  };

  function matches(query: string): boolean {
    const parsed = parse(query);
    return parsed.node ? matchesSearchNode(parsed.node, subject) : false;
  }

  it("agrees with the SQL compiler on every operator", () => {
    expect(matches("velocipede")).toBe(true);
    expect(matches("dirigible")).toBe(false);
    expect(matches("from:stripe")).toBe(true);
    expect(matches("to:alex@example.com")).toBe(true);
    expect(matches("cc:alex@example.com")).toBe(false);
    expect(matches("subject:invoice")).toBe(true);
    expect(matches("subject:velocipede")).toBe(false);
    expect(matches("is:unread")).toBe(true);
    expect(matches("is:read")).toBe(false);
    expect(matches("has:attachment")).toBe(true);
    expect(matches("is:starred")).toBe(false);
    expect(matches("filename:pdf")).toBe(true);
    expect(matches("in:inbox")).toBe(true);
    expect(matches("in:trash")).toBe(false);
    expect(matches("in:anywhere")).toBe(true);
  });

  it("composes the same way", () => {
    expect(matches("velocipede from:stripe")).toBe(true);
    expect(matches("velocipede from:globex")).toBe(false);
    expect(matches("dirigible OR velocipede")).toBe(true);
    expect(matches("-from:stripe")).toBe(false);
    expect(matches("invoice -is:read")).toBe(true);
  });

  it("matches whole words, not substrings, unless asked for a prefix", () => {
    expect(matches("veloci")).toBe(false);
    const parsed = parseSearchQuery("veloci", { now: NOW, prefixLastTerm: true });
    expect(parsed.node && matchesSearchNode(parsed.node, subject)).toBe(true);
  });

  it("bounds by date the same way", () => {
    expect(matches("newer_than:7d")).toBe(true);
    expect(matches("older_than:7d")).toBe(false);
  });
});
