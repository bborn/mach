import { beforeEach, describe, expect, it } from "vitest";
import type { CalendarEvent, Thread } from "@/types";
import {
  agentResolver,
  askRequest,
  clearAsk,
  contextFor,
  looksLikeAQuestion,
  needsAttention,
  paletteQuery,
  reduceSession,
  reduceSessions,
  requestAsk,
  subscribeAsk,
  UNKNOWN_BACKEND,
  artifactAction,
  eventWhen,
  guestLine,
  loadBackendStatus,
  type AgentEvent,
  type AgentSession,
  type ShellView,
} from "./agent";
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

const event: CalendarEvent = {
  id: 42,
  calendarId: "alex@example.com",
  accountId: 1,
  title: "Partner meeting",
  start: 1_754_040_000_000,
  end: 1_754_043_600_000,
  allDay: false,
  attendees: [],
};

const mailView: ShellView = {
  mode: "mail",
  calendarView: "week",
  anchor: 1_754_000_000_000,
  labelId: "INBOX",
  mailboxName: "Inbox",
  threadId: null,
  eventId: null,
};

function session(overrides: Partial<AgentSession> = {}): AgentSession {
  return {
    id: "agent-1",
    title: "Reply to this next tues",
    status: "running",
    createdAt: 1,
    context: [],
    entries: [{ role: "user", text: "reply to this next tues" }],
    ...overrides,
  };
}

function paletteContext(query: string): PaletteContext {
  return {
    query,
    threads: [thread],
    events: [event],
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

/* -------------------------------------------------------------------------- */
/* the palette seam                                                            */
/* -------------------------------------------------------------------------- */

describe("the ⌘K handoff", () => {
  beforeEach(() => clearAsk());

  it("is registered in the resolver chain", () => {
    const results = resolve(paletteContext("reply to this next tuesday"));
    expect(results.some((r) => r.kind === "agent")).toBe(true);
  });

  it("offers the sentence itself, so the row reads as the ask", () => {
    const results = agentResolver.resolve(paletteContext("reply to this next tues"));
    expect(results).toHaveLength(1);
    expect(results[0]?.title).toBe("reply to this next tues");
    expect(results[0]?.meta).toBe("⇥");
  });

  it("stays out of the way of a search", () => {
    // Two words is someone looking for mail, not someone asking a question.
    expect(agentResolver.resolve(paletteContext("tawny"))).toHaveLength(0);
    expect(agentResolver.resolve(paletteContext("data room"))).toHaveLength(0);
    // A question is a question however short.
    expect(agentResolver.resolve(paletteContext("who replied?"))).toHaveLength(1);
  });

  it("leaves `>` command mode alone", () => {
    expect(agentResolver.claims(">archive everything")).toBe(false);
    expect(looksLikeAQuestion("> add an account please")).toBe(false);
  });

  it("ranks below the local layers", () => {
    // The instant, free answer wins the top of the list; the round trip is an
    // offer underneath it.
    const results = resolve(paletteContext("series a data room link"));
    const kinds = results.map((r) => r.kind);
    expect(kinds.indexOf("agent")).toBeGreaterThan(kinds.indexOf("thread"));
  });

  it("records what was typed so ⇥ can hand over the same string", () => {
    agentResolver.resolve(paletteContext("what did tawny send me"));
    expect(paletteQuery()).toBe("what did tawny send me");
  });

  it("running the row asks, rather than opening anything", () => {
    let notified = 0;
    const unsubscribe = subscribeAsk(() => notified++);
    agentResolver.resolve(paletteContext("archive everything from linkedin"))[0]?.run();
    expect(askRequest()?.prompt).toBe("archive everything from linkedin");
    expect(notified).toBe(1);
    unsubscribe();
  });

  it("bumps the nonce so the same sentence twice opens twice", () => {
    requestAsk("do the thing");
    const first = askRequest()?.nonce ?? 0;
    requestAsk("do the thing");
    expect(askRequest()?.nonce).toBe(first + 1);
  });

  it("ignores an empty ask", () => {
    requestAsk("   ");
    expect(askRequest()).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */
/* context extraction                                                          */
/* -------------------------------------------------------------------------- */

describe("what “this” means", () => {
  it("attaches the open conversation", () => {
    const items = contextFor({ ...mailView, threadId: 11, openThread: thread });
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      kind: "thread",
      threadId: 11,
      label: "Series A data room",
    });
  });

  it("falls back to the row under the cursor", () => {
    const items = contextFor({ ...mailView, selectedThread: thread });
    expect(items[0]).toMatchObject({ kind: "thread", threadId: 11 });
  });

  it("attaches the mailbox when nothing is selected", () => {
    const items = contextFor(mailView);
    expect(items[0]).toMatchObject({ kind: "mailbox", label: "Inbox" });
    expect(items[0]?.detail).toContain("INBOX");
  });

  it("attaches a live search", () => {
    const items = contextFor({ ...mailView, search: "from:tawny" });
    expect(items.some((i) => i.kind === "search" && i.label.includes("from:tawny"))).toBe(true);
  });

  it("attaches the event and the day in calendar mode", () => {
    const items = contextFor({
      ...mailView,
      mode: "calendar",
      calendarView: "day",
      eventId: 42,
      selectedEvent: event,
    });
    expect(items[0]).toMatchObject({ kind: "event", eventId: 42, label: "Partner meeting" });
    // The day is always attached, so "am I free on Thursday" has a window.
    const day = items.find((i) => i.kind === "day");
    expect(day?.detail).toMatch(/unix milliseconds \d+ to \d+/);
  });

  it("leaves the rows on screen out unless they are asked for", () => {
    // The agent has search_threads and pays for every line on every turn.
    const items = contextFor({ ...mailView, visibleThreads: [thread] });
    expect(items.some((i) => i.id === "listing")).toBe(false);
  });

  it("attaches the rows on screen when they are", () => {
    const items = contextFor({ ...mailView, visibleThreads: [thread] }, { listing: true });
    const listing = items.find((i) => i.id === "listing");
    expect(listing?.label).toBe("1 conversation in view");
    expect(listing?.detail).toContain("Tawny Chen");
    expect(listing?.detail).toContain("Series A data room");
    expect(listing?.detail).toContain("(2 messages)");
    expect(listing?.detail).toContain("Any chance you can send the link?");
  });

  it("caps a long list and says how many it left off", () => {
    const many = Array.from({ length: 200 }, (_, i) => ({ ...thread, id: i + 1 }));
    const items = contextFor({ ...mailView, visibleThreads: many }, { listing: true });
    const listing = items.find((i) => i.id === "listing");
    expect(listing?.label).toBe("200 conversations in view");
    expect(listing?.detail).toContain("and 140 more further down the list");
    expect(listing?.detail?.split("\n").length).toBeLessThan(70);
  });

  it("attaches the events inside the range on screen, and nothing outside it", () => {
    const elsewhere: CalendarEvent = { ...event, id: 43, title: "Next month", start: 1_760_000_000_000, end: 1_760_003_600_000 };
    const items = contextFor(
      {
        ...mailView,
        mode: "calendar",
        calendarView: "week",
        visibleEvents: [event, elsewhere],
      },
      { listing: true },
    );
    const listing = items.find((i) => i.id === "listing");
    expect(listing?.label).toBe("1 event in view");
    expect(listing?.detail).toContain("Partner meeting");
    expect(listing?.detail).not.toContain("Next month");
  });

  it("carries ids separately from labels, so a stale label cannot mislead", () => {
    const items = contextFor({ ...mailView, threadId: 11, openThread: thread });
    // Rust resolves the id against the store; the label is only what the owner
    // reads on the removable line.
    expect(items[0]?.threadId).toBe(11);
    expect(items[0]?.id).toBe("thread:11");
  });
});

/* -------------------------------------------------------------------------- */
/* the session state machine                                                   */
/* -------------------------------------------------------------------------- */

describe("the session state machine", () => {
  it("adds a session on created and drops it on closed", () => {
    const created: AgentEvent = {
      type: "created",
      sessionId: "agent-1",
      session: session(),
    };
    let sessions = reduceSessions([], created);
    expect(sessions).toHaveLength(1);

    sessions = reduceSessions(sessions, { type: "closed", sessionId: "agent-1" });
    expect(sessions).toHaveLength(0);
  });

  it("ignores events for a session it has never seen", () => {
    const sessions = reduceSessions([], {
      type: "delta",
      sessionId: "ghost",
      text: "hello",
    });
    expect(sessions).toHaveLength(0);
  });

  it("accumulates deltas and lets the completed line replace them", () => {
    let s = session();
    s = reduceSession(s, { type: "delta", sessionId: s.id, text: "Sched" });
    s = reduceSession(s, { type: "delta", sessionId: s.id, text: "uled." });
    expect(s.streaming).toBe("Scheduled.");

    s = reduceSession(s, {
      type: "entry",
      sessionId: s.id,
      entry: { role: "agent", text: "Scheduled." },
    });
    expect(s.streaming).toBeUndefined();
    expect(s.entries[s.entries.length - 1]).toEqual({ role: "agent", text: "Scheduled." });
  });

  it("updates a tool line in place rather than stacking it up", () => {
    let s = session();
    s = reduceSession(s, {
      type: "entry",
      sessionId: s.id,
      entry: { role: "tool", id: "tu-1", name: "search_threads", summary: "Searching…", state: "running" },
    });
    s = reduceSession(s, {
      type: "entry",
      sessionId: s.id,
      entry: { role: "tool", id: "tu-1", name: "search_threads", summary: "7 matches", state: "ok" },
    });

    const tools = s.entries.filter((e) => e.role === "tool");
    expect(tools).toHaveLength(1);
    expect(tools[0]).toMatchObject({ summary: "7 matches", state: "ok" });
  });

  it("running → awaiting approval → running → done", () => {
    let s = session();
    expect(s.status).toBe("running");

    s = reduceSession(s, {
      type: "approval",
      sessionId: s.id,
      pending: {
        toolUseId: "tu-send",
        name: "send_draft",
        summary: "Send “Re: data room” to tawny@example.com on Tue",
        input: { draftId: "draft-1" },
      },
    });
    expect(s.status).toBe("awaitingApproval");
    expect(s.pending?.toolUseId).toBe("tu-send");

    // Answering it — either way — clears the prompt.
    s = reduceSession(s, { type: "status", sessionId: s.id, status: "running" });
    expect(s.pending).toBeUndefined();
    expect(s.status).toBe("running");

    s = reduceSession(s, { type: "status", sessionId: s.id, status: "done" });
    expect(s.status).toBe("done");
  });

  it("records a failure with its reason and clears any prompt", () => {
    let s = session({
      status: "awaitingApproval",
      pending: { toolUseId: "tu-1", name: "send_draft", summary: "Send it", input: {} },
    });
    s = reduceSession(s, {
      type: "failed",
      sessionId: s.id,
      message: "could not reach the Anthropic API: connection refused",
    });
    expect(s.status).toBe("failed");
    expect(s.error).toContain("connection refused");
    expect(s.pending).toBeUndefined();
  });

  it("replaces the attached context when a line is removed", () => {
    let s = session({
      context: [{ id: "thread:11", kind: "thread", label: "Series A data room", threadId: 11 }],
    });
    s = reduceSession(s, { type: "context", sessionId: s.id, context: [] });
    expect(s.context).toEqual([]);
  });

  it("knows which sessions are waiting on a human", () => {
    const waiting = session({ id: "a", status: "awaitingApproval" });
    const busy = session({ id: "b", status: "running" });
    const broken = session({ id: "c", status: "failed" });
    expect(needsAttention([waiting, busy, broken]).map((s) => s.id)).toEqual(["a", "c"]);
  });

  it("never mutates the session it is given", () => {
    const before = session();
    const frozen = JSON.stringify(before);
    reduceSession(before, { type: "delta", sessionId: before.id, text: "x" });
    expect(JSON.stringify(before)).toBe(frozen);
  });
});

/* -------------------------------------------------------------------------- */
/* which brain                                                                 */
/* -------------------------------------------------------------------------- */

describe("the backend status", () => {
  it("is 'nothing detected' outside the desktop app rather than an error", async () => {
    // The settings surface renders this. A browser tab has no agent, which is a
    // state to show, not a failure to handle.
    await expect(loadBackendStatus()).resolves.toEqual(UNKNOWN_BACKEND);
  });

  it("carries the label a session is tagged with", () => {
    const s = session({ backend: "Claude Code" });
    expect(s.backend).toBe("Claude Code");
  });
});

/* -------------------------------------------------------------------------- */
/* what the agent made                                                         */
/* -------------------------------------------------------------------------- */

describe("artifacts", () => {
  it("keeps what a tool made when the line is updated in place", () => {
    // The running line has no artifact — the tool has not finished. The
    // completed line replaces it, and the thing it made has to survive that,
    // because the running line is the one already on screen.
    let s = session();
    s = reduceSession(s, {
      type: "entry",
      sessionId: s.id,
      entry: { role: "tool", id: "tu-1", name: "draft_reply", summary: "Writing a reply…", state: "running" },
    });
    s = reduceSession(s, {
      type: "entry",
      sessionId: s.id,
      entry: {
        role: "tool",
        id: "tu-1",
        name: "draft_reply",
        summary: "Drafted “Re: Bookkeeper”",
        state: "ok",
        artifact: {
          kind: "draft",
          draftId: "draft-19fe",
          threadId: 41774,
          accountId: 1,
          label: "Re: Bookkeeper",
        },
      },
    });

    expect(s.entries).toHaveLength(2);
    const tool = s.entries[1];
    expect(tool.role).toBe("tool");
    if (tool.role !== "tool") throw new Error("unreachable");
    expect(tool.artifact).toEqual({
      kind: "draft",
      draftId: "draft-19fe",
      threadId: 41774,
      accountId: 1,
      label: "Re: Bookkeeper",
    });
  });

  it("names the action after the thing, not after the tool", () => {
    expect(
      artifactAction({ kind: "draft", draftId: "d", accountId: 1, label: "Re: x" }),
    ).toBe("Open draft");
    expect(artifactAction({ kind: "thread", threadId: 2, label: "x" })).toBe(
      "Open conversation",
    );
    expect(artifactAction({ kind: "event", eventId: 3, startMs: 0, label: "x" })).toBe(
      "Show event",
    );
  });

  it("keeps what a read surfaced, which is where the card's fields come from", () => {
    // Reads carry an artifact now. This is the shape `get_event` sends, and
    // every field past the ids is a line the model used to type out in prose.
    let s = session();
    s = reduceSession(s, {
      type: "entry",
      sessionId: s.id,
      entry: {
        role: "tool",
        id: "tu-2",
        name: "get_event",
        summary: "Read “30 min meeting”",
        state: "ok",
        artifact: {
          kind: "event",
          eventId: 91,
          startMs: 1_787_000_000_000,
          endMs: 1_787_001_800_000,
          label: "30 min meeting",
          conferenceUrl: "https://meet.google.com/vht-epjb-pjd",
          guests: ["Kerrie Kuiper"],
          rsvp: "accepted",
        },
      },
    });
    const tool = s.entries[s.entries.length - 1];
    if (tool.role !== "tool" || tool.artifact?.kind !== "event") {
      throw new Error("the read line has to carry its event");
    }
    // The one the calendar has to be scrolled to.
    expect(tool.artifact.startMs).toBe(1_787_000_000_000);
    expect(tool.artifact.conferenceUrl).toBe("https://meet.google.com/vht-epjb-pjd");
  });

  it("states the day of an event rather than ageing it backwards from now", () => {
    // `listTime` would call a meeting nine days out "Yesterday": it ages a
    // stamp towards the past, and every event worth a card is in the future.
    const start = Date.UTC(2026, 7, 20, 17, 0);
    const line = eventWhen(
      { kind: "event", eventId: 1, startMs: start, endMs: start + 1_800_000, label: "x" },
      Date.UTC(2026, 7, 11),
    );
    expect(line).toContain("Thu Aug 20");
    expect(line).not.toContain("Yesterday");

    // A different year says so; an all-day event has no clock to show.
    expect(
      eventWhen({ kind: "event", eventId: 1, startMs: start, label: "x" }, Date.UTC(2025, 0, 1)),
    ).toContain("2026");
    expect(
      eventWhen({ kind: "event", eventId: 1, startMs: start, allDay: true, label: "x" }),
    ).toContain("All day");
  });

  it("counts the guests it did not name", () => {
    expect(guestLine(["Ana", "Bo", "Cy", "Di"], 4)).toBe("Ana, Bo, Cy +1");
    // Rust caps the list; the count is what makes the card honest about it.
    expect(guestLine(["Ana", "Bo", "Cy"], 40)).toBe("Ana, Bo, Cy +37");
    expect(guestLine([])).toBe("");
  });
});
