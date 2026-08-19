import { describe, expect, it } from "vitest";
import { initialsOf, monogramColor } from "./monogram";

describe("initialsOf", () => {
  it("takes the ends of a two-part name", () => {
    expect(initialsOf("Ivy Chen", "ivy@x.com")).toBe("IC");
    expect(initialsOf("Meridian Air", "hello@meridian.example")).toBe("MA");
  });

  it("skips the middle of a longer name", () => {
    expect(initialsOf("Ada Beatrice King Lovelace", "ada@x.com")).toBe("AL");
  });

  it("gives one letter to a one-word name", () => {
    expect(initialsOf("Ledger", "receipts@ledger.example")).toBe("L");
  });

  it("falls back to the address when there is no name", () => {
    expect(initialsOf(undefined, "no-reply@stripe.example")).toBe("N");
    expect(initialsOf("", "  bruno@example.com")).toBe("B");
  });

  it("has something to draw even for an address it cannot read", () => {
    expect(initialsOf(undefined, undefined)).toBe("?");
    expect(initialsOf(undefined, "@@@")).toBe("?");
  });

  it("drops the punctuation a sender wraps its name in", () => {
    expect(initialsOf('"Tawny Reeves"', "t@x.com")).toBe("TR");
    expect(initialsOf("Reeves, Tawny", "t@x.com")).toBe("RT");
    expect(initialsOf("(Northloop) Ops", "ops@x.com")).toBe("NO");
  });

  it("keeps a whole code point, not half a surrogate pair", () => {
    expect(initialsOf("東京 チーム", "t@x.com")).toBe("東チ");
  });

  // A tile 26px across has room for the name or for the decoration, and the
  // name is what anybody is scanning for.
  it("steps over an emoji a sender put in front of its name", () => {
    expect(initialsOf("🎉 Party Planning", "p@x.com")).toBe("PP");
    expect(initialsOf("🎉", "p@x.com")).toBe("P");
  });
});

describe("monogramColor", () => {
  it("is stable for an address across calls", () => {
    expect(monogramColor("ivy@x.com")).toBe(monogramColor("ivy@x.com"));
  });

  it("ignores case and surrounding space, because Gmail does not", () => {
    expect(monogramColor("  Ivy@X.com ")).toBe(monogramColor("ivy@x.com"));
  });

  it("still has a colour for a sender with no address", () => {
    expect(monogramColor(undefined)).toMatch(/^#/);
  });
});
