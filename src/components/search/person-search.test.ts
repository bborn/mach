// @vitest-environment jsdom
/**
 * The whole route, from a name in ⌘K to the tree the store is asked for.
 *
 * `palette.test.ts` covers which rows are offered; this covers what picking one
 * *does*. It stops at the AST rather than at rows, because the AST is what
 * crosses the IPC seam — `db::queries::compile_search` turns exactly this into
 * SQL — so an assertion here is an assertion about the results, not about a
 * string that happens to look right.
 */

import { describe, expect, it, vi } from "vitest";
import type { Participant } from "@/types";
import { parseSearchQuery, type SearchNode } from "@/lib/search-query";
import type { PaletteContext } from "@/lib/palette/resolver";
import { SEARCH_EVENT, searchResolver, type SearchEventDetail } from "./palette";

const TAWNY: Participant = { name: "Tawny Marks", email: "tawny@example.com" };

function context(query: string, people: Participant[] = []): PaletteContext {
  return {
    query,
    threads: [],
    events: [],
    people,
    mailboxes: [],
    commands: [],
    actions: {
      openThread: () => {},
      openEvent: () => {},
      openMailbox: () => {},
      runCommand: () => {},
      composeTo: () => {},
    },
  };
}

/** Picks the first row and reports the query it handed to the search view. */
function pick(query: string, people: Participant[] = []): string {
  const [result] = searchResolver.resolve(context(query, people));
  expect(result).toBeDefined();

  const seen = vi.fn();
  const listener = (event: Event) =>
    seen((event as CustomEvent<SearchEventDetail>).detail.query);
  window.addEventListener(SEARCH_EVENT, listener);
  try {
    result!.run();
  } finally {
    window.removeEventListener(SEARCH_EVENT, listener);
  }
  expect(seen).toHaveBeenCalledOnce();
  return seen.mock.calls[0]![0] as string;
}

/** The `field` leaves of a parsed query, as `["from tawny@…", …]`. */
function fields(node: SearchNode | null): string[] {
  if (!node) return [];
  switch (node.type) {
    case "and":
    case "or":
      return node.nodes.flatMap(fields);
    case "not":
      return fields(node.node);
    case "field":
      return [`${node.field} ${node.value}`];
    default:
      return [];
  }
}

describe("picking the person row", () => {
  it("lands on the search view with a query the box can show and edit", () => {
    expect(pick("john@gmail.com")).toBe(
      "from:john@gmail.com OR to:john@gmail.com OR cc:john@gmail.com",
    );
  });

  it("parses to an OR over the three address operators", () => {
    const parsed = parseSearchQuery(pick("john@gmail.com"), { prefixLastTerm: false });
    expect(parsed.node?.type).toBe("or");
    expect(fields(parsed.node)).toEqual([
      "from john@gmail.com",
      "to john@gmail.com",
      "cc john@gmail.com",
    ]);
    // What the query bar draws, so the interpretation is visible without
    // reading the operators back.
    expect(parsed.chips).toEqual([
      "from john@gmail.com",
      "to john@gmail.com",
      "cc john@gmail.com",
    ]);
    expect(parsed.unknown).toEqual([]);
  });

  it("searches the contact's address, not the name that was typed", () => {
    expect(pick("tawny", [TAWNY])).toBe(
      "from:tawny@example.com OR to:tawny@example.com OR cc:tawny@example.com",
    );
  });

  it("still hands over a whole query for a stranger", () => {
    // No contact, no thread, nothing in the store — the search is still the
    // question, and it is still answerable.
    const parsed = parseSearchQuery(pick("stranger@nowhere.test"), {
      prefixLastTerm: false,
    });
    expect(fields(parsed.node)).toHaveLength(3);
  });

  it("leaves the plain-words row doing what it always did", () => {
    expect(pick("invoice")).toBe("invoice");
  });
});
