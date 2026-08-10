import { describe, expect, it } from "vitest";
import { actionOf, criteriaOf, isUsable } from "./Filters";

/**
 * The form's two translations, which are the whole reason the form exists.
 *
 * A person types "from" and ticks "Skip the inbox". Gmail wants
 * `{criteria:{from}, action:{removeLabelIds:["INBOX"]}}`, and nobody should
 * have to know that.
 */

function draft(overrides: Partial<Parameters<typeof criteriaOf>[0]> = {}) {
  return {
    accountId: 1,
    from: "",
    subject: "",
    query: "",
    skipInbox: false,
    markRead: false,
    trash: false,
    labelId: "none",
    ...overrides,
  };
}

describe("what the form sends", () => {
  it("omits the fields nobody filled in rather than sending empty strings", () => {
    expect(criteriaOf(draft({ from: "  no-reply@okta.com  " }))).toEqual({
      from: "no-reply@okta.com",
    });
  });

  it("says skip the inbox the way Gmail says it", () => {
    expect(actionOf(draft({ skipInbox: true }))).toEqual({
      addLabelIds: [],
      removeLabelIds: ["INBOX"],
    });
  });

  it("says delete it the way Gmail says it", () => {
    expect(actionOf(draft({ trash: true }))).toEqual({
      addLabelIds: ["TRASH"],
      removeLabelIds: [],
    });
  });

  it("combines the three effects and a label", () => {
    expect(
      actionOf(draft({ skipInbox: true, markRead: true, labelId: "Label_18" })),
    ).toEqual({
      addLabelIds: ["Label_18"],
      removeLabelIds: ["INBOX", "UNREAD"],
    });
  });
});

describe("what the form refuses to send", () => {
  // A filter with no criteria matches every message that ever arrives. Rust
  // refuses it too; this is what stops the button being pressable at all.
  it("will not create a rule that matches everything", () => {
    expect(isUsable(draft({ skipInbox: true }))).toBe(false);
  });

  it("will not create a rule that does nothing", () => {
    expect(isUsable(draft({ from: "no-reply@okta.com" }))).toBe(false);
  });

  it("is usable once it has both halves", () => {
    expect(isUsable(draft({ from: "no-reply@okta.com", skipInbox: true }))).toBe(true);
  });

  it("counts a whitespace-only field as empty", () => {
    expect(isUsable(draft({ from: "   ", skipInbox: true }))).toBe(false);
  });
});
