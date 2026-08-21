/**
 * What this guards is the *shape* of the rail and the keyboard's route through
 * it — the two things that are easy to lose without noticing.
 *
 * The shape, because the rail's whole claim is that an inbox is a place and the
 * accounts are filters within it; the moment a row escapes its section or the
 * folding stops removing rows from the model, the pointer and the keyboard
 * start walking different lists and only one of them is on screen.
 *
 * The keyboard, because a standing rule of this app is that everything is
 * reachable without a mouse, and a `<div onClick>` renders identically to a
 * `<button>` — the only thing that would tell you is a Tab key nobody presses
 * in a test. So the markup assertions are on the *elements*: real buttons, real
 * accessible names, real `aria-expanded`, via `react-dom/server`, with no jsdom
 * and nothing to click.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Account, Label, Thread } from "@/types";
import type { Favorite } from "@/lib/favorites";
import { RailRow } from "./AccountRail";
import { railItems, railStep, type RailHandlers, type RailItem } from "./rail-model";
import { countByAccount, type InboxUnread } from "./use-inbox-unread";

/* ------------------------------------------------------------------ fixtures */

function account(id: number, email: string): Account {
  return { id, email, name: email, colorIndex: 1, kind: "personal" };
}

function mailbox(id: string, name: string): Label {
  return { id, accountId: null, name, kind: "system" };
}

function thread(id: number, accountId: number): Thread {
  return {
    id,
    accountId,
    subject: "s",
    snippet: "s",
    participants: [],
    timestamp: 0,
    unread: true,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
  };
}

const ACCOUNTS = [
  account(1, "alex@northwind.example"),
  account(2, "alex@lumen.example"),
  account(3, "alex@talleres.example"),
];

const MAILBOXES = [
  mailbox("STARRED", "Starred"),
  mailbox("SNOOZED", "Snoozed"),
  mailbox("SENT", "Sent"),
  mailbox("ARCHIVE", "Archive"),
];

function unread(counts: Record<number, number> = {}, capped = false): InboxUnread {
  const byAccount = new Map(Object.entries(counts).map(([id, n]) => [Number(id), n]));
  let total = 0;
  for (const n of byAccount.values()) total += n;
  return { byAccount, total, capped };
}

const CALLS: string[] = [];
const HANDLERS: RailHandlers = {
  open: (accountId, labelId) => CALLS.push(`open:${accountId ?? "all"}:${labelId}`),
  openLabel: (labelId) => CALLS.push(`label:${labelId}`),
  openFavorite: () => CALLS.push("favorite"),
  unfavorite: (key) => CALLS.push(`unfavorite:${key}`),
  toggle: (section) => CALLS.push(`toggle:${section}`),
};

function build(over: Partial<Parameters<typeof railItems>[0]> = {}): RailItem[] {
  return railItems(
    {
      accounts: ACCOUNTS,
      mailboxes: MAILBOXES,
      favorites: [],
      accountId: null,
      labelId: "INBOX",
      threadId: null,
      unread: unread(),
      collapsed: [],
      ...over,
    },
    HANDLERS,
  );
}

const keys = (items: RailItem[]) => items.map((item) => item.key);
const selected = (items: RailItem[]) => items.filter((i) => i.active).map((i) => i.key);

/* --------------------------------------------------------------- the shape */

describe("the rail's shape", () => {
  it("heads with Inbox and nests the accounts under it", () => {
    const items = build();
    expect(keys(items).slice(0, 4)).toEqual([
      "section:inbox",
      "account:1",
      "account:2",
      "account:3",
    ]);
    expect(items[0]?.level).toBe(1);
    expect(items[1]?.level).toBe(2);
  });

  it("does not put Calendar in the rail — that is a surface of the window", () => {
    const items = build();
    expect(keys(items)).not.toContain("surface:calendar");
    expect(keys(items).indexOf("section:folders")).toBe(4);
  });

  it("does not repeat Inbox among the folders", () => {
    const items = build({ mailboxes: [mailbox("INBOX", "Inbox"), ...MAILBOXES] });
    // The caller filters INBOX out; what this pins is that the model does not
    // put it back — Inbox is the section, not a row inside one.
    expect(keys(items).filter((k) => k === "mailbox:INBOX")).toHaveLength(1);
    expect(keys(items).indexOf("section:inbox")).toBe(0);
  });

  it("has no Add account row — that is a setting, not a place", () => {
    expect(keys(build()).some((k) => k.includes("add-account"))).toBe(false);
  });

  it("shows the Favorites section only when something is favorited", () => {
    expect(keys(build()).includes("section:favorites")).toBe(false);

    const favorite: Favorite = {
      kind: "mailbox",
      labelId: "Label_9",
      accountId: null,
      name: "Receipts",
    };
    const items = build({ favorites: [favorite] });
    expect(keys(items)).toContain("section:favorites");
    expect(keys(items)).toContain("favorite:mailbox:all:Label_9");
  });
});

/* ------------------------------------------------------------- the folding */

describe("folding", () => {
  it("removes a section's rows from the model, not just from the paint", () => {
    const items = build({ collapsed: ["inbox"] });
    expect(keys(items)).toContain("section:inbox");
    expect(keys(items).some((k) => k.startsWith("account:"))).toBe(false);
    expect(items[0]?.expanded).toBe(false);
  });

  it("folds the folders independently of the inbox", () => {
    const items = build({ collapsed: ["folders"] });
    expect(keys(items).some((k) => k.startsWith("account:"))).toBe(true);
    expect(keys(items).some((k) => k.startsWith("mailbox:"))).toBe(false);
  });

  it("lets a folded heading carry the mark from the row it is hiding", () => {
    // In Sent, with Folders folded: the rail must still answer "where am I".
    expect(selected(build({ labelId: "SENT", collapsed: ["folders"] }))).toEqual([
      "section:folders",
    ]);
    // And with an account's inbox picked and Inbox folded.
    expect(selected(build({ labelId: "INBOX", accountId: 2, collapsed: ["inbox"] }))).toEqual([
      "section:inbox",
    ]);
  });

  it("never lights two rows at once", () => {
    for (const over of [
      {},
      { accountId: 2 },
      { labelId: "SENT" },
      { labelId: "SENT", accountId: 2 },
      { collapsed: ["inbox", "folders"] },
    ]) {
      expect(selected(build(over)).length).toBeLessThanOrEqual(1);
    }
  });
});

/* ------------------------------------------------------------ what rows do */

describe("what a row means", () => {
  it("makes an account row mean that account's inbox", () => {
    CALLS.length = 0;
    build().find((i) => i.key === "account:2")?.activate?.();
    expect(CALLS).toEqual(["open:2:INBOX"]);
  });

  it("makes the Inbox heading mean every account's inbox", () => {
    CALLS.length = 0;
    build()[0]?.activate?.();
    expect(CALLS).toEqual(["open:all:INBOX"]);
  });

  it("opens Primary when that is what Inbox means", () => {
    CALLS.length = 0;
    const items = build({ inboxId: "PRIMARY", labelId: "PRIMARY" });
    items[0]?.activate?.();
    items.find((i) => i.key === "account:2")?.activate?.();
    expect(CALLS).toEqual(["open:all:PRIMARY", "open:2:PRIMARY"]);
    expect(selected(items)).toEqual(["section:inbox"]);
  });

  it("leaves a folder row's account filter alone", () => {
    CALLS.length = 0;
    build({ accountId: 2 }).find((i) => i.key === "mailbox:SENT")?.activate?.();
    expect(CALLS).toEqual(["label:SENT"]);
  });

  it("marks an account row active only in the inbox", () => {
    expect(selected(build({ accountId: 2 }))).toEqual(["account:2"]);
    expect(selected(build({ accountId: 2, labelId: "SENT" }))).toEqual(["mailbox:SENT"]);
  });
});

/* ---------------------------------------------------------------- the counts */

describe("the unread counts", () => {
  it("puts each account's count on its row and the sum on Inbox", () => {
    const items = build({ unread: unread({ 1: 3, 2: 1 }) });
    expect(items[0]?.count).toBe(4);
    expect(items.find((i) => i.key === "account:1")?.count).toBe(3);
    expect(items.find((i) => i.key === "account:2")?.count).toBe(1);
    expect(items.find((i) => i.key === "account:3")?.count).toBe(0);
  });

  it("says a capped count is a floor rather than claiming it is exact", () => {
    const items = build({ unread: unread({ 1: 500 }, true) });
    expect(items[0]?.countSuffix).toBe("+");
    expect(items.find((i) => i.key === "account:1")?.countSuffix).toBe("+");
  });

  it("counts by account and drops what the UI has already taken away", () => {
    const threads = [thread(11, 1), thread(12, 1), thread(21, 2)];
    expect([...countByAccount(threads, new Set())]).toEqual([
      [1, 2],
      [2, 1],
    ]);
    // An archive-everything gesture: the badge falls with the rows, not a round
    // trip later.
    expect([...countByAccount(threads, new Set([11, 12, 21]))]).toEqual([]);
    expect([...countByAccount(threads, new Set([11]))]).toEqual([
      [1, 1],
      [2, 1],
    ]);
  });
});

/* --------------------------------------------------------------- the keyboard */

describe("the arrow keys", () => {
  const open = build();
  const index = (items: RailItem[], key: string) => keys(items).indexOf(key);

  it("folds the section you are standing on", () => {
    expect(railStep(open, index(open, "section:inbox"), "out")).toEqual({
      kind: "toggle",
      section: "inbox",
    });
  });

  it("steps out to the heading when there is nothing to fold", () => {
    expect(railStep(open, index(open, "account:2"), "out")).toEqual({
      kind: "move",
      index: index(open, "section:inbox"),
    });
    expect(railStep(open, index(open, "mailbox:SENT"), "out")).toEqual({
      kind: "move",
      index: index(open, "section:folders"),
    });
  });

  it("unfolds a folded section before stepping into it", () => {
    const folded = build({ collapsed: ["folders"] });
    const at = index(folded, "section:folders");
    expect(railStep(folded, at, "in")).toEqual({ kind: "toggle", section: "folders" });
    // Once open, the same key walks in.
    expect(railStep(open, index(open, "section:folders"), "in")).toEqual({
      kind: "move",
      index: index(open, "section:folders") + 1,
    });
  });

  it("does nothing at the edges rather than wrapping", () => {
    expect(railStep(open, index(open, "mailbox:ARCHIVE"), "in")).toEqual({ kind: "none" });
    expect(railStep(open, 999, "out")).toEqual({ kind: "none" });
    expect(railStep(open, 0, "out")).toEqual({ kind: "toggle", section: "inbox" });
  });
});

/* ----------------------------------------------------------------- the markup */

describe("a rail row", () => {
  const row = (item: RailItem, focused = false, onToggle?: () => void) =>
    renderToStaticMarkup(<RailRow item={item} index={0} focused={focused} onToggle={onToggle} />);

  it("is a button, so the keyboard can reach it", () => {
    const html = row(build()[0]!);
    expect(html).toContain("<button");
    expect(html).not.toContain("<div onclick");
    expect(html).toContain('role="treeitem"');
  });

  it("announces its depth, its selection and whether it is folded", () => {
    const items = build({ accountId: 2 });
    const heading = row(items[0]!, false, () => {});
    expect(heading).toContain('aria-level="1"');
    expect(heading).toContain('aria-expanded="true"');

    const folded = row(build({ collapsed: ["inbox"] })[0]!, false, () => {});
    expect(folded).toContain('aria-expanded="false"');

    const account = row(items.find((i) => i.key === "account:2")!);
    expect(account).toContain('aria-level="2"');
    expect(account).toContain('aria-selected="true"');
  });

  it("gives the disclosure an accessible name of its own", () => {
    expect(row(build()[0]!, false, () => {})).toContain('aria-label="Collapse Inbox"');
    expect(row(build({ collapsed: ["inbox"] })[0]!, false, () => {})).toContain(
      'aria-label="Expand Inbox"',
    );
  });

  it("omits the disclosure on a row that has nothing to fold", () => {
    const sent = build().find((i) => i.key === "mailbox:SENT")!;
    expect(row(sent)).not.toContain('aria-label="Collapse');
    expect(row(sent)).not.toContain("aria-expanded");
  });

  it("truncates a long address but keeps the whole of it in the tooltip", () => {
    const account = build().find((i) => i.key === "account:1")!;
    const html = row(account);
    expect(html).toContain("truncate");
    expect(account.title).toBe("alex@northwind.example");
    expect(account.shortcut).toBe("ctrl+1");
    // The address is the row's label and the tooltip's, so a truncated paint
    // still has the whole string to hover.
    expect(html).toContain("alex@northwind.example");
  });

  it("puts the jump key on Inbox and the folders it opens", () => {
    const items = build();
    expect(items.find((i) => i.key === "section:inbox")?.shortcut).toBe("g i");
    expect(items.find((i) => i.key === "mailbox:STARRED")?.shortcut).toBe("g s");
    expect(items.find((i) => i.key === "mailbox:SENT")?.shortcut).toBe("g t");
  });

  it("puts All's jump on the All row, not on Inbox", () => {
    const items = build({ mailboxes: [mailbox("INBOX", "All"), ...MAILBOXES] });
    expect(items.find((i) => i.key === "mailbox:INBOX")?.shortcut).toBe("g shift+i");
    expect(items.find((i) => i.key === "section:inbox")?.shortcut).toBe("g i");
  });

  it("puts the keyboard's cursor in the tab order and nothing else", () => {
    expect(row(build()[0]!, true)).toContain('tabindex="0"');
    expect(row(build()[0]!, false)).toContain('tabindex="-1"');
  });
});
