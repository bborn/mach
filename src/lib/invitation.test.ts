/**
 * The rules an invitation is answered under, without a DOM in the way.
 *
 * The component test beside this one mounts the card and presses the keys; this
 * pins the decisions the card makes before it draws anything — which messages
 * are invitations at all, which one a keystroke means, and what "already
 * answered" is called.
 */

import { describe, expect, it } from "vitest";
import type { Invitation, Message } from "@/types";
import {
  ANSWERS,
  activeInvitation,
  answerLabel,
  invitationOn,
  isAnswerable,
  REQUEST,
  whenLabel,
} from "./invitation";

function invitation(over: Partial<Invitation> = {}): Invitation {
  return {
    uid: "6r2h1c9k@google.com",
    method: REQUEST,
    eventId: 77,
    allDay: false,
    recurring: false,
    ...over,
  };
}

function message(over: Partial<Message> = {}): Message {
  return {
    id: 1,
    threadId: 1,
    accountId: 1,
    from: { name: "Alex", email: "alex@example.com" },
    to: [],
    cc: [],
    timestamp: 0,
    bodyText: "",
    snippet: "",
    attachments: [],
    isDraft: false,
    ...over,
  };
}

describe("which messages are invitations", () => {
  it("is none of them, for ordinary mail", () => {
    expect(invitationOn(message())).toBeNull();
  });

  it("is a REQUEST", () => {
    expect(invitationOn(message({ invitation: invitation() }))).not.toBeNull();
  });

  /*
   * Rust already refuses these, and this refuses them again. The field is data
   * crossing a boundary, and "the other side promised" is how a cancellation
   * ends up wearing an Accept button.
   */
  it("is not a reply or a cancellation", () => {
    for (const method of ["REPLY", "CANCEL", "COUNTER", "PUBLISH", ""]) {
      expect(invitationOn(message({ invitation: invitation({ method }) }))).toBeNull();
    }
  });
});

describe("whether there is anything to answer", () => {
  it("needs an event id, because that is what the command addresses", () => {
    expect(isAnswerable(invitation())).toBe(true);
    expect(isAnswerable(invitation({ eventId: undefined }))).toBe(false);
    expect(isAnswerable(null)).toBe(false);
  });
});

describe("which invitation a keystroke means", () => {
  it("is the one the message cursor is on", () => {
    expect(activeInvitation([4, 9, 12], 9)).toBe(9);
  });

  it("is the newest when the cursor is somewhere else", () => {
    expect(activeInvitation([4, 9, 12], null)).toBe(12);
    expect(activeInvitation([4, 9, 12], 7)).toBe(12);
  });

  it("is nothing when there are none", () => {
    expect(activeInvitation([], 4)).toBeNull();
  });
});

describe("the answer already on the calendar", () => {
  it("is named in words", () => {
    expect(answerLabel("accepted")).toBe("Yes");
    expect(answerLabel("tentative")).toBe("Maybe");
    expect(answerLabel("declined")).toBe("No");
  });

  it("is nothing when the invitation has not been answered", () => {
    expect(answerLabel("needsAction")).toBeNull();
    expect(answerLabel(undefined)).toBeNull();
  });
});

describe("when the meeting is", () => {
  it("reads as a day and a range", () => {
    const label = whenLabel(invitation({ start: Date.parse("2026-08-12T15:00:00"), end: Date.parse("2026-08-12T16:00:00") }));
    expect(label).toContain("Aug 12, 2026");
    expect(label).toContain("–");
  });

  it("drops the clock for an all-day event", () => {
    const label = whenLabel(
      invitation({ start: Date.parse("2026-08-12T00:00:00"), end: Date.parse("2026-08-13T00:00:00"), allDay: true }),
    );
    expect(label).toContain("Aug 12, 2026");
    expect(label).not.toContain("–");
  });

  it("says nothing at all when the event is not in the store", () => {
    expect(whenLabel(invitation({ eventId: undefined }))).toBe("");
  });
});

describe("the keys", () => {
  /*
   * The invariant `composer-keys.test.ts` enforces, stated here as well because
   * that test scans for `allowInInput` and these would pass it by being absent
   * rather than by being safe. These are chords on a letter no other binding
   * claims, and none of their tokens is a key a text editor owns.
   */
  it("are a chord on a key the composer does not need", () => {
    for (const answer of ANSWERS) {
      expect(answer.keys.split(" ")[0]).toBe("i");
      expect(answer.keys.split(" ")).toHaveLength(2);
    }
    expect(ANSWERS.map((a) => a.keys)).toEqual(["i y", "i m", "i n"]);
  });

  it("run from most to least committal, the way the calendar does", () => {
    expect(ANSWERS.map((a) => a.response)).toEqual(["accepted", "tentative", "declined"]);
    expect(ANSWERS.map((a) => a.label)).toEqual(["Yes", "Maybe", "No"]);
  });
});
