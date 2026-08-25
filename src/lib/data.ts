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
  FilterAction,
  FilterCriteria,
  ForcedSync,
  Label,
  LabelId,
  MailFilter,
  MessageId,
  Participant,
  Rsvp,
  SyncStatus,
  Thread,
  ThreadDetail,
  ThreadId,
  ThreadCursor,
  ThreadPage,
  ThreadQuery,
  TimeRange,
  PendingAuthorization,
} from "@/types";
import type { Contact } from "./contacts";
import * as fixtures from "./fixtures";
import { isOutsidePrimary, PRIMARY_LABEL, PROMOTIONS_LABEL } from "./mailboxes";
import { matchesSearchNode, type SearchNode } from "./search-query";

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

/**
 * Who Google emails about a write to an event.
 *
 * **Omitting this means `guests`, and that is the whole point of the field.**
 * Google's calendar API notifies nobody unless the request says `sendUpdates`.
 * Mach never said it, so an event created here with three guests on it went onto
 * one calendar and no others, and none of the three were told — a failure that
 * looks identical to the working case from the organizer's side.
 *
 * `nobody` is a real thing to want (fixing a typo in the notes of a thirty-person
 * meeting) and it is never the default.
 */
export type Notify = "guests" | "externalGuests" | "nobody";

/**
 * What to do about an event's video call.
 *
 * A verb, not a URL: Google will not accept a Meet link you hand it. The only
 * way onto an event is to ask for one and read back the code Google minted.
 */
export type Conferencing = "meet" | "none";

/**
 * Whether the event defends the time. Google's `transparency`: `opaque` is
 * busy (the default), `transparent` is free.
 */
export type Transparency = "opaque" | "transparent";

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
  /** `meet` asks Google for a Meet link. */
  conferencing?: Conferencing;
  /** Busy or free. Omit to leave Google's default, which is busy. */
  transparency?: Transparency;
  /**
   * Who hears about it. Omitted invites the guests — see {@link Notify}.
   *
   * It rides on the draft rather than beside it because the draft is the whole
   * payload the editor produces, and the answer to "should I invite these
   * people" is made in the same breath as the guest list.
   */
  notify?: Notify;
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
  conferencing?: Conferencing;
  /** Busy or free. */
  transparency?: Transparency;
  /**
   * Who hears about it. Omitted tells the guests — see {@link Notify}.
   *
   * It does not count towards "did anything change": a patch carrying only an
   * answer to "tell the guests?" is still a no-op, and still costs no request.
   */
  notify?: Notify;
}

export type Command =
  | { kind: "archive"; threadIds: ThreadId[] }
  | { kind: "unarchive"; threadIds: ThreadId[]; restore?: ThreadLabelState[] }
  | { kind: "markRead"; threadIds: ThreadId[]; read: boolean }
  | { kind: "star"; threadIds: ThreadId[]; starred: boolean }
  | { kind: "label"; threadIds: ThreadId[]; labelId: LabelId; add: boolean }
  /**
   * Gmail's "Move to Primary": INBOX on, bulk tabs off.
   *
   * `Label` moves one id. Getting a conversation into Mach's Inbox is two
   * things — keep `INBOX`, strip Promotions / Social / Updates / Forums — and
   * composing that from two commands would be two remote calls and two undo
   * entries for one keystroke. The inverse carries the prior label set, the
   * way `notSpam` does, because putting the bulk categories back is only
   * faithful if they were the ones that were there.
   */
  | { kind: "moveToInbox"; threadIds: ThreadId[]; restore?: ThreadLabelState[] }
  | { kind: "reportSpam"; threadIds: ThreadId[] }
  | { kind: "notSpam"; threadIds: ThreadId[]; restore?: ThreadLabelState[] }
  | { kind: "trash"; threadIds: ThreadId[] }
  | { kind: "untrash"; threadIds: ThreadId[]; restore?: ThreadLabelState[] }
  | { kind: "snooze"; threadIds: ThreadId[]; until: number }
  | { kind: "unsnooze"; threadIds: ThreadId[] }
  /**
   * Ask the sender to stop, using the `List-Unsubscribe` header on one message.
   *
   * The only command in the vocabulary that names a message rather than a set
   * of conversations, and the only one with no inverse: it is an outbound
   * request to somebody else's server — an RFC 8058 POST, or a `mailto:`
   * unsubscribe — and nothing here can un-send it. It changes no local row
   * either, which is why `lib/projection.ts` has nothing to say about it.
   *
   * Rust refuses to act for `method: "link"`, and so does the UI: a link is a
   * page a person has to complete. `openUnsubscribePage` is that route.
   */
  | { kind: "unsubscribe"; messageId: MessageId }
  | {
      kind: "rsvp";
      eventId: EventId;
      response: Rsvp;
      /** A note to the organizer, sent with the response. */
      comment?: string;
      notify?: Notify;
    }
  | {
      kind: "createEvent";
      accountId: AccountId;
      calendarId: CalendarId;
      draft: EventDraft;
    }
  | { kind: "updateEvent"; eventId: EventId; patch: EventPatch; scope?: EventScope }
  | { kind: "deleteEvent"; eventId: EventId; scope?: EventScope; notify?: Notify }
  | {
      kind: "moveEvent";
      eventId: EventId;
      accountId: AccountId;
      calendarId: CalendarId;
      notify?: Notify;
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

/**
 * The commands that take a list of conversations.
 *
 * Read off the field rather than off the `kind`, which it did not used to be:
 * "everything that is not a calendar command" was the same set for as long as
 * every non-calendar command carried `threadIds`, and `unsubscribe` is the
 * first that does not. Extracting on the field means a command that names
 * something else is excluded by construction rather than by a list somebody has
 * to remember to add to.
 */
export type MailCommand = Extract<Command, { threadIds: ThreadId[] }>;

const CALENDAR_KINDS = new Set<string>(CALENDAR_COMMAND_KINDS);

export function isMailCommand(command: Command): command is MailCommand {
  return "threadIds" in command;
}

export function isCalendarCommand(command: Command): command is CalendarCommand {
  return CALENDAR_KINDS.has(command.kind);
}

/**
 * The ids a command addresses, whichever part of the vocabulary it is from.
 *
 * Two commands have none. A create's row does not exist until it has run, and
 * `CommandResult.applied` is where the new id comes back. `unsubscribe`
 * addresses no local row at all — its `messageId` says which header to use, not
 * which row to change — so an empty list is the honest answer rather than a
 * shortcoming of this function.
 */
export function targetIds(command: Command): number[] {
  if (isMailCommand(command)) return command.threadIds;
  return "eventId" in command ? [command.eventId] : [];
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
  /**
   * What the ⌘Z entry should say, when that is **less** than {@link message}.
   *
   * Absent for almost everything, because the two are normally the same
   * sentence. Trashing a selection that held drafts is the exception: Gmail's
   * `drafts.delete` is permanent, so "Trashed 3 conversations · discarded 1
   * draft" is what happened and "Trashed 3 conversations" is what ⌘Z would do.
   */
  undoLabel?: string;
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
 *   archive ⇄ unarchive · trash ⇄ untrash · reportSpam ⇄ notSpam ·
 *   snooze → unsnooze
 *
 * Snooze's inverse is `unsnooze`, not `unarchive`: waking a thread restores the
 * labels it was snoozed from, which "add INBOX" would not do. `notSpam` here is
 * the plain form for the same reason `untrash` is — this shape-only version has
 * no prior state to name, and the real command layer's inverse carries one.
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
    case "reportSpam":
      return { kind: "notSpam", threadIds: command.threadIds };
    case "notSpam":
      return { kind: "reportSpam", threadIds: command.threadIds };
    case "snooze":
      return { kind: "unsnooze", threadIds: command.threadIds };
    case "markRead":
      return { ...command, read: !command.read };
    case "star":
      return { ...command, starred: !command.starred };
    case "label":
      return { ...command, add: !command.add };
    /*
     * There is no inverse, and there is no state anywhere that would supply
     * one. The other cases below need something only the command layer holds;
     * this one needs something nobody holds. An unsubscribe is a request that
     * left the machine — the sender's list has already been written to, and
     * "subscribe me again" is not a thing `List-Unsubscribe` can express. The
     * gesture in `useMach` archives the conversation *as well*, and that half
     * is what ⌘Z takes back.
     */
    case "unsubscribe":
    // These need state only the command layer holds: the prior labels, the
    // prior RSVP, the row id a create is about to mint, the calendar an event
    // came from. Nothing local can honestly claim an inverse.
    case "moveToInbox":
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

/** The second half of `searchThreads`: everything the operator search needs. */
export interface SearchOptions {
  /** The parsed query. Absent means "rank the raw text", which is what ⌘K does. */
  filter?: SearchNode | null;
  /** Scope to one account, as the rail does. Absent searches every account. */
  accountId?: AccountId | null;
  /** Keyset resume point, for the next page of results. */
  cursor?: ThreadCursor | null;
}

export interface MachDataSource {
  /** Which implementation this is. The UI uses it to say so, honestly. */
  readonly kind: "tauri" | "fixture";

  listAccounts(): Promise<Account[]>;
  listLabels(accountId?: AccountId | null): Promise<Label[]>;
  listCalendars(): Promise<Calendar[]>;
  listThreads(query: ThreadQuery): Promise<ThreadPage>;
  getThread(threadId: ThreadId): Promise<ThreadDetail | null>;
  /**
   * Search.
   *
   * With no `options.filter` this is the ⌘K path: a bag of words, ranked by
   * relevance. With one — the AST `parseSearchQuery` produced from the same
   * text — it is the operator search behind the search view: compiled to SQL,
   * newest first, and paginated with the same cursor as the stream.
   */
  searchThreads(text: string, limit?: number, options?: SearchOptions): Promise<ThreadPage>;
  listEvents(range: TimeRange): Promise<CalendarEvent[]>;

  /**
   * Every address the store has seen, ranked, for the address fields.
   *
   * A scan over every message, so this is the one read the UI starts and then
   * forgets about: it is fired once after the first render and nothing waits
   * on it. See `useContacts`.
   */
  listContacts(): Promise<Contact[]>;

  /**
   * Dispatch a command.
   *
   * `source` says who asked — `user` (the default), `agent`, or
   * `plugin:<id>` — and it is not decoration. A plugin source is checked
   * against that plugin's declared capabilities and rate limit on the Rust
   * side before anything runs, and it is what makes "what did that plugin do
   * to my mailbox" an answerable question.
   */
  execute(command: Command, source?: string): Promise<CommandResult>;
  commandCatalogue(): Promise<CommandSpec[]>;

  syncStatus(): Promise<SyncStatus>;
  /**
   * Sync mail and calendar now — every account, or `accountId` alone.
   *
   * Resolves when the pass is over. Nothing on screen waits for it; what waits
   * is the one line that says whether it worked.
   */
  syncNow(accountId?: AccountId): Promise<ForcedSync>;

  /**
   * Start an authorization. `email` names the account being repaired, and the
   * flow refuses to finish as anyone else.
   */
  beginAddAccount(email?: string): Promise<PendingAuthorization>;
  completeAddAccount(pendingId: string): Promise<Account>;
  removeAccount(accountId: AccountId): Promise<void>;

  /**
   * Gmail filters, read live from Google rather than from the local store.
   *
   * The one place the app waits on Google, and the argument is in
   * `src-tauri/src/commands/filters.rs`: a filter has never been a local row,
   * Google offers no change feed to keep a copy fresh, and a delete addressing
   * an id from a stale list is worse than a moment of waiting. Callers must
   * render the failure — `MachError` carries the sentence.
   */
  listFilters(accountId?: AccountId | null): Promise<MailFilter[]>;
  createFilter(
    accountId: AccountId,
    criteria: FilterCriteria,
    action: FilterAction,
  ): Promise<MailFilter>;
  deleteFilter(accountId: AccountId, filterId: string): Promise<void>;
  /** Hand the URL to the system browser; Google's consent screen is not ours. */
  openExternal(url: string): Promise<void>;

  /**
   * Open the unsubscribe page this message's header points at.
   *
   * The URL is never sent to the webview, which is the point of a second door
   * beside `openExternal`: a `List-Unsubscribe` URL comes from the sender, and
   * handing an arbitrary one to the page would make the mailbox a place a
   * stranger can put a link. Rust validates it — https, and the one the header
   * actually named — and opens it itself.
   *
   * Two callers: the `link` method, which no command can act on, and the
   * failure of an `oneClick` or `mail` unsubscribe, where the page is what is
   * left to try.
   *
   * `system` picks the destination. Left off, the page opens in Mach's own
   * page window — a webview with no capability grant and no cookies shared with
   * anything, which is where reading a form is safe. Set, it goes to the
   * default browser instead, which is the one that has his sessions in it and
   * is what a login-walled unsubscribe needs. Either way this side never learns
   * the URL.
   */
  openUnsubscribePage(messageId: MessageId, system?: boolean): Promise<void>;

  /** Push, never poll. All three return an unsubscribe. */
  onSyncStatus(handler: (status: SyncStatus) => void): Promise<Unsubscribe>;
  onThreadsChanged(handler: () => void): Promise<Unsubscribe>;
  /**
   * A snooze that came due and could not be woken.
   *
   * The wake sweep runs on a tick with no gesture behind it, so its refusals
   * have no status line of their own to land on. The conversation is still
   * snoozed and the next sweep retries it; this is how the owner gets to know
   * that is happening.
   */
  onWakeFailed(handler: (failure: WakeFailure) => void): Promise<Unsubscribe>;
}

/** One refused wake, as `ipc::events::WakeFailedPayload` sends it. */
export interface WakeFailure {
  threadIds: ThreadId[];
  message: string;
  retriable: boolean;
}

/** "Could not wake 2 conversations — Google had an error". */
export function describeWakeFailure(failure: WakeFailure): string {
  const what = pluralize(failure.threadIds.length, "conversation");
  return `Could not wake ${what} — ${failure.message}`;
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
  if (query.labelId && !inMailbox(thread, query.labelId)) return false;
  if (query.unreadOnly && !thread.unread) return false;
  return true;
}

/**
 * Carrying any of these means the conversation is filed somewhere of its own,
 * and so is not in the archive. The same list as `FILED_ELSEWHERE` in
 * `db::queries` — the two are separate implementations of one definition, and a
 * fixture app that disagreed about what Archive means would be worse than no
 * fixture app.
 */
const FILED_ELSEWHERE: readonly LabelId[] = ["INBOX", "SENT", "DRAFT", "SPAM", "TRASH"];

/**
 * Whether a thread is in one mailbox. Archive and Snoozed are questions rather
 * than labels; see `withVirtualMailboxes`.
 */
function inMailbox(thread: Thread, labelId: LabelId): boolean {
  if (labelId === "ARCHIVE") {
    return (
      !thread.labelIds.some((id) => FILED_ELSEWHERE.includes(id)) &&
      !fixtures.SNOOZED_THREAD_IDS.includes(thread.id)
    );
  }
  if (labelId === "SNOOZED") return fixtures.SNOOZED_THREAD_IDS.includes(thread.id);
  if (labelId === PRIMARY_LABEL) {
    return thread.labelIds.includes("INBOX") && !isOutsidePrimary(thread.labelIds);
  }
  if (labelId === PROMOTIONS_LABEL) {
    return thread.labelIds.includes(labelId) && thread.labelIds.includes("INBOX");
  }
  return thread.labelIds.includes(labelId);
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
  async searchThreads(text, limit, options) {
    /*
     * The operator path, evaluated in TypeScript — which is exactly what the
     * real source must never do, and exactly right here. There are two dozen
     * fixture threads and no SQLite to compile against; the point of this arm
     * is that `bun run dev` in a browser tab exercises the same parser, the
     * same AST and the same view as the app does.
     */
    if (options?.filter) {
      const rows = fixtures.threads
        .filter((t) => options.accountId == null || t.accountId === options.accountId)
        .filter((t) =>
          matchesSearchNode(options.filter!, {
            thread: t,
            messages: fixtures.messagesByThread.get(t.id) ?? [],
          }),
        )
        .sort(byRecency);
      return page(rows, { limit, after: options.cursor ?? null });
    }

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
  // People who are in no fixture thread, so the fixture browser shows what the
  // real store does: completion for somebody who is not on screen.
  async listContacts(): Promise<Contact[]> {
    return fixtures.contacts;
  },

  async execute(command, _source) {
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
      case "moveToInbox":
        return fixtureResult(
          command,
          command.restore?.length
            ? "Moved back"
            : `Moved ${pluralize(command.threadIds.length, "conversation")} to Inbox`,
        );
      case "reportSpam":
        return fixtureResult(
          command,
          `Reported ${pluralize(command.threadIds.length, "conversation")} as spam`,
        );
      case "notSpam":
        return fixtureResult(
          command,
          `Marked ${pluralize(command.threadIds.length, "conversation")} not spam`,
        );
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
      // No `undo`, no `applied`: nothing local changed and nothing can be taken
      // back. `fixtureResult` would attach an inverse it does not have.
      case "unsubscribe":
        return { ok: true, message: "Unsubscribed", applied: [], failed: [] };
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
        needsReauthorization: false,
        lastSuccessAt: Date.now(),
        updatedAt: Date.now(),
      })),
      lastPassStartedAt: null,
      lastPassFinishedAt: Date.now(),
      configured: true,
      configurationError: null,
      needsReauthorization: [],
      missingScope: [],
      storeEmpty: fixtures.threads.length === 0,
    };
  },
  async syncNow(accountId?: AccountId): Promise<ForcedSync> {
    // Fixtures have no Google to look at, so the honest answer is a pass that
    // ran and found nothing — not a failure, which would put a red dot in the
    // status bar of a window that is working exactly as intended.
    return {
      started: true,
      accounts: fixtures.accounts
        .filter((account) => accountId === undefined || account.id === accountId)
        .map((account) => ({
          accountId: account.id,
          email: account.email,
          messagesWritten: 0,
          eventsWritten: 0,
          error: null,
          needsReauthorization: false,
          cancelled: false,
          skipped: false,
        })),
    };
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

  // A filter lives in Gmail, not in the fixture set, so there is nothing
  // honest to render here and nothing to pretend was created.
  async listFilters() {
    return [];
  },
  async createFilter(): Promise<MailFilter> {
    throw new MachError("unavailable", "Filters can only be created in the desktop app.");
  },
  async deleteFilter() {
    throw new MachError("unavailable", "Filters can only be removed in the desktop app.");
  },
  async openExternal(url) {
    if (typeof window !== "undefined") window.open(url, "_blank", "noopener");
  },
  // The URL lives in the store beside the message, and the fixture browser has
  // neither. Resolving is what a fixture can honestly do here: the gesture
  // completes, and nothing claims a page was opened.
  async openUnsubscribePage() {},

  async onSyncStatus() {
    return () => {};
  },
  async onThreadsChanged() {
    return () => {};
  },
  async onWakeFailed() {
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
