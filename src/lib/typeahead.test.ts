import { describe, expect, it } from "vitest";
import { activeToken, committedAddresses, replaceToken, tokenQuery } from "./typeahead";

describe("activeToken", () => {
  it("is the whole field when there is one address", () => {
    expect(activeToken("ada", 3)).toEqual({ start: 0, end: 3, text: "ada" });
  });

  it("is the chunk after the last separator before the caret", () => {
    const value = "ada@x.com, bo";
    expect(activeToken(value, value.length)).toEqual({ start: 10, end: 13, text: " bo" });
  });

  it("stops at the next separator when the caret is in the middle", () => {
    const value = "ada@x.com, bob@y.com, cy@z.com";
    expect(activeToken(value, 14).text).toBe(" bob@y.com");
  });

  it("does not split inside a quoted name", () => {
    const value = '"Doe, Jane" <j@x.com>';
    expect(activeToken(value, value.length)).toEqual({ start: 0, end: value.length, text: value });
  });

  it("treats a newline as a separator, because people paste lists", () => {
    expect(activeToken("a@x.com\nbo", 10).text).toBe("bo");
  });

  it("clamps a caret outside the string", () => {
    expect(activeToken("abc", 99).text).toBe("abc");
    expect(activeToken("abc", -5).text).toBe("abc");
  });
});

describe("tokenQuery", () => {
  it("is the chunk without its padding", () => {
    expect(tokenQuery("ada@x.com,  bo ", 15)).toBe("bo");
  });
});

describe("replaceToken", () => {
  it("puts the chosen address in place and opens the next one", () => {
    const value = "bo";
    expect(replaceToken(value, 2, "Bob <bob@y.com>")).toEqual({
      value: "Bob <bob@y.com>, ",
      caret: 17,
    });
  });

  it("keeps the addresses already in the field, and their spacing", () => {
    const value = "ada@x.com, bo";
    const next = replaceToken(value, value.length, "bob@y.com");
    expect(next.value).toBe("ada@x.com, bob@y.com, ");
    expect(next.caret).toBe(next.value.length);
  });

  it("leaves the rest of the line alone when completing in the middle", () => {
    const value = "ada@x.com, bo, cy@z.com";
    const next = replaceToken(value, 13, "bob@y.com");
    expect(next.value).toBe("ada@x.com, bob@y.com, cy@z.com");
    // Right after what was inserted, not at the end of the line.
    expect(next.value.slice(0, next.caret)).toBe("ada@x.com, bob@y.com");
  });

  it("completes an empty field", () => {
    expect(replaceToken("", 0, "a@x.com").value).toBe("a@x.com, ");
  });
});

describe("committedAddresses", () => {
  it("lists what is already there, lowercased, without the chunk being typed", () => {
    const value = "Ada <Ada@X.com>, bob@y.com, cy";
    expect(committedAddresses(value, value.length)).toEqual(["ada@x.com", "bob@y.com"]);
  });

  it("excludes the chunk under the caret wherever it is", () => {
    const value = "a@x.com, b@y.com, c@z.com";
    expect(committedAddresses(value, 10)).toEqual(["a@x.com", "c@z.com"]);
  });

  it("is empty for an empty field", () => {
    expect(committedAddresses("", 0)).toEqual([]);
  });
});
