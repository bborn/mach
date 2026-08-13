import { describe, expect, it } from "vitest";
import type { Label } from "@/types";
import {
  mailboxName,
  mailboxTargets,
  railMailboxes,
  withVirtualMailboxes,
} from "./mailboxes";

function system(id: string, name = id): Label {
  return { id, accountId: null, name, kind: "system" };
}

function user(id: string, name: string, accountId: number | null = null): Label {
  return { id, accountId, name, kind: "user" };
}

describe("mailboxName", () => {
  it("translates Gmail's shouting into words", () => {
    expect(mailboxName(system("INBOX"))).toBe("Inbox");
    expect(mailboxName(system("CATEGORY_PROMOTIONS"))).toBe("Promotions");
    expect(mailboxName(system("DRAFT"))).toBe("Drafts");
  });

  it("humanises system labels it has never heard of", () => {
    expect(mailboxName(system("YELLOW_STAR"))).toBe("Yellow star");
    expect(mailboxName(system("CATEGORY_MADE_UP"))).toBe("Made up");
  });

  it("leaves user labels exactly as the user named them", () => {
    expect(mailboxName(user("Label_12", "Boomerang-Outbox"))).toBe("Boomerang-Outbox");
    expect(mailboxName(user("Label_13", "DMARC"))).toBe("DMARC");
  });

  it("accepts a system label whose name, not id, is the Gmail constant", () => {
    expect(mailboxName({ id: "5", accountId: 1, name: "SENT", kind: "system" })).toBe("Sent");
  });
});

describe("railMailboxes", () => {
  const labels = [
    system("CATEGORY_FORUMS"),
    system("SENT"),
    system("UNREAD"),
    system("INBOX"),
    system("TRASH"),
    user("Label_1", "Investors"),
  ];

  it("keeps the canonical mailboxes, in canonical order", () => {
    expect(railMailboxes(labels).map((l) => l.id)).toEqual(["INBOX", "SENT", "TRASH"]);
  });

  it("drops the noise — categories, UNREAD, and every user label", () => {
    const ids = railMailboxes(labels).map((l) => l.id);
    expect(ids).not.toContain("CATEGORY_FORUMS");
    expect(ids).not.toContain("UNREAD");
    expect(ids).not.toContain("Label_1");
  });

  it("renames as it goes, so the rail never shows an id", () => {
    expect(railMailboxes(labels).map((l) => l.name)).toEqual(["Inbox", "Sent", "Trash"]);
  });

  it("shows nothing rather than guessing when no labels have loaded", () => {
    expect(railMailboxes([])).toEqual([]);
  });
});

describe("withVirtualMailboxes", () => {
  /* What `list_labels` actually returns for a real Gmail account: no ARCHIVE
     and no SNOOZED, because Gmail has neither. */
  const stored = [
    system("INBOX"),
    system("STARRED"),
    system("SENT"),
    system("DRAFT"),
    system("SPAM"),
    system("TRASH"),
    user("Label_112", "Mach/Snoozed", 1),
  ];

  it("adds the mailboxes Gmail has no label for", () => {
    const ids = withVirtualMailboxes(stored).map((l) => l.id);
    expect(ids).toContain("ARCHIVE");
    expect(ids).toContain("SNOOZED");
  });

  it("puts them in the rail, where the keyboard already went", () => {
    expect(railMailboxes(withVirtualMailboxes(stored)).map((l) => l.name)).toEqual([
      "Inbox",
      "Starred",
      "Snoozed",
      "Drafts",
      "Sent",
      "Archive",
      "Spam",
      "Trash",
    ]);
  });

  it("offers them to ⌘K too, so the two ways in agree", () => {
    const names = mailboxTargets(withVirtualMailboxes(stored), () => undefined).map((t) => t.name);
    expect(names).toContain("Archive");
    expect(names).toContain("Snoozed");
  });

  it("adds nothing before the first label list arrives", () => {
    // A rail whose only row is "Archive" is worse than a rail with no rows.
    expect(withVirtualMailboxes([])).toEqual([]);
  });

  it("does not duplicate a mailbox the store somehow already has", () => {
    const ids = withVirtualMailboxes([system("INBOX"), system("ARCHIVE")]).map((l) => l.id);
    expect(ids.filter((id) => id === "ARCHIVE")).toHaveLength(1);
  });
});

describe("mailboxTargets", () => {
  const names: Record<number, string> = { 1: "Northwind", 4: "Personal" };
  const accountName = (id: number) => names[id];

  it("offers every label, not just the ones in the rail", () => {
    const targets = mailboxTargets([system("CATEGORY_FORUMS"), user("L1", "Investors")], accountName);
    expect(targets.map((t) => t.name)).toEqual(["Forums", "Investors"]);
  });

  it("says which account owns a name two mailboxes share", () => {
    const targets = mailboxTargets(
      [user("L1", "Receipts", 1), user("L2", "Receipts", 4), user("L3", "Family", 4)],
      accountName,
    );
    expect(targets.map((t) => t.name)).toEqual([
      "Receipts · Northwind",
      "Receipts · Personal",
      "Family",
    ]);
  });

  it("leaves a unified label alone — it has no one account to name", () => {
    const targets = mailboxTargets([system("INBOX"), user("L1", "Inbox", 1)], accountName);
    expect(targets.map((t) => t.name)).toEqual(["Inbox", "Inbox · Northwind"]);
  });

  it("carries the kind through, so results can say mailbox or label", () => {
    const targets = mailboxTargets([system("INBOX"), user("L1", "Investors", 1)], accountName);
    expect(targets.map((t) => t.kind)).toEqual(["system", "user"]);
  });
});
