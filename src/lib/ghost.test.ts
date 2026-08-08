import { describe, expect, it } from "vitest";
import { buildPrompt, cleanCompletion, shouldRequest } from "./ghost";

describe("shouldRequest", () => {
  it("never asks when the caret is not at the end", () => {
    expect(shouldRequest("emailBody", "Thanks for sending that over", false)).toBe(false);
  });

  it("waits until there is something to continue", () => {
    expect(shouldRequest("emailBody", "Thanks", true)).toBe(false);
    expect(shouldRequest("emailBody", "Thanks for the", true)).toBe(true);
  });

  it("lets a subject start sooner than a body", () => {
    expect(shouldRequest("emailSubject", "Q3 num", true)).toBe(true);
    expect(shouldRequest("emailBody", "Q3 num", true)).toBe(false);
  });

  it("stays quiet at a fresh paragraph, which is where a guess is worst", () => {
    expect(shouldRequest("emailBody", "Thanks for the update.\n\n", true)).toBe(false);
    expect(shouldRequest("emailBody", "Thanks for the update.\nOne more thing", true)).toBe(true);
  });
});

describe("buildPrompt", () => {
  it("carries the context lines and the text so far", () => {
    const { prompt, system, maxTokens } = buildPrompt({
      kind: "emailBody",
      prefix: "Happy to help with",
      context: ["Subject: Rent roll", "  ", "Writing to: Ada"],
    });
    expect(prompt).toContain("Subject: Rent roll");
    expect(prompt).toContain("Writing to: Ada");
    expect(prompt).toContain("Happy to help with");
    expect(system).toContain("never repeat");
    expect(maxTokens).toBeGreaterThan(0);
  });

  it("sends only the tail of a very long draft", () => {
    const prefix = `${"x".repeat(5000)}the end`;
    const { prompt } = buildPrompt({ kind: "emailBody", prefix });
    expect(prompt).toContain("the end");
    expect(prompt.length).toBeLessThan(2500);
  });
});

describe("cleanCompletion", () => {
  it("drops a prefix the model repeated back", () => {
    expect(cleanCompletion("emailBody", "Thanks for the", "Thanks for the update — I will")).toBe(
      " update — I will",
    );
  });

  it("drops a repeat that differs only in case", () => {
    expect(cleanCompletion("emailBody", "thanks for the", "Thanks for the update")).toBe(" update");
  });

  it("unwraps quotes and code fences", () => {
    expect(cleanCompletion("emailSubject", "Q3 ", '"numbers"')).toBe("numbers");
    expect(cleanCompletion("emailSubject", "Q3 ", "```\nnumbers\n```")).toBe("numbers");
  });

  it("refuses an answer that is a reply rather than a continuation", () => {
    expect(cleanCompletion("emailBody", "Thanks for the", "I cannot help with that.")).toBe("");
    expect(cleanCompletion("emailBody", "Thanks for the", "Sure! Here you go")).toBe("");
  });

  it("keeps a single-line field on one line", () => {
    expect(cleanCompletion("emailSubject", "Q3 ", "numbers\nand a second thought")).toBe("numbers");
  });

  it("does not double a space the writer already typed", () => {
    expect(cleanCompletion("emailBody", "Thanks for the ", "   update")).toBe("update");
  });

  it("truncates at a word boundary rather than mid-word", () => {
    const long = `${"word ".repeat(200)}`;
    const cleaned = cleanCompletion("emailSubject", "Subject: ", long);
    expect(cleaned.length).toBeLessThanOrEqual(60);
    expect(cleaned.endsWith("word")).toBe(true);
  });

  it("treats whitespace and emptiness as no suggestion", () => {
    expect(cleanCompletion("emailBody", "Thanks", "")).toBe("");
    expect(cleanCompletion("emailBody", "Thanks", "   \n  ")).toBe("");
  });
});
