import { describe, expect, it, vi } from "vitest";
import wireSample from "./wire-sample.json";
import { MachError, type Command, type CommandResult, type WakeFailure } from "./data";
import {
  createIpcSource,
  errorMessage,
  isTauri,
  mapAccounts,
  mapCalendars,
  mapEvent,
  mapLabels,
  mapMessage,
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

  it("names the address a sign-in is repairing, and sends null when adding", async () => {
    const { transport, calls } = fakeTransport({
      begin_add_account: { url: "https://accounts.google.com/o/oauth2/v2/auth", pendingId: "p1" },
    });
    const source = createIpcSource(transport);

    await source.beginAddAccount("bruno.bornsztein@gmail.com");
    // Rust turns this into Google's `login_hint` and holds it against whatever
    // identity comes back, so "Sign in again" cannot connect a second account.
    expect(calls[0]).toEqual({
      command: "begin_add_account",
      args: { email: "bruno.bornsztein@gmail.com" },
    });

    await source.beginAddAccount();
    expect(calls[1]?.args).toEqual({ email: null });
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

  it("carries the round-trip fields migration 5 added", async () => {
    // This mapper builds a fresh object literal, so a field nobody names is
    // dropped in silence — no type error, because `WireEvent` is our own
    // description of the wire rather than something generated from it. It has
    // now happened twice: once to `recurringEventId`/`htmlLink`, and once to
    // everything below, which was stored, synced and rendered but never
    // arrived. This test is the tripwire.
    const { transport } = fakeTransport({
      list_events: [
        {
          id: 1,
          accountId: 1,
          calendarId: "c",
          title: "Weekly",
          startTs: 100,
          endTs: 150,
          recurrence: ["RRULE:FREQ=WEEKLY"],
          reminders: { useDefault: false, overrides: [{ method: "popup", minutes: 10 }] },
          iCalUID: "uid-abc@google.com",
          organizerSelf: true,
          guestsCanModify: false,
        },
      ],
    });

    const [event] = await createIpcSource(transport).listEvents({ start: 0, end: 999 });

    expect(event.recurrence).toEqual(["RRULE:FREQ=WEEKLY"]);
    expect(event.reminders).toEqual({
      useDefault: false,
      overrides: [{ method: "popup", minutes: 10 }],
    });
    expect(event.iCalUID).toBe("uid-abc@google.com");
    expect(event.organizerSelf).toBe(true);
    expect(event.guestsCanModify).toBe(false);
  });

  it("keeps 'Google did not say' distinct from 'no'", async () => {
    // Collapsing an absent `organizerSelf` to `false` would make every row
    // written before migration 5 read-only until it next synced.
    const { transport } = fakeTransport({
      list_events: [
        { id: 1, accountId: 1, calendarId: "c", title: "Old row", startTs: 100, endTs: 150 },
      ],
    });

    const [event] = await createIpcSource(transport).listEvents({ start: 0, end: 999 });

    expect(event.organizerSelf).toBeUndefined();
    expect(event.guestsCanModify).toBeUndefined();
    expect(event.recurrence).toBeUndefined();
    expect(event.reminders).toBeUndefined();
    expect(event.iCalUID).toBeUndefined();
  });

  it("carries the conference, the guests and the creator migration 7 added", async () => {
    // Third time on this mapper, same trap: a fresh object literal, a wire type
    // we wrote by hand, and no type error for a field nobody names. Everything
    // asserted here is stored in SQLite, filled in by sync, covered by a Rust
    // test, and rendered by the modal — all of which is worth nothing if it is
    // dropped between Rust and React, which is exactly what happened to
    // `recurringEventId`, then to all of migration 5, then to migration 6.
    const { transport } = fakeTransport({
      list_events: [
        {
          id: 1,
          accountId: 1,
          calendarId: "c",
          title: "Team standup",
          startTs: 100,
          endTs: 150,
          creator: { email: "ops@offerlab.com", name: "Ops Bot" },
          guests: [
            { email: "dana@offerlab.com", name: "Dana", response: "declined",
              comment: "Declined because I am out of office" },
            { email: "sean@offerlab.com", response: "tentative", organizer: true, optional: true },
            { email: "me@example.com", response: "accepted", isSelf: true },
          ],
          conference: {
            id: "abc-defg-hij",
            name: "Google Meet",
            entryPoints: [
              { kind: "video", uri: "https://meet.google.com/abc-defg-hij",
                label: "meet.google.com/abc-defg-hij" },
              { kind: "phone", uri: "tel:+1-513-555-0199", label: "+1 513-555-0199",
                pin: "396011834", regionCode: "US" },
            ],
          },
          attachments: [{ title: "Sprint notes", url: "https://drive.google.com/open?id=1AbC" }],
          visibility: "private",
          transparency: "transparent",
        },
      ],
    });

    const [event] = await createIpcSource(transport).listEvents({ start: 0, end: 999 });

    expect(event.creator).toEqual({ name: "Ops Bot", email: "ops@offerlab.com" });
    expect(event.guests?.[0]).toMatchObject({
      email: "dana@offerlab.com",
      response: "declined",
      comment: "Declined because I am out of office",
    });
    expect(event.guests?.[1]).toMatchObject({ organizer: true, optional: true });
    expect(event.guests?.[2]?.isSelf).toBe(true);
    expect(event.conference?.id).toBe("abc-defg-hij");
    expect(event.conference?.entryPoints).toHaveLength(2);
    expect(event.conference?.entryPoints[1]).toMatchObject({
      kind: "phone",
      uri: "tel:+1-513-555-0199",
      pin: "396011834",
      regionCode: "US",
    });
    expect(event.attachments?.[0]?.url).toBe("https://drive.google.com/open?id=1AbC");
    expect(event.visibility).toBe("private");
    expect(event.transparency).toBe("transparent");
  });

  it("drops a conference entry point with nowhere to go, and an empty conference with it", async () => {
    // An entry point with no uri renders as an empty line and dials nothing, and
    // a conference block with no way in is a heading over a blank.
    const { transport } = fakeTransport({
      list_events: [
        {
          id: 1,
          accountId: 1,
          calendarId: "c",
          title: "Sync",
          startTs: 100,
          endTs: 150,
          conference: { name: "Google Meet", entryPoints: [{ kind: "sip" }] },
        },
      ],
    });

    const [event] = await createIpcSource(transport).listEvents({ start: 0, end: 999 });
    expect(event.conference).toBeUndefined();
  });

  it("keeps “no answer” distinct from “has not answered” on a guest", async () => {
    // A row written before migration 7 has addresses and no answers. Collapsing
    // that to `needsAction` would put guests into the "awaiting" count that
    // nobody has ever been asked about.
    const { transport } = fakeTransport({
      list_events: [
        {
          id: 1,
          accountId: 1,
          calendarId: "c",
          title: "Old row",
          startTs: 100,
          endTs: 150,
          guests: [{ email: "ada@example.com" }, { email: "b@x.com", response: "somethingNew" }],
        },
      ],
    });

    const [event] = await createIpcSource(transport).listEvents({ start: 0, end: 999 });
    expect(event.guests?.[0]?.response).toBeUndefined();
    expect(event.guests?.[0]?.optional).toBe(false);
    // A response Google has never sent is not passed through as if it were one.
    expect(event.guests?.[1]?.response).toBeUndefined();
    expect(event.creator).toBeUndefined();
    expect(event.conference).toBeUndefined();
    expect(event.attachments).toBeUndefined();
    expect(event.visibility).toBeUndefined();
  });

  it("carries every field migration 6 added to a calendar", async () => {
    // The same tripwire as the two above, one table later. `mapCalendars` also
    // builds a fresh object literal, so a column that is stored, synced and
    // tested in Rust is dropped in silence here unless it is named — and
    // `WireCalendar` is our own description of the wire, so nothing type-checks
    // the omission. Every field this asserts is one the sidebar or the edit
    // guard actually reads.
    const { transport } = fakeTransport({
      list_accounts: [{ id: 1, email: "bruno@example.com", colourIndex: 1 }],
      list_calendars: [
        {
          id: "c_d814cb@group.calendar.google.com",
          accountId: 1,
          accountEmail: "bruno@example.com",
          name: "Alicia & Bruno",
          colourIndex: 1,
          eventCount: 12,
          description: "Ours",
          backgroundColor: "#f83a22",
          foregroundColor: "#ffffff",
          accessRole: "writer",
          timeZone: "America/Chicago",
          primary: false,
          selected: true,
          deleted: false,
        },
      ],
    });

    const [calendar] = await createIpcSource(transport).listCalendars();

    expect(calendar.name).toBe("Alicia & Bruno");
    expect(calendar.description).toBe("Ours");
    expect(calendar.backgroundColor).toBe("#f83a22");
    expect(calendar.foregroundColor).toBe("#ffffff");
    expect(calendar.accessRole).toBe("writer");
    expect(calendar.timeZone).toBe("America/Chicago");
    expect(calendar.primary).toBe(false);
    expect(calendar.selected).toBe(true);
    expect(calendar.deleted).toBe(false);
  });

  it("reads a calendar the backend knows nothing about as permissive, not denied", async () => {
    // Every calendar looks like this on the first launch after migration 6:
    // events synced, metadata not yet fetched. Defaulting `accessRole` to
    // `reader` here would make the whole calendar read-only until the next
    // sweep, and defaulting `selected` to false would show an empty grid.
    const { transport } = fakeTransport({
      list_accounts: [{ id: 1, email: "bruno@example.com", colourIndex: 1 }],
      list_calendars: [
        {
          id: "team@group.calendar.google.com",
          accountId: 1,
          accountEmail: "bruno@example.com",
          name: "team@group.calendar.google.com",
          colourIndex: 1,
          eventCount: 3,
        },
      ],
    });

    const [calendar] = await createIpcSource(transport).listCalendars();

    expect(calendar.accessRole).toBeUndefined();
    expect(calendar.backgroundColor).toBeUndefined();
    expect(calendar.description).toBeUndefined();
    expect(calendar.timeZone).toBeUndefined();
    expect(calendar.selected).toBe(true);
    expect(calendar.deleted).toBe(false);
    expect(calendar.primary).toBe(false);
  });

  it("keeps a calendar Google says is hidden, and says it is hidden", async () => {
    const { transport } = fakeTransport({
      list_accounts: [{ id: 1, email: "bruno@example.com", colourIndex: 1 }],
      list_calendars: [
        {
          id: "muted@group.calendar.google.com",
          accountId: 1,
          accountEmail: "bruno@example.com",
          name: "Muted",
          colourIndex: 1,
          eventCount: 0,
          selected: false,
          deleted: true,
        },
      ],
    });

    const [calendar] = await createIpcSource(transport).listCalendars();

    expect(calendar.selected).toBe(false);
    expect(calendar.deleted).toBe(true);
  });

  it("drops an access role it does not recognise rather than trusting it", async () => {
    // An unknown role must not become a denial by accident: `canEditEvent` only
    // withholds the editor on a positively read-only role, so `undefined` is the
    // permissive answer and a passed-through mystery string would not be.
    const { transport } = fakeTransport({
      list_accounts: [{ id: 1, email: "bruno@example.com", colourIndex: 1 }],
      list_calendars: [
        {
          id: "c",
          accountId: 1,
          accountEmail: "bruno@example.com",
          name: "Odd",
          colourIndex: 1,
          eventCount: 0,
          accessRole: "somethingNew",
        },
      ],
    });

    const [calendar] = await createIpcSource(transport).listCalendars();
    expect(calendar.accessRole).toBeUndefined();
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

  it("carries `isDraft` across, so an unsent message is not drawn as a sent one", async () => {
    // The fourth time this mapper has eaten a field, and the first with a
    // safety consequence rather than a cosmetic one. `mapMessage` builds a
    // fresh object literal and `WireMessage` is our own hand-written
    // description of the wire, so a field nobody names is dropped with no type
    // error — see the three tripwires at the bottom of `describe("events")`.
    //
    // Rust has serialized `isDraft` on every message since `compose::mirror`
    // shipped. It arrived here as `undefined`, so a draft rendered in its
    // thread exactly like a reply that had already gone out, while the agent
    // told the owner the same thread carried a DRAFT label.
    const { transport } = fakeTransport({
      get_thread: {
        thread: WIRE_THREAD,
        messages: [
          {
            id: 900,
            threadId: 41,
            accountId: 2,
            from: { email: "marcus@lumen.example" },
            internalDate: 1_700_000_000_000,
            snippet: "Started around 02:40 UTC",
          },
          {
            id: 901,
            threadId: 41,
            accountId: 2,
            from: { email: "alex@lumen.example" },
            internalDate: 1_700_000_100_000,
            bodyText: "Looking now — will have numbers by",
            isDraft: true,
          },
        ],
      },
    });

    const detail = await createIpcSource(transport).getThread(41);

    expect(detail?.messages[0]?.isDraft).toBe(false);
    expect(detail?.messages[1]?.isDraft).toBe(true);
  });

  it("treats a message the wire said nothing about as sent, not as a draft", async () => {
    // The permissive direction is the other way round here than it is on a
    // calendar: "the seam did not say" must not mark somebody's sent mail
    // unsent, which would be a false alarm on every row of every thread.
    const { transport } = fakeTransport({
      get_thread: {
        thread: WIRE_THREAD,
        messages: [
          { id: 900, threadId: 41, accountId: 2, snippet: "hi", isDraft: null },
          { id: 901, threadId: 41, accountId: 2, snippet: "hi" },
        ],
      },
    });

    const detail = await createIpcSource(transport).getThread(41);
    expect(detail?.messages.map((m) => m.isDraft)).toEqual([false, false]);
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

describe("filters", () => {
  const WIRE_FILTER = {
    accountId: 2,
    accountEmail: "alex@lumen.example",
    id: "ANe1Bmh",
    criteria: { from: "no-reply@okta.com" },
    action: { removeLabelIds: ["INBOX"] },
    description: "Mail from no-reply@okta.com. It skips the inbox.",
  };

  it("reads every account's filters when none is named", async () => {
    const { transport, calls } = fakeTransport({ list_filters: [WIRE_FILTER] });
    const filters = await createIpcSource(transport).listFilters();

    expect(calls[0]).toEqual({ command: "list_filters", args: {} });
    // The sentence is Rust's and is carried through rather than rebuilt, so
    // Preferences and an approval prompt cannot describe the same rule
    // differently.
    expect(filters[0]?.description).toBe("Mail from no-reply@okta.com. It skips the inbox.");
    expect(filters[0]?.accountEmail).toBe("alex@lumen.example");
  });

  it("sends the criteria and the action as Gmail's two halves", async () => {
    const { transport, calls } = fakeTransport({ create_filter: WIRE_FILTER });
    await createIpcSource(transport).createFilter(
      2,
      { from: "no-reply@okta.com" },
      { addLabelIds: [], removeLabelIds: ["INBOX"] },
    );

    expect(calls[0]).toEqual({
      command: "create_filter",
      args: {
        accountId: 2,
        criteria: { from: "no-reply@okta.com" },
        action: { addLabelIds: [], removeLabelIds: ["INBOX"] },
      },
    });
  });

  it("deletes by the id Google assigned", async () => {
    const { transport, calls } = fakeTransport({ delete_filter: null });
    await createIpcSource(transport).deleteFilter(2, "ANe1Bmh");
    expect(calls[0]).toEqual({
      command: "delete_filter",
      args: { accountId: 2, filterId: "ANe1Bmh" },
    });
  });

  it("does not invent a description a backend did not send", async () => {
    const { transport } = fakeTransport({ list_filters: [{ id: "x" }] });
    const filters = await createIpcSource(transport).listFilters();
    expect(filters[0]).toEqual({
      accountId: 0,
      accountEmail: "",
      id: "x",
      criteria: {},
      action: {},
      description: "",
    });
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
        missingScope: [],
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

  /*
   * The wake sweep has no gesture behind it, so a refusal has no status line of
   * its own to land on. This is the only way it reaches the window.
   */
  it("carries a refused wake through, ids and reason intact", async () => {
    const { transport, listeners } = fakeTransport();
    const seen: unknown[] = [];

    await createIpcSource(transport).onWakeFailed((failure) => seen.push(failure));
    listeners["wake-failed"]?.({
      threadIds: [7, 9],
      message: "google: 503 backend error",
      retriable: true,
    });

    expect(seen).toEqual([
      { threadIds: [7, 9], message: "google: 503 backend error", retriable: true },
    ]);
  });

  it("does not let a wake failure arrive as an empty message", async () => {
    const { transport, listeners } = fakeTransport();
    const seen: WakeFailure[] = [];

    await createIpcSource(transport).onWakeFailed((failure) => seen.push(failure));
    listeners["wake-failed"]?.({});

    expect(seen[0]?.message).not.toBe("");
    expect(seen[0]?.threadIds).toEqual([]);
    expect(seen[0]?.retriable).toBe(false);
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

/* -------------------------------------------------------------------------- */
/* Nothing Rust sends may vanish on the way in                                 */
/* -------------------------------------------------------------------------- */

/**
 * The mappers below build fresh object literals, so a field Rust sends is
 * dropped in silence unless somebody names it — and five times now, nobody
 * did: `recurringEventId`, `htmlLink`, migration 5, migration 7, `isDraft`.
 * The last of those had the agent and the UI stating different things about the
 * same message in front of the owner.
 *
 * Every one of those was fixed with a test for that one field, which is a
 * tripwire for a field somebody already knows about. The tests further up this
 * file are those tripwires and they stay — they are what each incident cost.
 * This is the general case.
 *
 * The mechanism is a real payload rather than a type. `WireEvent` and its
 * siblings are our hand-written *description* of the wire, and in all five
 * incidents the description had drifted from Rust exactly as badly as the
 * mapper had, so `keyof WireEvent` did not contain the missing key either and
 * no amount of TypeScript could have noticed. `src/lib/wire-sample.json` is
 * generated by `the_wire_sample_is_what_the_frontend_will_receive` in
 * `src-tauri/tests/ipc.rs` from struct literals that name every field, so the
 * Rust compiler refuses a new column that is not in it.
 *
 * What that leaves this test doing: run the mapper against the sample and watch
 * which keys it actually reads. A key the mapper never touched is either a
 * field the UI has just started dropping, or a decision — and a decision has to
 * be written down in `NOT_READ` before this will pass.
 */

/**
 * `value`, wrapped so that every property read is recorded.
 *
 * Reading is the honest test of consumption. A manifest of "fields we map"
 * would be one more list to forget to add to, which is the failure being fixed
 * rather than a fix for it.
 */
function watched<T>(value: T, seen: Set<string>, path: string): T {
  if (Array.isArray(value)) {
    return value.map((item) => watched(item, seen, path)) as T;
  }
  if (value === null || typeof value !== "object") return value;
  return new Proxy(value as object, {
    get(target, key, receiver) {
      const read = Reflect.get(target, key, receiver);
      if (typeof key !== "string") return read;
      const child = path ? `${path}.${key}` : key;
      seen.add(child);
      return watched(read, seen, child);
    },
  }) as T;
}

/** Every leaf path in the payload, arrays flattened onto their element. */
function paths(value: unknown, path = ""): string[] {
  if (Array.isArray(value)) return value.flatMap((item) => paths(item, path));
  if (value === null || typeof value !== "object") return path ? [path] : [];
  return Object.entries(value).flatMap(([key, child]) =>
    paths(child, path ? `${path}.${key}` : key),
  );
}

/** Whether a declared field is this path, or the thing this path is inside. */
function covers(declared: string, path: string): boolean {
  return path === declared || path.startsWith(`${declared}.`);
}

/**
 * Fields Rust sends that the UI has decided it does not want.
 *
 * Each of these is a choice, and the point of the list is that it is a choice
 * somebody made rather than a field that slipped through. Adding to it should
 * feel like a small commitment, because it is one.
 */
const NOT_READ: Record<string, string[]> = {
  // The store's own identities and the denormalized account columns the rail
  // gets from `mapAccounts` instead. `gmailThreadId` is the id Google knows a
  // thread by; every screen here addresses threads by the local row id.
  thread: ["gmailThreadId", "accountEmail", "accountColourIndex"],
  // Headers the composer needs and the reader does not: `mapMessage` produces
  // what a rendered message is made of, and `src-tauri/src/compose` builds a
  // reply from the row in Rust rather than from anything the UI hands back.
  // `subject` lives on the thread, which is the only place it is displayed.
  message: [
    "gmailMessageId",
    "rfc822MessageId",
    "inReplyTo",
    "references",
    "replyTo",
    "bcc",
    "subject",
    "isUnread",
    // Whether the sender declared `format=flowed` on the plain-text part. The
    // decision it drives is made in `render::render_text_with`, which has
    // already rejoined the soft breaks by the time the frontend sees anything;
    // what arrives here is HTML, and there is nothing left for the UI to do
    // with the flag.
    "bodyTextFlowed",
    "bodyTextDelsp",
    // Whether this row's `body_html` was evicted. A real fact about the row,
    // and the reading pane learns it from `render_message_body` rather than
    // from here: that answer is per render and this one is as old as the last
    // `get_thread`, so a message whose body has just been re-fetched would
    // still be carrying `true` on the thread payload. One source, and it is the
    // render.
    "htmlEvicted",
    // Google's id for the blob and the path it lands at once fetched, both of
    // which belong to `lib/attachments.ts` and the Rust side that downloads it.
    "attachments.gmailAttachmentId",
    "attachments.localPath",
  ],
  // `googleEventId` addresses the event in Google; writes go through the local
  // row id and Rust resolves it. `updatedAt` is sync bookkeeping. `status` is
  // decided before it ever gets here: `sync/calendar.rs` deletes a cancelled
  // event rather than storing it, so the only rows on this wire are live ones.
  event: ["googleEventId", "updatedAt", "status"],
  // `eventCount` is how `list_calendars` proves a calendar with no metadata is
  // still real; the sidebar counts what it is showing. `colorId` is Google's
  // palette index, which `backgroundColor` already gives us as a colour.
  calendar: ["accountEmail", "eventCount", "colorId"],
};

describe("the wire sample", () => {
  const cases: { name: string; run: (wire: never) => unknown }[] = [
    { name: "thread", run: (wire) => mapThread(wire) },
    { name: "message", run: (wire) => mapMessage(wire) },
    { name: "event", run: (wire) => mapEvent(wire) },
    { name: "calendar", run: (wire) => mapCalendars([wire], [])[0] },
  ];

  for (const { name, run } of cases) {
    it(`is fully consumed by map${name[0]?.toUpperCase()}${name.slice(1)}`, () => {
      const payload = (wireSample as Record<string, unknown>)[name];
      const seen = new Set<string>();
      run(watched(payload, seen, "") as never);

      const dropped = paths(payload)
        .filter((key) => !seen.has(key))
        // A declared field covers what is inside it: `bcc` is a list of people
        // and saying "the reader does not want the bcc line" is one decision,
        // not one per participant field.
        .filter((key) => !(NOT_READ[name] ?? []).some((ignored) => covers(ignored, key)));

      expect(dropped, `${name}: Rust sends these and the mapper never reads them`).toEqual([]);
    });
  }

  it("keeps its own list of exceptions honest", () => {
    /*
     * An exception outlives the field it was written for. Once Rust stops
     * sending `bcc`, the line saying the reader does not want it is no longer a
     * decision about anything — and the next person to read the list has to
     * work out which of its entries still mean something. So the list is
     * checked against the payload too.
     */
    for (const [name, ignored] of Object.entries(NOT_READ)) {
      const sent = paths((wireSample as Record<string, unknown>)[name]);
      for (const key of ignored) {
        expect(
          sent.some((path) => covers(key, path)),
          `${name}.${key} is not on the wire any more`,
        ).toBe(true);
      }
    }
  });
});
