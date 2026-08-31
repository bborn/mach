/**
 * What the composer strip says, and that it cannot disagree with the keyboard.
 *
 * Two things are being held here. The first is the labels: the owner could not
 * tell `⌥1 Getting into your Influ…` from `⌥2 Fwd: Your Deposit Rec…`, and the
 * cases below are the ones that produced those — a subject that is mostly
 * prefix, a subject that is mostly shared noun, a draft with nobody in it yet.
 *
 * The second is the property the strip is built on: the tab drawn as ⌥2 and the
 * composer ⌥2 switches to are the same draft *by construction*. `composerTabs`
 * is the one place a composer's chord is decided, and the last test in this
 * file is what keeps it the one place — a source scan, in the manner of
 * `lib/composer-keys.test.ts`, because the failure it guards against is
 * somebody adding a second expression that agrees today.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import type { Draft, DraftKind } from "@/lib/compose";
import { composerTabs, KEYED_COMPOSERS } from "./composer-tabs";

function draft(over: Partial<Draft> = {}): Draft {
  return {
    id: "d1",
    accountId: 1,
    kind: "new" as DraftKind,
    to: [],
    cc: [],
    bcc: [],
    subject: "",
    body: "",
    bodyFormat: "html",
    updatedAt: 0,
    ...over,
  };
}

/** One draft's tab, which is all most of these cases are about. */
function tab(over: Partial<Draft> = {}) {
  return composerTabs([draft(over)])[0]!;
}

describe("what a composer's tab says", () => {
  it("leads with the recipient's name and follows with the subject", () => {
    const one = tab({
      to: [{ name: "Tawny Rusk", email: "tawny@northloop.example" }],
      subject: "Series A data room",
    });
    expect(one.lead).toBe("Tawny Rusk");
    expect(one.trail).toBe("Series A data room");
    expect(one.label).toBe("Tawny Rusk — Series A data room");
  });

  /*
   * The reported label, and the whole point of the change: `Fwd: Your Deposit
   * Rec…` spent its twenty characters on a word that says what the draft is —
   * which the strip already shows — and a receipt anybody might be sending.
   */
  it("takes the reply and forward prefixes off the subject", () => {
    expect(tab({ subject: "Fwd: Your Deposit Receipt" }).lead).toBe("Your Deposit Receipt");
    expect(tab({ subject: "Re: Series A data room" }).lead).toBe("Series A data room");
    expect(tab({ subject: "Re: Fwd: Re[2]: Invoice" }).lead).toBe("Invoice");
  });

  it("falls back to the address where the sender has no name for them", () => {
    expect(tab({ to: [{ email: "sarah@northwind.example" }] }).lead).toBe(
      "sarah@northwind.example",
    );
  });

  // The first name and a count. Three names in a tab leaves room for nothing
  // else, and the composer below it has the full list.
  it("counts the rest rather than listing them", () => {
    expect(
      tab({
        to: [
          { name: "Tawny Rusk", email: "tawny@northloop.example" },
          { email: "deb@feldmanlegal.example" },
          { email: "cass@paperbark.example" },
        ],
      }).lead,
    ).toBe("Tawny Rusk +2");
  });

  it("names a draft in copy after the person in copy", () => {
    expect(tab({ cc: [{ name: "Deb Feldman", email: "deb@feldmanlegal.example" }] }).lead).toBe(
      "Deb Feldman",
    );
  });

  it("leads with the subject when there is no recipient yet", () => {
    const one = tab({ subject: "Fwd: Your Deposit Receipt" });
    expect(one.lead).toBe("Your Deposit Receipt");
    expect(one.trail).toBeNull();
  });

  it("names an empty composer for what it is", () => {
    expect(tab().lead).toBe("New message");
    expect(tab({ kind: "reply" }).lead).toBe("Reply");
    expect(tab({ kind: "replyAll" }).lead).toBe("Reply");
    expect(tab({ kind: "adopted" }).lead).toBe("Reply");
    expect(tab({ kind: "forward" }).lead).toBe("Forward");
  });

  // A subject of spaces is not a subject, and a name of spaces is not a name.
  it("treats whitespace as nothing at either position", () => {
    expect(tab({ subject: "   ", to: [{ name: "  ", email: "sam@example.com" }] }).lead).toBe(
      "sam@example.com",
    );
    expect(tab({ subject: "Re:  " }).lead).toBe("New message");
  });
});

/**
 * The recipient leads until it turns out not to be anybody.
 *
 * The reported label is the first case: `⌥1 Transactional Email Reply Inbox ·
 * Your…`, where the name is what Delta's mail service prints on every receipt
 * and the subject — the half that would have told this draft from another —
 * was the half that fell off the end.
 *
 * The rest of these are the cases the rule must *not* fire on, and they are why
 * it reads the address rather than the display name. "Transactional Email Reply
 * Inbox" and "Delta Air Lines" are the same shape as each other; their
 * addresses are not.
 */
describe("a recipient that identifies nobody", () => {
  it("steps aside for the subject when its address carries a routing token", () => {
    const one = tab({
      to: [
        {
          name: "Transactional Email Reply Inbox",
          email: "reply-H3DFZJSV4PQEFIHGBKJDBP6IAA.10202@t.delta.com",
        },
      ],
      subject: "Re: Your Delta flight receipt",
    });
    expect(one.lead).toBe("Your Delta flight receipt");
    expect(one.trail).toBe("Transactional Email Reply Inbox");
  });

  it("steps aside for a mailbox that says nobody reads it", () => {
    for (const email of [
      "no-reply@members.example",
      "noreply@members.example",
      "do.not.reply@members.example",
      "noreply-orders@shop.example",
      "notifications@social.example",
      "bounces@lists.example",
      "mailer-daemon@mx.example",
    ]) {
      expect(tab({ to: [{ name: "Some Service", email }], subject: "Order 4471" }).lead).toBe(
        "Order 4471",
      );
    }
  });

  it("leaves a person alone, however corporate their name reads", () => {
    const one = tab({
      to: [{ name: "Delta Air Lines", email: "customer.care@delta.com" }],
      subject: "Refund for flight DL219",
    });
    expect(one.lead).toBe("Delta Air Lines");
    expect(one.trail).toBe("Refund for flight DL219");
  });

  // A desk somebody sits at. Writing to it is writing to the company, and the
  // company is the useful half of the label.
  it("leaves a staffed role address alone", () => {
    for (const email of [
      "support@stripe.example",
      "info@paperbark.example",
      "sales@northloop.example",
      "billing@northloop.example",
    ]) {
      expect(tab({ to: [{ name: "Paperbark", email }], subject: "Invoice 12" }).lead).toBe(
        "Paperbark",
      );
    }
  });

  // Thirteen characters with a birth year in them is a person. The bar for a
  // routing token is sixteen, so this one is nowhere near it.
  it("leaves an address with digits in it alone", () => {
    expect(
      tab({ to: [{ email: "johnsmith1985@example.com" }], subject: "Saturday" }).lead,
    ).toBe("johnsmith1985@example.com");
    expect(
      tab({ to: [{ name: "Ada Lovelace", email: "ada.lovelace2@example.com" }], subject: "Notes" })
        .lead,
    ).toBe("Ada Lovelace");
  });

  // A long name is not a token; a token has digits in it. `bartholomewsinclair`
  // is nineteen characters of somebody.
  it("leaves a long all-letters local part alone", () => {
    expect(
      tab({ to: [{ email: "bartholomewsinclair@example.com" }], subject: "Lunch" }).lead,
    ).toBe("bartholomewsinclair@example.com");
  });

  // Nothing to step aside *for*. A generic "Reply" would be worse than the
  // boilerplate, which at least says which service.
  it("keeps the machine name when there is no subject to lead instead", () => {
    const one = tab({
      kind: "reply",
      to: [{ name: "Transactional Email Reply Inbox", email: "no-reply@t.delta.com" }],
    });
    expect(one.lead).toBe("Transactional Email Reply Inbox");
    expect(one.trail).toBeNull();
  });

  it("still counts the other recipients when the first one steps aside", () => {
    const one = tab({
      to: [
        { name: "Notifications", email: "noreply@social.example" },
        { name: "Deb Feldman", email: "deb@feldmanlegal.example" },
      ],
      subject: "Escalation",
    });
    expect(one.lead).toBe("Escalation");
    expect(one.trail).toBe("Notifications +1");
  });
});

describe("the strip and the keyboard, from one list", () => {
  const drafts = Array.from({ length: 11 }, (_, index) =>
    draft({ id: `d${index + 1}`, subject: `Draft ${index + 1}` }),
  );

  it("gives every open composer a tab, in the order they were opened", () => {
    expect(composerTabs(drafts).map((tab) => tab.id)).toEqual(drafts.map((entry) => entry.id));
  });

  it("puts the nth composer on ⌥n", () => {
    const tabs = composerTabs(drafts);
    for (let index = 0; index < KEYED_COMPOSERS; index += 1) {
      expect(tabs[index]!.keys).toBe(`alt+${index + 1}`);
    }
  });

  // There is no ⌥10, so the tenth composer has a tab and no chord — which is a
  // tab that must still be reachable, and is, with ← →.
  it("leaves a composer past the ninth with a tab and no key", () => {
    const tabs = composerTabs(drafts);
    expect(tabs).toHaveLength(11);
    expect(tabs[KEYED_COMPOSERS]!.keys).toBeNull();
    expect(tabs[tabs.length - 1]!.keys).toBeNull();
  });

  it("draws a strip for a single draft, which is where the chord is learned", () => {
    const tabs = composerTabs([draft()]);
    expect(tabs).toHaveLength(1);
    expect(tabs[0]!.keys).toBe("alt+1");
  });

  /**
   * The dock never spells a composer's chord itself.
   *
   * The keys and the strip used to be a `drafts.slice(0, 9)` and a
   * `drafts.map()` sitting a thousand lines apart, agreeing because the two
   * expressions happened to match. Either could have been changed alone. This
   * is what stops the second expression coming back: every `alt+` in
   * `ComposerDock.tsx` must arrive from `composerTabs`.
   */
  it("leaves ComposerDock with no chord of its own to get wrong", () => {
    const source = readFileSync(
      fileURLToPath(new URL("./ComposerDock.tsx", import.meta.url)),
      "utf8",
    );
    const code = source
      // Comments are allowed to name the keys; they are what explains them.
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/^\s*\/\/.*$/gm, "");
    // A digit, so ⌘⌥↑ and ⌘⌥↓ — the resizer's, and nothing to do with the
    // strip — are not what this catches.
    expect(code).not.toMatch(/alt\+[1-9]/);
    expect(code).toContain("composerTabs(drafts)");
  });
});
