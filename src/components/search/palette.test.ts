import { describe, expect, it } from "vitest";
import type { Participant } from "@/types";
import { resolve, type PaletteContext } from "@/lib/palette/resolver";
import { correspondenceQuery, searchResolver } from "./palette";

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

/*
 * What a row *runs* is asserted in `person-search.test.ts`, which needs a
 * window to hear the handoff event. This file stays in the node environment
 * because one of the tests below is about working without one.
 */

describe("the ⌘K route into search", () => {
  it("declines the command mode and the empty box", () => {
    expect(searchResolver.claims(">archive")).toBe(false);
    expect(searchResolver.claims("   ")).toBe(false);
    expect(searchResolver.claims("from:tawny")).toBe(true);
  });

  it("offers one row that says what the query was understood to mean", () => {
    const [result, ...rest] = searchResolver.resolve(context("from:tawny is:unread"));
    expect(rest).toHaveLength(0);
    expect(result?.title).toBe("Search all mail for from tawny · unread");
    expect(result?.kind).toBe("command");
  });

  it("ranks an operator query above anything fuzzy, and plain words below", () => {
    const operators = searchResolver.resolve(context("from:tawny"))[0];
    const words = searchResolver.resolve(context("tawny"))[0];
    expect(operators?.score ?? 0).toBeGreaterThan(1000);
    // 1000 is `fuzzyScore`'s ceiling — a plain word must not outrank a real
    // prefix match on a command or a label.
    expect(words?.score ?? 0).toBeLessThan(1000);
  });

  it("offers nothing when there is nothing to run", () => {
    expect(searchResolver.resolve(context("is:"))).toEqual([]);
    expect(searchResolver.resolve(context("  "))).toEqual([]);
  });

  it("is registered in the chain by importing this module, not by editing it", () => {
    // The seam `lib/palette/resolver.ts` documents: no edit to that file, and
    // the palette component still knows nothing about search.
    const results = resolve(context("from:tawny"));
    expect(results.some((r) => r.id === "search:open")).toBe(true);
  });

  it("runs without a window, because the parser half is also a module", () => {
    const [result] = searchResolver.resolve(context("invoice"));
    expect(() => result?.run()).not.toThrow();
  });
});

const TAWNY: Participant = { name: "Tawny Marks", email: "tawny@example.com" };

describe("mail with one person", () => {
  it("offers the correspondence for an address, and searches to and from", () => {
    const [result, ...rest] = searchResolver.resolve(context("john@gmail.com"));
    expect(result?.title).toBe("Search all mail with john@gmail.com");
    expect(correspondenceQuery("john@gmail.com")).toBe(
      "from:john@gmail.com OR to:john@gmail.com OR cc:john@gmail.com",
    );
    // One row, not two: the full-text search for the same characters is not
    // offered beside it.
    expect(rest).toHaveLength(0);
  });

  it("offers it for an address nobody in the store has ever written to", () => {
    // `ctx.people` is built from mail that exists, so a stranger is absent from
    // it — and "have I ever heard from this person" is exactly the question.
    const results = searchResolver.resolve(context("stranger@nowhere.test", [TAWNY]));
    expect(results.map((r) => r.title)).toEqual([
      "Search all mail with stranger@nowhere.test",
    ]);
  });

  it("names the contact when it knows one, and keeps the address beside it", () => {
    const [result] = searchResolver.resolve(context("tawny@example.com", [TAWNY]));
    expect(result?.title).toBe("Search all mail with Tawny Marks");
    expect(result?.subtitle).toBe("tawny@example.com");
  });

  it("takes the address out of `Name <addr>`, which is what a paste looks like", () => {
    const [result] = searchResolver.resolve(context("Tawny Marks <tawny@example.com>"));
    expect(result?.title).toBe("Search all mail with tawny@example.com");
  });

  it("offers a name as well as the words, because both are worth having", () => {
    const results = searchResolver.resolve(context("tawny", [TAWNY]));
    expect(results.map((r) => r.title)).toEqual([
      "Search all mail with Tawny Marks",
      "Search all mail for tawny",
    ]);
    // The person is the better guess, so it is the row ⏎ lands on.
    expect(results[0]!.score).toBeGreaterThan(results[1]!.score!);
  });

  it("does not scatter-match a stranger across three letters", () => {
    // "twm" is a subsequence of "Tawny Marks" and nothing a person means.
    const titles = searchResolver.resolve(context("twm", [TAWNY])).map((r) => r.title);
    expect(titles).toEqual(["Search all mail for twm"]);
  });

  it("never reads an operator query as a bare address", () => {
    const [result] = searchResolver.resolve(context("from:john@gmail.com"));
    expect(result?.title).toBe("Search all mail for from john@gmail.com");
  });

  it("keys the row by the person, so frecency remembers who — not what was typed", () => {
    const byAddress = searchResolver.resolve(context("tawny@example.com", [TAWNY]))[0];
    const byName = searchResolver.resolve(context("tawny", [TAWNY]))[0];
    expect(byAddress?.id).toBe("search:person:tawny@example.com");
    expect(byName?.id).toBe(byAddress?.id);
  });

  it("is reachable through the chain, like every other row", () => {
    const results = resolve(context("john@gmail.com"));
    expect(results.some((r) => r.id === "search:person:john@gmail.com")).toBe(true);
  });
});
