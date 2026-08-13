/**
 * The one question the reading pane's button is an answer to.
 *
 * A conversation has many messages and at most one of them is worth writing to.
 * Getting that wrong is not a cosmetic failure — it means the app asks the
 * wrong sender to stop, or asks an address that stopped working two years ago,
 * and neither says anything on screen about having done so.
 */

import { describe, expect, it } from "vitest";
import type { Message } from "@/types";
import {
  REPORT_SPAM_LABEL,
  UNSUBSCRIBE_LABEL,
  unsubscribeAction,
} from "./unsubscribe-offer";

function message(over: Partial<Message> = {}): Message {
  return {
    id: 1,
    threadId: 7,
    accountId: 1,
    from: { name: "Whiny Nil", email: "books@whinynil.example" },
    to: [],
    cc: [],
    timestamp: Date.UTC(2026, 7, 10, 9, 0),
    bodyText: "",
    snippet: "",
    attachments: [],
    isDraft: false,
    ...over,
  };
}

describe("unsubscribeAction", () => {
  it("offers nothing for a conversation no message vouches for", () => {
    expect(unsubscribeAction([])).toBeNull();
    expect(unsubscribeAction([message(), message({ id: 2 })])).toBeNull();
  });

  it("names the message, the sender and the word on the button", () => {
    const action = unsubscribeAction([
      message({ id: 44, unsubscribe: { offer: "unsubscribe", method: "oneClick" } }),
    ]);

    expect(action).toEqual({
      messageId: 44,
      offer: { offer: "unsubscribe", method: "oneClick" },
      label: UNSUBSCRIBE_LABEL,
      sender: "Whiny Nil",
    });
  });

  /*
   * The button is honest about which of the two things it does. Rust reached
   * for `reportSpam` because nothing vouches for the sender, and a button
   * reading "Unsubscribe" over that would promise the gentler action.
   */
  it("says Report spam when that is the honest offer", () => {
    const action = unsubscribeAction([
      message({ id: 9, unsubscribe: { offer: "reportSpam", reason: "unknownSender" } }),
    ]);

    expect(action?.label).toBe(REPORT_SPAM_LABEL);
    expect(action?.offer).toEqual({ offer: "reportSpam", reason: "unknownSender" });
  });

  it("takes the newest offer, not the first one it finds", () => {
    const action = unsubscribeAction([
      message({
        id: 1,
        timestamp: Date.UTC(2024, 0, 1),
        unsubscribe: { offer: "unsubscribe", method: "mail" },
      }),
      message({ id: 2, timestamp: Date.UTC(2025, 0, 1) }),
      message({
        id: 3,
        timestamp: Date.UTC(2026, 0, 1),
        from: { name: "Whiny Nil Lists", email: "lists@whinynil.example" },
        unsubscribe: { offer: "unsubscribe", method: "oneClick" },
      }),
      message({ id: 4, timestamp: Date.UTC(2026, 5, 1) }),
    ]);

    expect(action?.messageId).toBe(3);
    expect(action?.sender).toBe("Whiny Nil Lists");
  });

  // Out of order is the case the loop has to survive: the newest message is
  // the newest by its timestamp, whatever position the list put it in.
  it("does not assume the list is sorted", () => {
    const action = unsubscribeAction([
      message({
        id: 10,
        timestamp: Date.UTC(2026, 6, 1),
        unsubscribe: { offer: "unsubscribe", method: "oneClick" },
      }),
      message({
        id: 11,
        timestamp: Date.UTC(2024, 6, 1),
        unsubscribe: { offer: "reportSpam", reason: "notBulkMail" },
      }),
    ]);

    expect(action?.messageId).toBe(10);
  });

  it("falls back to the address, and then to something sayable", () => {
    const noName = unsubscribeAction([
      message({
        from: { name: "", email: "noreply@whinynil.example" },
        unsubscribe: { offer: "unsubscribe", method: "link" },
      }),
    ]);
    expect(noName?.sender).toBe("noreply@whinynil.example");

    const nobody = unsubscribeAction([
      message({
        from: { name: "", email: "" },
        unsubscribe: { offer: "unsubscribe", method: "link" },
      }),
    ]);
    expect(nobody?.sender).toBe("the sender");
  });
});
