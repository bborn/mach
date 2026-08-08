/**
 * The data-access seam.
 *
 * Everything the UI knows about mail and calendar arrives through
 * `MachDataSource`, and every mutation leaves through `execute(command)`.
 * There are two implementations: `tauriSource` in `./ipc.ts`, which is what the
 * app runs on, and `fixtureSource` below, which is what a plain `bun run dev`
 * browser tab renders when there is no Tauri runtime to talk to. `main.tsx`
 * picks one; nothing else in the app knows which is live.
 *
 * The `Command` union is the same vocabulary as the Rust `Command` enum in
 * `src-tauri/src/commands/types.rs` — same `kind` strings, same field names,
 * because those commands are also the agent's tools. Rust is authoritative:
 * when the two disagree, this file is wrong.
 */

import type {
  Account,
  AccountId,
  Calendar,
  CalendarEvent,
  CalendarId,
  EventId,
  Label,
  LabelId,
  Participant,
  Rsvp,
  SyncStatus,
  Thread,
  ThreadDetail,
  ThreadId,
  ThreadPage,
  ThreadQuery,
  TimeRange,
  PendingAuthorization,
} from "@/types";
import * as fixtures from "./fixtures";

/* -------------------------------------------------------------------------- */
/* Commands — the write half of the seam                                       */
/* -------------------------------------------------------------------------- */

/**
 * A thread's label set and unread flag at a point in time.
 *
 * This is what makes undo exact: restoring a thread means putting *these*
 * labels back, not adding INBOX and hoping. The UI never builds one — the
 * command layer mints them and hands them back inside `undo`.
 */
export interface ThreadLabelState {
  threadId: ThreadId;
  labelIds: LabelId[];
  isUnread: boolean;
}

/**
 * Which occurrences of a recurring series an edit addresses.
 *
 * Rows are concrete occurrences — Google expands series with
 * `singleEvents=true` — so an edit has to say which ones it means. `this`
 * addresses the occurrence's own id; `all` addresses the series master.
 *
 * **There is deliberately no `thisAndFollowing`.** Google has no endpoint for
 * it, and faking it means rewriting the master's RRULE and inserting a second
 * series — three calls whose partial failure leaves a split series behind. The
 * modal says so out loud rather than quietly doing one of the other two.
 */
export type EventScope = "this" | "all";

/** Everything needed to bring an event into being. Times are epoch millis. */
export interface EventDraft {
  title: string;
  description?: string;
  location?: string;
  startTs: number;
  endTs: number;
  isAllDay: boolean;
  attendees: Participant[];
  /** RRULE lines, verbatim: `["RRULE:FREQ=WEEKLY;BYDAY=TU"]`. */
  recurrence: string[];
  /** Popup reminder offsets in minutes. Omit to keep Google's defaults. */
  reminderMinutes?: number[];
}

/**
 * A partial edit: only the named fields change.
 *
 * An empty string clears a text field. A time change must always name
 * `startTs`, `endTs` and `isAllDay` together — switching between timed and
 * all-day changes the shape of both ends, and Google rejects a half-converted
 * pair.
 */
export interface EventPatch {
  title?: string;
  description?: string;
  location?: string;
  startTs?: number;
  endTs?: number;
  isAllDay?: boolean;
  attendees?: Participant[];
  recurrence?: string[];
  reminderMinutes?: number[];
}

export type Command =
  | { kind: "archive"; threadIds: ThreadId[] }
  | { kind: "unarchive"; threadIds: ThreadId[]; restore?: ThreadLabelState[] }
  | { kind: "markRead"; threadIds: ThreadId[]; read: boolean }
  | { kind: "star"; threadIds: ThreadId[]; starred: boolean }
  | { kind: "label"; threadIds: ThreadId[]; labelId: LabelId; add: boolean }
  | { kind: "trash"; threadIds: ThreadId[] }
  | { kind: "untrash"; threadIds: ThreadId[]; restore?: ThreadLabelState[] }
  | { kind: "snooze"; threadIds: ThreadId[]; until: number }
  | { kind: "unsnooze"; threadIds: ThreadId[] }
  | { kind: "rsvp"; eventId: EventId; response: Rsvp }
  | {
      kind: "createEvent";
      accountId: AccountId;
      calendarId: CalendarId;
      draft: EventDraft;
    }
  | { kind: "updateEvent"; eventId: EventId; patch: EventPatch; scope?: EventScope }
  | { kind: "deleteEvent"; eventId: EventId; scope?: EventScope }
  | {
      kind: "moveEvent";
      eventId: EventId;
      accountId: AccountId;
      calendarId: CalendarId;
    };

export type CommandKind = Command["kind"];

/** The calendar half of the vocabulary. */
export const CALENDAR_COMMAND_KINDS = [
  "rsvp",
  "createEvent",
  "updateEvent",
  "deleteEvent",
  "moveEvent",
] as const;

export type CalendarCommandKind = (typeof CALENDAR_COMMAND_KINDS)[number];

export type CalendarCommand = Extract<Command, { kind: CalendarCommandKind }>;

/** The commands that take a list of threads — i.e. everything but the calendar. */
export type MailCommand = Exclude<Command, { kind: CalendarCommandKind }>;

const CALENDAR_KINDS = new Set<string>(CALENDAR_COMMAND_KINDS);

export function isMailCommand(command: Command): command is MailCommand {
  return !CALENDAR_KINDS.has(command.kind);
}

/**
 * The ids a command addresses, whichever half of the vocabulary it is from.
 *
 * A create has none: the row it makes does not exist until it has run, and
 * `CommandResult.applied` is where the new id comes back.
 */
export function targetIds(command: Command): number[] {
  if (isMailCommand(command)) return command.threadIds;
  return command.kind === "createEvent" ? [] : [command.eventId];
}

/** Why a remote call failed, in the categories a caller can act on. */
export type FailureKind =
  | "auth"
  | "rateLimited"
  | "forbidden"
  | "notFound"
  | "server"
  | "network"
  | "invalid"
  | "unexpected";

/** One remote failure, naming exactly the ids it covers. */
export interface CommandFailure {
  ids: number[];
  kind: FailureKind;
  message: string;
  /** Whether dispatching the same command again could plausibly succeed. */
  retriable: boolean;
  /** Whether the local store was reverted for these ids. Today: always. */
  rolledBack: boolean;
}

export interface CommandResult {
  /** True when nothing failed. A no-op is still `ok`. */
  ok: boolean;
  /** Human sentence for the status bar: "Archived 1 conversation". */
  message: string;
  /** The command that reverses this one, narrowed to the ids that changed. */
  undo?: Command;
  /** Ids whose local state now reflects the command. */
  applied: number[];
  /** Ids that were rolled back, grouped by the failure that hit them. */
  failed: CommandFailure[];
}

/** Every id named by a failure, deduplicated. */
export function failedIds(result: CommandResult): number[] {
  return [...new Set(result.failed.flatMap((f) => f.ids))];
}

/**
 * A short, honest sentence for a result that did not entirely succeed.
 *
 * `execute_command` can return `ok: false` with some ids applied and the rest
 * rolled back, and "Archived 3 conversations" would then be a lie. This says
 * what actually happened, and names the reason rather than the count alone.
 */
export function describeResult(result: CommandResult): string {
  if (result.ok || result.failed.length === 0) return result.message;

  const failed = failedIds(result).length;
  const reason = result.failed[0]?.message ?? FAILURE_LABELS[result.failed[0]?.kind ?? "unexpected"];
  const applied = result.applied.length
    ? `${result.message} · `
    : "";
  return `${applied}${failed} failed — ${reason}`;
}

export const FAILURE_LABELS: Record<FailureKind, string> = {
  auth: "the account needs re-authorizing",
  rateLimited: "Google is rate limiting",
  forbidden: "Google refused",
  notFound: "already gone on Google's side",
  server: "Google had an error",
  network: "no answer from Google",
  invalid: "the command could not be sent",
  unexpected: "unexpected response",
};

/**
 * The inverse of a command, for sources that have to compute it themselves.
 *
 * The real command layer returns a better `undo` than this — one narrowed to
 * the ids that actually changed, carrying the exact prior label sets. This is
 * the shape-only version the fixture source uses, and the reference for which
 * command reverses which:
 *
 *   archive ⇄ unarchive · trash ⇄ untrash · snooze → unsnooze
 *
 * Snooze's inverse is `unsnooze`, not `unarchive`: waking a thread restores the
 * labels it was snoozed from, which "add INBOX" would not do.
 */
export function inverseOf(command: Command): Command | undefined {
  switch (command.kind) {
    case "archive":
      return { kind: "unarchive", threadIds: command.threadIds };
    case "unarchive":
      return { kind: "archive", threadIds: command.threadIds };
    case "trash":
      return { kind: "untrash", threadIds: command.threadIds };
    case "untrash":
      return { kind: "trash", threadIds: command.threadIds };
    case "snooze":
      return { kind: "unsnooze", threadIds: command.threadIds };
    case "markRead":
      return { ...command, read: !command.read };
    case "star":
      return { ...command, starred: !command.starred };
    case "label":
      return { ...command, add: !command.add };
    // These need state only the command layer holds: the prior labels, the
    // prior RSVP, the row id a create is about to mint, the calendar an event
    // came from. Nothing local can honestly claim an inverse.
    case "unsnooze":
    case "rsvp":
    case "createEvent":
    case "updateEvent":
    case "deleteEvent":
    case "moveEvent":
      return undefined;
  }
}

/* -------------------------------------------------------------------------- */
/* The command catalogue — the same data the agent will enumerate              */
/* -------------------------------------------------------------------------- */

export type ParamType =
  | "threadIds"
  | "eventId"
  | "bool"
  | "timestamp"
  | "labelId"
  | "rsvpResponse"
  | "threadLabelStates"
  | "accountId"
  | "calendarId"
  | "eventDraft"
  | "eventPatch"
  | "eventScope";

export interface ParamSpec {
  name: string;
  type: ParamType;
  required: boolean;
  description: string;
}

export interface CommandSpec {
  kind: string;
  summary: string;
  params: ParamSpec[];
  undoable: boolean;
  batch: boolean;
}

/* -------------------------------------------------------------------------- */
/* Errors                                                                      */
/* -------------------------------------------------------------------------- */

export type MachErrorKind =
  /** No Google OAuth client is configured, so nothing can be authorized yet. */
  | "notConfigured"
  /** There is no backend to talk to — a browser tab, or IPC is not up. */
  | "unavailable"
  /** The backend answered, and the answer was an error. */
  | "backend";

export class MachError extends Error {
  readonly kind: MachErrorKind;
  readonly cause?: unknown;

  constructor(kind: MachErrorKind, message: string, cause?: unknown) {
    super(message);
    this.name = "MachError";
    this.kind = kind;
    this.cause = cause;
  }
}

/* -------------------------------------------------------------------------- */
/* The interface the IPC layer must satisfy                                    */
/* -------------------------------------------------------------------------- */

export type Unsubscribe = () => void;

export interface MachDataSource {
  /** Which implementation this is. The UI uses it to say so, honestly. */
  readonly kind: "tauri" | "fixture";

  listAccounts(): Promise<Account[]>;
  listLabels(accountId?: AccountId | null): Promise<Label[]>;
  listCalendars(): Promise<Calendar[]>;
  listThreads(query: ThreadQuery): Promise<ThreadPage>;
  getThread(threadId: ThreadId): Promise<ThreadDetail | null>;
  searchThreads(text: string, limit?: number): Promise<ThreadPage>;
  listEvents(range: TimeRange): Promise<CalendarEvent[]>;

  execute(command: Command): Promise<CommandResult>;
  commandCatalogue(): Promise<CommandSpec[]>;

  syncStatus(): Promise<SyncStatus>;
  syncNow(): Promise<void>;

  beginAddAccount(): Promise<PendingAuthorization>;
  completeAddAccount(pendingId: string): Promise<Account>;
  removeAccount(accountId: AccountId): Promise<void>;
  /** Hand the URL to the system browser; Google's consent screen is not ours. */
  openExternal(url: string): Promise<void>;

  /** Push, never poll. Both return an unsubscribe. */
  onSyncStatus(handler: (status: SyncStatus) => void): Promise<Unsubscribe>;
  onThreadsChanged(handler: () => void): Promise<Unsubscribe>;
}

export const DEFAULT_PAGE_SIZE = 100;

/* -------------------------------------------------------------------------- */
/* Fixture implementation — the offline/dev fallback                           */
/* -------------------------------------------------------------------------- */

/** Newest first, ties broken by id, which is the keyset order the cursor walks. */
function byRecency(a: Thread, b: Thread): number {
  return b.timestamp - a.timestamp || b.id - a.id;
}

function page(rows: Thread[], query: ThreadQuery): ThreadPage {
  const limit = query.limit && query.limit > 0 ? query.limit : DEFAULT_PAGE_SIZE;
  const after = query.after;
  const remaining = after
    ? rows.filter(
        (t) =>
          t.timestamp < after.lastMessageAt ||
          (t.timestamp === after.lastMessageAt && t.id < after.id),
      )
    : rows;
  const threads = remaining.slice(0, limit);
  const last = threads[threads.length - 1];
  return {
    threads,
    nextCursor:
      last && remaining.length > threads.length
        ? { lastMessageAt: last.timestamp, id: last.id }
        : null,
  };
}

function matchesQuery(thread: Thread, query: ThreadQuery): boolean {
  if (query.accountId != null && thread.accountId !== query.accountId) return false;
  if (query.labelId && !thread.labelIds.includes(query.labelId)) return false;
  if (query.unreadOnly && !thread.unread) return false;
  return true;
}

function pluralize(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function fixtureResult(command: Command, message: string): CommandResult {
  return {
    ok: true,
    message,
    undo: inverseOf(command),
    applied: targetIds(command),
    failed: [],
  };
}

export const fixtureSource: MachDataSource = {
  kind: "fixture",

  async listAccounts() {
    return fixtures.accounts;
  },
  async listLabels(accountId) {
    return accountId == null
      ? fixtures.labels
      : fixtures.labels.filter((l) => l.accountId === null || l.accountId === accountId);
  },
  async listCalendars() {
    return fixtures.calendars;
  },
  async listThreads(query) {
    return page(fixtures.threads.filter((t) => matchesQuery(t, query)).sort(byRecency), query);
  },
  async getThread(threadId) {
    const thread = fixtures.threads.find((t) => t.id === threadId);
    if (!thread) return null;
    return { thread, messages: fixtures.messagesByThread.get(threadId) ?? [] };
  },
  async searchThreads(text, limit) {
    const needle = text.trim().toLowerCase();
    if (!needle) return { threads: [], nextCursor: null };
    const rows = fixtures.threads
      .filter((t) =>
        `${t.subject} ${t.snippet} ${t.participants.map((p) => `${p.name} ${p.email}`).join(" ")}`
          .toLowerCase()
          .includes(needle),
      )
      .sort(byRecency);
    return page(rows, { limit });
  },
  async listEvents(range) {
    return fixtures.events
      .filter((e) => e.start < range.end && e.end > range.start)
      .sort((a, b) => a.start - b.start);
  },

  async execute(command) {
    // Fixtures do not persist. The optimistic local state in `useMach` is what
    // the user sees; the real source writes to SQLite and echoes back.
    switch (command.kind) {
      case "archive":
        return fixtureResult(
          command,
          `Archived ${pluralize(command.threadIds.length, "conversation")}`,
        );
      case "unarchive":
        return fixtureResult(command, "Moved back to the inbox");
      case "markRead":
        return fixtureResult(command, command.read ? "Marked read" : "Marked unread");
      case "star":
        return fixtureResult(command, command.starred ? "Starred" : "Unstarred");
      case "label":
        return fixtureResult(command, command.add ? "Label added" : "Label removed");
      case "trash":
        return fixtureResult(command, `Trashed ${pluralize(command.threadIds.length, "conversation")}`);
      case "untrash":
        return fixtureResult(command, "Taken out of the trash");
      case "snooze":
        return fixtureResult(
          command,
          `Snoozed ${pluralize(command.threadIds.length, "conversation")}`,
        );
      case "unsnooze":
        return fixtureResult(command, "Woken");
      case "rsvp":
        return fixtureResult(command, "RSVP sent");
      // The calendar write path is real only against the command layer: these
      // arms keep the fixture window honest rather than pretending it saved.
      case "createEvent":
        return fixtureResult(command, `Created “${command.draft.title || "New event"}”`);
      case "updateEvent":
        return fixtureResult(command, "Saved the event");
      case "deleteEvent":
        return fixtureResult(command, "Deleted the event");
      case "moveEvent":
        return fixtureResult(command, "Moved the event");
    }
  },

  async commandCatalogue() {
    return [];
  },

  async syncStatus() {
    // Fixture data is, by construction, already here. Claiming otherwise would
    // put a progress bar on screen that could never finish.
    return {
      running: false,
      accounts: fixtures.accounts.map((account) => ({
        accountId: account.id,
        email: account.email,
        phase: "done" as const,
        backfillTotal: 0,
        backfillDone: 0,
        messagesWritten: 0,
        eventsWritten: 0,
        lastError: null,
        lastSuccessAt: Date.now(),
        updatedAt: Date.now(),
      })),
      lastPassStartedAt: null,
      lastPassFinishedAt: Date.now(),
      configured: true,
      configurationError: null,
      needsReauthorization: [],
    };
  },
  async syncNow() {
    /* no network to reach from fixtures */
  },

  async beginAddAccount() {
    throw new MachError(
      "unavailable",
      "This window is running on fixture data. Accounts can only be added in the desktop app.",
    );
  },
  async completeAddAccount() {
    throw new MachError("unavailable", "Accounts can only be added in the desktop app.");
  },
  async removeAccount() {
    throw new MachError("unavailable", "Accounts can only be removed in the desktop app.");
  },
  async openExternal(url) {
    if (typeof window !== "undefined") window.open(url, "_blank", "noopener");
  },

  async onSyncStatus() {
    return () => {};
  },
  async onThreadsChanged() {
    return () => {};
  },
};

/* -------------------------------------------------------------------------- */
/* The swap point                                                              */
/* -------------------------------------------------------------------------- */

let current: MachDataSource = fixtureSource;

export function getDataSource(): MachDataSource {
  return current;
}

export function setDataSource(source: MachDataSource): void {
  current = source;
}
