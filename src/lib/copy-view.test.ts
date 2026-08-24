import { beforeEach, describe, expect, it } from "vitest";
import type { CalendarEvent, Thread } from "@/types";
import type { ShellView } from "./agent";
import {
  clearCopyRequest,
  copyContextText,
  copyRequest,
  copyScore,
  copyViewResolver,
  copyableContext,
  copyableMessage,
  describeCopy,
  publishSearch,
} from "./copy-view";
import { resolve, type PaletteContext } from "./palette/resolver";

/* -------------------------------------------------------------------------- */
/* fixtures                                                                    */
/* -------------------------------------------------------------------------- */

const thread: Thread = {
  id: 11,
  accountId: 1,
  subject: "Series A data room",
  snippet: "Any chance you can send the link?",
  participants: [{ name: "Tawny Chen", email: "tawny@example.com" }],
  timestamp: 1_754_000_000_000,
  unread: true,
  starred: false,
  hasAttachment: false,
  messageCount: 2,
  labelIds: ["INBOX"],
};

const other: Thread = { ...thread, id: 12, subject: "Invoice 4471" };

const event: CalendarEvent = {
  id: 42,
  calendarId: "alex@example.com",
  accountId: 1,
  title: "Partner meeting",
  start: 1_754_000_000_000,
  end: 1_754_003_600_000,
  allDay: false,
  location: "Room 4",
  attendees: [],
};

const mailbox: ShellView = {
  mode: "mail",
  calendarView: "week",
  anchor: 1_754_000_000_000,
  labelId: "INBOX",
  mailboxName: "Inbox",
  threadId: null,
  eventId: null,
};

function paletteContext(query: string): PaletteContext {
  return {
    query,
    threads: [],
    events: [],
    people: [],
    mailboxes: [],
    commands: [],
    actions: {
      openThread: () => {},
      openEvent: () => {},
      openMailbox: () => {},
      runCommand: () => {},
      composeTo: () => {},
    },
  };
}

beforeEach(() => {
  publishSearch(null);
  clearCopyRequest();
});

/* -------------------------------------------------------------------------- */
/* what each surface copies                                                    */
/* -------------------------------------------------------------------------- */

describe("what ⌘⌥C copies, per surface", () => {
  it("an open conversation copies the conversation, and not the list beside it", () => {
    const items = copyableContext({
      ...mailbox,
      threadId: 11,
      openThread: thread,
      visibleThreads: [thread, other],
    });
    // The id is what Rust resolves the messages from; the label is only what
    // the toast reads back.
    expect(items[0]).toMatchObject({ kind: "thread", threadId: 11 });
    // The forty subject lines around it were most of the payload and none of
    // the ask.
    expect(items.some((i) => i.id === "listing")).toBe(false);
  });

  it("a conversation opened out of a search still carries the query", () => {
    publishSearch({ query: "from:tawny", results: [thread, other] });
    const items = copyableContext({ ...mailbox, threadId: 11, openThread: thread });
    expect(items[0]).toMatchObject({ kind: "thread", threadId: 11 });
    expect(items.some((i) => i.kind === "search")).toBe(true);
    expect(items.some((i) => i.id === "listing")).toBe(false);
  });

  /*
   * It used to copy the row *and* the index beside it, on the grounds that the
   * list is context. Reported as "copy for llm seems to copy the entire
   * messages index too though which is stupid": a numbered list of thirty-four
   * unrelated subject lines was most of the payload, and none of it was the
   * conversation he pointed at. A conversation under the cursor is as named as
   * one in the reading pane.
   */
  it("a list with a row under the cursor copies that conversation and not the index", () => {
    const items = copyableContext({
      ...mailbox,
      selectedThread: thread,
      visibleThreads: [thread, other],
    });
    expect(items[0]).toMatchObject({ kind: "thread", threadId: 11 });
    expect(items.some((i) => i.id === "listing")).toBe(false);
  });

  /*
   * And the index is still the answer when there is no conversation to name —
   * which is what keeps "what am I looking at" answerable in a mailbox nobody
   * has moved the cursor in.
   */
  it("a list with nothing under the cursor copies the rows in view", () => {
    const items = copyableContext({ ...mailbox, visibleThreads: [thread, other] });
    expect(items.some((i) => i.kind === "thread")).toBe(false);
    const listing = items.find((i) => i.id === "listing");
    expect(listing?.detail).toContain("Invoice 4471");
  });

  it("a mailbox with nothing selected still copies something", () => {
    // The thinnest surface in the app. A copy that refused here would be a key
    // that sometimes does nothing.
    const items = copyableContext({ ...mailbox, visibleThreads: [] });
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: "mailbox", label: "Inbox" });
  });

  it("a search copies the query and its results, not the mailbox underneath", () => {
    publishSearch({ query: "from:tawny", results: [other] });
    const items = copyableContext({ ...mailbox, visibleThreads: [thread] });

    expect(items.some((i) => i.kind === "search" && i.label.includes("from:tawny"))).toBe(true);
    const listing = items.find((i) => i.id === "listing");
    expect(listing?.detail).toContain("Invoice 4471");
    expect(listing?.detail).toContain('matching "from:tawny"');
    expect(listing?.detail).not.toContain("Series A data room");
  });

  it("a closed search hands the mailbox straight back", () => {
    publishSearch({ query: "from:tawny", results: [other] });
    publishSearch(null);
    const items = copyableContext({ ...mailbox, visibleThreads: [thread] });
    expect(items.some((i) => i.kind === "search")).toBe(false);
    expect(items.find((i) => i.id === "listing")?.detail).toContain("Series A data room");
  });

  it("a selected event copies the event and the range it sits in", () => {
    const items = copyableContext({
      ...mailbox,
      mode: "calendar",
      eventId: 42,
      selectedEvent: event,
      visibleEvents: [event],
    });
    expect(items[0]).toMatchObject({ kind: "event", eventId: 42 });
    expect(items.find((i) => i.kind === "day")?.detail).toMatch(/unix milliseconds/);
    expect(items.some((i) => i.id === "listing")).toBe(false);
  });

  it("the calendar with nothing selected copies the range and what is in it", () => {
    const items = copyableContext({
      ...mailbox,
      mode: "calendar",
      visibleEvents: [event],
    });
    expect(items.some((i) => i.kind === "event")).toBe(false);
    expect(items.find((i) => i.id === "listing")?.detail).toContain("Room 4");
  });
});

/* -------------------------------------------------------------------------- */
/* saying what happened                                                        */
/* -------------------------------------------------------------------------- */

describe("what the toast says", () => {
  it("names the conversation, not the act", () => {
    const items = copyableContext({ ...mailbox, threadId: 11, openThread: thread });
    expect(describeCopy(items, false)).toBe("Copied “Series A data room”");
  });

  /*
   * The rows come along only when no conversation was named — see the surface
   * table. A cursor on a row used to bring them too, and the index was most of
   * what landed on the clipboard.
   */
  it("counts the rows when they came along too", () => {
    const items = copyableContext({ ...mailbox, visibleThreads: [thread, other] });
    expect(describeCopy(items, false)).toBe("Copied “Inbox” and 2 conversations in view");
  });

  it("names one message when that is what was copied", () => {
    const items = copyableMessage({
      threadId: 11,
      messageId: 91,
      subject: "Series A data room",
      from: "Tawny Chen",
    });
    expect(describeCopy(items, false)).toBe("Copied “Tawny Chen — Series A data room”");
  });

  it("says so when the cap bit, rather than trimming quietly", () => {
    const items = copyableContext({ ...mailbox, threadId: 11, openThread: thread });
    expect(describeCopy(items, true)).toBe("Copied “Series A data room” — trimmed to fit");
  });

  it("still says something when there is nothing but a mailbox", () => {
    expect(describeCopy(copyableContext(mailbox), false)).toBe("Copied “Inbox”");
  });
});

/* -------------------------------------------------------------------------- */
/* the clipboard itself                                                        */
/* -------------------------------------------------------------------------- */

describe("the copy call", () => {
  it("says what it needs when there is no desktop app behind it", async () => {
    // A browser tab has no pasteboard to write to, and the sentence names the
    // remedy rather than reporting a rejected promise.
    await expect(copyContextText([])).rejects.toThrow(/desktop app/);
  });
});

/* -------------------------------------------------------------------------- */
/* ⌘K                                                                          */
/* -------------------------------------------------------------------------- */

describe("the ⌘K row", () => {
  it("is registered in the resolver chain", () => {
    const results = resolve(paletteContext(">"));
    expect(results.some((r) => r.id === "command:copy-view")).toBe(true);
  });

  it("answers to the words somebody would actually type", () => {
    expect(copyScore("copy")).toBeGreaterThan(0);
    expect(copyScore("clipboard")).toBeGreaterThan(0);
    expect(copyScore("llm")).toBeGreaterThan(0);
    expect(copyScore("paste")).toBeGreaterThan(0);
  });

  it("stays out of the way of an ordinary search", () => {
    expect(copyScore("tawny")).toBe(0);
    expect(copyScore("invoice")).toBe(0);
  });

  it("records a request rather than copying from a function with no shell", () => {
    copyViewResolver.resolve(paletteContext("copy"))[0]?.run();
    expect(copyRequest()?.nonce).toBe(1);
    copyViewResolver.resolve(paletteContext("copy"))[0]?.run();
    expect(copyRequest()?.nonce).toBe(2);
  });

  it("wears its binding, so pressing it once teaches the key", () => {
    const row = copyViewResolver.resolve(paletteContext("copy"))[0];
    expect(row?.meta).toBe("⌘⌥C");
  });
});
