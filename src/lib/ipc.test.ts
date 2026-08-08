import { describe, expect, it, vi } from "vitest";
import { MachError, type Command, type CommandResult } from "./data";
import {
  createIpcSource,
  errorMessage,
  isTauri,
  mapAccounts,
  mapLabels,
  mapSyncStatus,
  mapThread,
  toMachError,
  type IpcTransport,
} from "./ipc";

/** A transport that answers from a table and records what it was asked. */
function fakeTransport(replies: Record<string, unknown> = {}) {
  const calls: { command: string; args?: Record<string, unknown> }[] = [];
  const listeners: Record<string, (payload: unknown) => void> = {};
  const unlisten = vi.fn();

  const transport: IpcTransport = {
    async invoke(command, args) {
      calls.push({ command, args });
      if (!(command in replies)) throw new Error(`unexpected command ${command}`);
      const reply = replies[command];
      if (reply instanceof Error) throw reply;
      return reply as never;
    },
    async listen(event, handler) {
      listeners[event] = handler as (payload: unknown) => void;
      return unlisten;
    },
    openExternal: vi.fn(async () => {}),
  };

  return { transport, calls, listeners, unlisten };
}

const WIRE_THREAD = {
  id: 41,
  accountId: 2,
  accountEmail: "alex@lumen.example",
  accountColourIndex: 1,
  gmailThreadId: "18f",
  subject: "Checkout conversion dropped 6%",
  snippet: "Started around 02:40 UTC",
  participants: [{ name: "Marcus Oyelaran", email: "marcus@lumen.example" }, { email: "b@x.com" }],
  lastMessageAt: 1_700_000_000_000,
  isUnread: true,
  messageCount: 3,
  hasAttachments: false,
  labelIds: ["INBOX", "STARRED"],
};

describe("thread mapping", () => {
  it("renames the Rust row fields onto the UI shape", () => {
    const thread = mapThread(WIRE_THREAD);
    expect(thread).toMatchObject({
      id: 41,
      accountId: 2,
      timestamp: 1_700_000_000_000,
      unread: true,
      hasAttachment: false,
      messageCount: 3,
    });
  });

  it("derives `starred` from the Gmail label, which is where it actually lives", () => {
    expect(mapThread(WIRE_THREAD).starred).toBe(true);
    expect(mapThread({ ...WIRE_THREAD, labelIds: ["INBOX"] }).starred).toBe(false);
  });

  it("falls back to the address when a participant has no display name", () => {
    expect(mapThread(WIRE_THREAD).participants[1]).toEqual({
      name: "b@x.com",
      email: "b@x.com",
    });
  });

  it("survives the nulls Rust's Option fields serialize to", () => {
    const thread = mapThread({
      id: 1,
      accountId: 1,
      subject: null,
      snippet: null,
      participants: null,
      lastMessageAt: null,
      isUnread: null,
      messageCount: null,
      hasAttachments: null,
      labelIds: null,
    });
    expect(thread.subject).toBe("(no subject)");
    expect(thread.participants).toEqual([]);
    expect(thread.timestamp).toBe(0);
    expect(thread.messageCount).toBe(1);
    expect(thread.labelIds).toEqual([]);
  });
});

describe("account mapping", () => {
  it("shifts a zero-based colour ramp into the 1..5 the tokens define", () => {
    const accounts = mapAccounts([
      { id: 1, email: "a@example.com", colourIndex: 0 },
      { id: 2, email: "b@example.net", colourIndex: 4 },
    ]);
    expect(accounts.map((a) => a.colorIndex)).toEqual([1, 5]);
  });

  it("leaves a one-based ramp alone", () => {
    const accounts = mapAccounts([
      { id: 1, email: "a@x.com", colourIndex: 1 },
      { id: 2, email: "b@y.com", colourIndex: 3 },
    ]);
    expect(accounts.map((a) => a.colorIndex)).toEqual([1, 3]);
  });

  it("classifies consumer domains as personal and everything else as workspace", () => {
    const accounts = mapAccounts([
      { id: 1, email: "alex.rivera@gmail.com", displayName: "Personal", colourIndex: 1 },
      { id: 2, email: "alex@example.com", colourIndex: 2 },
    ]);
    expect(accounts[0]).toMatchObject({ kind: "personal", name: "Personal" });
    expect(accounts[1]).toMatchObject({ kind: "workspace", name: "alex" });
  });
});

describe("label mapping", () => {
  const wire = [
    { id: 1, accountId: 1, gmailLabelId: "INBOX", name: "Inbox", labelType: "system" },
    { id: 2, accountId: 2, gmailLabelId: "INBOX", name: "Inbox", labelType: "system" },
    { id: 3, accountId: 1, gmailLabelId: "Label_12", name: "Investors", labelType: "user" },
  ];

  it("collapses a system label shared by every account into one unified row", () => {
    const labels = mapLabels(wire);
    expect(labels.filter((l) => l.id === "INBOX")).toHaveLength(1);
    expect(labels.find((l) => l.id === "INBOX")?.accountId).toBeNull();
  });

  it("keeps a per-account user label attached to its account", () => {
    expect(mapLabels(wire).find((l) => l.id === "Label_12")).toMatchObject({
      accountId: 1,
      kind: "user",
    });
  });
});

describe("requests", () => {
  it("sends the keyset cursor and a null accountId for the unified stream", async () => {
    const { transport, calls } = fakeTransport({
      list_threads: { items: [WIRE_THREAD], nextCursor: { lastMessageAt: 5, id: 41 } },
    });
    const source = createIpcSource(transport);

    const page = await source.listThreads({
      accountId: null,
      labelId: "INBOX",
      limit: 60,
      after: { lastMessageAt: 9, id: 7 },
    });

    expect(calls[0]).toEqual({
      command: "list_threads",
      args: {
        query: {
          accountId: null,
          labelId: "INBOX",
          unreadOnly: false,
          limit: 60,
          cursor: { lastMessageAt: 9, id: 7 },
        },
      },
    });
    expect(page.threads).toHaveLength(1);
    expect(page.nextCursor).toEqual({ lastMessageAt: 5, id: 41 });
  });

  it("reports the end of the list as a null cursor, never as an absent key", async () => {
    const { transport } = fakeTransport({ list_threads: { items: [] } });
    const page = await createIpcSource(transport).listThreads({});
    expect(page).toEqual({ threads: [], nextCursor: null });
  });

  it("passes millisecond bounds to list_events and drops cancelled instances", async () => {
    const { transport, calls } = fakeTransport({
      list_events: [
        { id: 2, accountId: 1, calendarId: "c", title: "Later", startTs: 200, endTs: 300 },
        { id: 3, accountId: 1, calendarId: "c", title: "Gone", startTs: 50, status: "cancelled" },
        { id: 1, accountId: 1, calendarId: "c", title: "Earlier", startTs: 100, endTs: 150 },
      ],
    });

    const events = await createIpcSource(transport).listEvents({ start: 0, end: 999 });

    expect(calls[0]?.args).toEqual({ startMs: 0, endMs: 999 });
    expect(events.map((e) => e.title)).toEqual(["Earlier", "Later"]);
  });

  it("carries recurringEventId and htmlLink, which the grid has to have", async () => {
    const { transport } = fakeTransport({
      list_events: [
        {
          id: 1,
          accountId: 1,
          calendarId: "c",
          title: "Weekly",
          startTs: 100,
          endTs: 150,
          recurringEventId: "series-abc",
          htmlLink: "https://www.google.com/calendar/event?eid=xyz",
        },
        { id: 2, accountId: 1, calendarId: "c", title: "One-off", startTs: 200, endTs: 250 },
      ],
    });

    const [weekly, oneOff] = await createIpcSource(transport).listEvents({ start: 0, end: 999 });

    expect(weekly.recurringEventId).toBe("series-abc");
    expect(weekly.htmlLink).toBe("https://www.google.com/calendar/event?eid=xyz");
    // Absent on the wire stays absent here, so "not a series" and "the backend
    // did not say" remain two different answers.
    expect(oneOff.recurringEventId).toBeUndefined();
    expect(oneOff.htmlLink).toBeUndefined();
  });

  it("names the thread id argument the way the Tauri command declares it", async () => {
    const { transport, calls } = fakeTransport({
      get_thread: {
        thread: WIRE_THREAD,
        messages: [
          {
            id: 900,
            threadId: 41,
            accountId: 2,
            from: { email: "marcus@lumen.example" },
            internalDate: 1_700_000_000_000,
            bodyText: null,
            snippet: "Started around 02:40 UTC",
            attachments: null,
          },
        ],
      },
    });

    const detail = await createIpcSource(transport).getThread(41);

    expect(calls[0]).toEqual({ command: "get_thread", args: { threadId: 41 } });
    // No plaintext part yet: the snippet is what Gmail itself shows.
    expect(detail?.messages[0]?.bodyText).toBe("Started around 02:40 UTC");
    expect(detail?.messages[0]?.timestamp).toBe(1_700_000_000_000);
  });

  it("returns null for a thread the store does not have", async () => {
    const { transport } = fakeTransport({ get_thread: null });
    expect(await createIpcSource(transport).getThread(1)).toBeNull();
  });
});

describe("commands", () => {
  it("sends the internally tagged command untouched", async () => {
    const { transport, calls } = fakeTransport({
      execute_command: { ok: true, message: "Snoozed 1 conversation", applied: [7], failed: [] },
    });
    const command: Command = { kind: "snooze", threadIds: [7], until: 1_700_000_000_000 };

    await createIpcSource(transport).execute(command);

    expect(calls[0]).toEqual({ command: "execute_command", args: { command } });
  });

  it("keeps applied, failed and the inverse from a partial failure", async () => {
    const wire: CommandResult = {
      ok: false,
      message: "Archived 2 conversations",
      undo: { kind: "unarchive", threadIds: [1, 2] },
      applied: [1, 2],
      failed: [
        {
          ids: [3],
          kind: "rateLimited",
          message: "429 from Gmail",
          retriable: true,
          rolledBack: true,
        },
      ],
    };
    const { transport } = fakeTransport({ execute_command: wire });

    const result = await createIpcSource(transport).execute({
      kind: "archive",
      threadIds: [1, 2, 3],
    });

    expect(result.ok).toBe(false);
    expect(result.applied).toEqual([1, 2]);
    expect(result.failed[0]?.ids).toEqual([3]);
    expect(result.undo).toEqual({ kind: "unarchive", threadIds: [1, 2] });
  });

  it("fills in applied and failed when the backend omits the empty ones", async () => {
    const { transport } = fakeTransport({ execute_command: { ok: true, message: "Nothing to do" } });
    const result = await createIpcSource(transport).execute({ kind: "archive", threadIds: [] });
    expect(result.applied).toEqual([]);
    expect(result.failed).toEqual([]);
  });
});

describe("sync status", () => {
  it("maps the watch-channel snapshot, defaulting an unknown phase to idle", () => {
    const status = mapSyncStatus({
      running: true,
      accounts: [
        {
          accountId: 1,
          email: "alex@example.com",
          phase: "backfill",
          backfillTotal: 61_204,
          backfillDone: 12_480,
          messagesWritten: 12_480,
          eventsWritten: 0,
          lastError: null,
          lastSuccessAt: null,
          updatedAt: 5,
        },
        { accountId: 2, email: "b@x.com", phase: "something-new" },
      ],
      lastPassStartedAt: 1,
      lastPassFinishedAt: null,
      configured: false,
      configurationError: "missing config: MACH_GOOGLE_CLIENT_ID",
      needsReauthorization: ["b@x.com"],
    });

    expect(status.running).toBe(true);
    expect(status.configured).toBe(false);
    expect(status.configurationError).toBe("missing config: MACH_GOOGLE_CLIENT_ID");
    expect(status.needsReauthorization).toEqual(["b@x.com"]);
    expect(status.accounts[0]).toMatchObject({ phase: "backfill", backfillDone: 12_480 });
    expect(status.accounts[1]?.phase).toBe("idle");
    expect(status.lastPassFinishedAt).toBeNull();
  });
});

describe("events", () => {
  it("subscribes to the push channels and maps the payload before handing it on", async () => {
    const { transport, listeners } = fakeTransport();
    const source = createIpcSource(transport);
    const seen: unknown[] = [];

    await source.onSyncStatus((status) => seen.push(status));
    listeners["sync-status"]?.({ running: true, accounts: [] });

    expect(seen).toEqual([
      {
        running: true,
        accounts: [],
        lastPassStartedAt: null,
        lastPassFinishedAt: null,
        configured: true,
        configurationError: null,
        needsReauthorization: [],
      },
    ]);
  });

  it("refetches on threads-changed without reading the payload", async () => {
    const { transport, listeners } = fakeTransport();
    const handler = vi.fn();
    await createIpcSource(transport).onThreadsChanged(handler);
    listeners["threads-changed"]?.(undefined);
    expect(handler).toHaveBeenCalledOnce();
  });

  it("hands back an unsubscribe", async () => {
    const { transport, unlisten } = fakeTransport();
    const off = await createIpcSource(transport).onThreadsChanged(() => {});
    off();
    expect(unlisten).toHaveBeenCalled();
  });
});

describe("errors", () => {
  it("reads Rust's { kind, message } error struct", () => {
    expect(errorMessage({ kind: "unknownThread", message: "no thread with id 4" })).toBe(
      "no thread with id 4",
    );
    expect(errorMessage("plain string from tauri")).toBe("plain string from tauri");
  });

  it("classifies a missing OAuth client as configuration, not as a backend fault", () => {
    const error = toMachError("missing config: MACH_GOOGLE_CLIENT_ID");
    expect(error.kind).toBe("notConfigured");
  });

  it("classifies everything else as a backend error", () => {
    expect(toMachError({ kind: "db", message: "database is locked" }).kind).toBe("backend");
  });

  it("wraps a rejected invoke rather than leaking the raw value", async () => {
    const { transport } = fakeTransport({ list_accounts: new Error("ipc is down") });
    await expect(createIpcSource(transport).listAccounts()).rejects.toBeInstanceOf(MachError);
  });
});

describe("runtime detection", () => {
  it("is false with no window, and true once Tauri injects its globals", () => {
    expect(isTauri()).toBe(false);
    vi.stubGlobal("window", { __TAURI_INTERNALS__: {} });
    expect(isTauri()).toBe(true);
    vi.unstubAllGlobals();
  });
});
