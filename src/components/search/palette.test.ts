import { describe, expect, it } from "vitest";
import { resolve, type PaletteContext } from "@/lib/palette/resolver";
import { searchResolver } from "./palette";

function context(query: string): PaletteContext {
  return {
    query,
    threads: [],
    events: [],
    people: [],
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
