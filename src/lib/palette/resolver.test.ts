import { describe, expect, it, vi } from "vitest";
import type { MailboxTarget } from "@/lib/mailboxes";
import { KIND_ORDER, mailboxResolver, resolve, type PaletteContext } from "./resolver";

const MAILBOXES: MailboxTarget[] = [
  { id: "INBOX", name: "Inbox", accountId: null, kind: "system" },
  { id: "Label_7", name: "Receipts", accountId: 1, kind: "user" },
  { id: "Label_9", name: "Boomerang-Outbox", accountId: 4, kind: "user" },
];

function context(query: string, overrides: Partial<PaletteContext> = {}): PaletteContext {
  return {
    query,
    threads: [],
    events: [],
    people: [],
    mailboxes: MAILBOXES,
    commands: [],
    actions: {
      openThread: vi.fn(),
      openEvent: vi.fn(),
      openMailbox: vi.fn(),
      runCommand: vi.fn(),
      composeTo: vi.fn(),
    },
    ...overrides,
  };
}

describe("mailboxResolver", () => {
  it("finds a label by name", () => {
    const results = mailboxResolver.resolve(context("receip"));
    expect(results.map((r) => r.title)).toEqual(["Receipts"]);
  });

  it("opens the label it matched", () => {
    const ctx = context("boomerang");
    mailboxResolver.resolve(ctx)[0]!.run();
    expect(ctx.actions.openMailbox).toHaveBeenCalledWith("Label_9");
  });

  it("says whether a hit is a mailbox or a label", () => {
    expect(mailboxResolver.resolve(context("inbox"))[0]!.meta).toBe("mailbox");
    expect(mailboxResolver.resolve(context("receipts"))[0]!.meta).toBe("label");
  });

  it("stays quiet on an empty query and in command mode", () => {
    expect(mailboxResolver.resolve(context("  "))).toEqual([]);
    expect(mailboxResolver.claims(">arch")).toBe(false);
  });

  it("keys results by account too, so two Receipts are two rows", () => {
    const ctx = context("receipts", {
      mailboxes: [
        { id: "Label_7", name: "Receipts · Northwind", accountId: 1, kind: "user" },
        { id: "Label_8", name: "Receipts · Personal", accountId: 4, kind: "user" },
      ],
    });
    const ids = mailboxResolver.resolve(ctx).map((r) => r.id);
    expect(new Set(ids).size).toBe(2);
  });
});

describe("commandResolver", () => {
  const commands = [
    { id: "favorite-thread", title: "Favorite this conversation", keywords: "favorite pin sidebar bookmark conversation thread" },
    { id: "archive", title: "Archive conversation", keywords: "archive done" },
  ];

  it("does not scatter-match a command over the labels the user meant", () => {
    // "boomer" subsequence-matches "bookmark conversation"; it must not beat
    // the Boomerang labels to the top of the list.
    expect(resolve(context("boomer", { commands })).map((r) => r.kind)).not.toContain("command");
  });

  it("still offers a command on a real substring hit", () => {
    const titles = resolve(context("favorite", { commands })).map((r) => r.title);
    expect(titles).toContain("Favorite this conversation");
  });

  it("keeps loose matching inside `>` mode, where commands are what was asked for", () => {
    const results = resolve(context(">boomer", { commands }));
    expect(results.every((r) => r.kind === "command")).toBe(true);
    expect(results.length).toBeGreaterThan(0);
  });
});

describe("the chain", () => {
  it("includes mailboxes in the default resolver set", () => {
    const results = resolve(context("inbox"));
    expect(results.some((r) => r.kind === "mailbox")).toBe(true);
  });

  it("ranks mailboxes above mail, since a named label is an intent to navigate", () => {
    expect(KIND_ORDER.indexOf("mailbox")).toBeLessThan(KIND_ORDER.indexOf("thread"));
  });
});
