/**
 * Two rows on the rail carry a number, and the rest carry none.
 *
 * The second half is the part that erodes. Every mail client eventually grows a
 * Spam count, and this one must not: 354 of the owner's 384 spam threads are
 * unread, because nobody reads spam, and neither that number nor the total is
 * anything he can act on. Sent, Starred and Promotions are archives of finished
 * things. So the list of counted rows is asserted as a closed set rather than
 * one row at a time — a new count anywhere fails this.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Account, Label } from "@/types";
import type { MailboxCounts } from "@/lib/data";
import { RailRow } from "./AccountRail";
import { railItems, type RailHandlers, type RailItem } from "./rail-model";

const HANDLERS: RailHandlers = {
  open: () => {},
  openLabel: () => {},
  openFavorite: () => {},
  unfavorite: () => {},
  toggle: () => {},
  openUnsent: () => {},
};

const mailbox = (id: string, name: string): Label => ({
  id,
  accountId: null,
  name,
  kind: "system",
});

/** Every row the rail can show, so the "nothing else" claim covers all of it. */
const MAILBOXES = [
  mailbox("INBOX", "All"),
  mailbox("STARRED", "Starred"),
  mailbox("SNOOZED", "Snoozed"),
  mailbox("DRAFT", "Drafts"),
  mailbox("SENT", "Sent"),
  mailbox("ARCHIVE", "Archive"),
  mailbox("SPAM", "Spam"),
  mailbox("TRASH", "Trash"),
  mailbox("CATEGORY_PROMOTIONS", "Promotions"),
];

const ACCOUNT: Account = {
  id: 1,
  email: "alex@northwind.example",
  name: "alex@northwind.example",
  colorIndex: 1,
  kind: "personal",
};

function build(counts: MailboxCounts): RailItem[] {
  return railItems(
    {
      accounts: [ACCOUNT],
      mailboxes: MAILBOXES,
      favorites: [],
      accountId: null,
      labelId: "INBOX",
      threadId: null,
      unread: { byAccount: new Map(), total: 0, capped: false },
      counts,
      unsent: 0,
      collapsed: [],
    },
    HANDLERS,
  );
}

const mailboxRow = (items: RailItem[], id: string) =>
  items.find((item) => item.key === `mailbox:${id}`)!;

const html = (item: RailItem) =>
  renderToStaticMarkup(<RailRow item={item} index={0} focused={false} />);

describe("the counts on the rail", () => {
  it("puts a total on Drafts and on Snoozed", () => {
    const items = build({ drafts: 3, snoozed: 12 });
    expect(mailboxRow(items, "DRAFT").count).toBe(3);
    expect(mailboxRow(items, "SNOOZED").count).toBe(12);
  });

  it("counts nothing on Spam, Trash, Sent or Promotions", () => {
    const items = build({ drafts: 3, snoozed: 12 });
    for (const id of ["SPAM", "TRASH", "SENT", "CATEGORY_PROMOTIONS"]) {
      expect(mailboxRow(items, id).count, `${id} carries a count`).toBeUndefined();
    }
  });

  it("counts nothing on any row but those two", () => {
    const counted = build({ drafts: 3, snoozed: 12 })
      .filter((item) => item.key.startsWith("mailbox:") && item.count !== undefined)
      .map((item) => item.key);
    expect(counted).toEqual(["mailbox:SNOOZED", "mailbox:DRAFT"]);
  });

  it("draws nothing at zero — no 0, no empty badge", () => {
    const items = build({ drafts: 0, snoozed: 0 });
    expect(mailboxRow(items, "DRAFT").count).toBeUndefined();
    expect(mailboxRow(items, "SNOOZED").count).toBeUndefined();
    // The paint, not just the model: an empty Drafts row is the row it always
    // was, with the label and nothing after it.
    const painted = html(mailboxRow(items, "DRAFT"));
    expect(painted).toContain("Drafts");
    expect(painted).not.toContain("tabular-nums");
  });

  it("paints the number when there is one", () => {
    const painted = html(mailboxRow(build({ drafts: 4, snoozed: 0 }), "DRAFT"));
    expect(painted).toContain("tabular-nums");
    expect(painted).toContain("4");
    // A total, so no `+` — that suffix belongs to the capped unread counts.
    expect(painted).not.toContain("4+");
  });

  it("is a total, not an unread count — one draft, none of them unread", () => {
    // The owner's own store: Drafts is 1 thread and 0 unread. An unread count
    // on this row would show nothing, ever.
    expect(mailboxRow(build({ drafts: 1, snoozed: 0 }), "DRAFT").count).toBe(1);
  });
});
