/**
 * `MachDataSource` over Tauri IPC.
 *
 * This is the only file that knows the wire format. The Rust side hands back
 * its *row* types — `ThreadSummary`, `Message`, `Event` — which are shaped for
 * SQLite, not for a thread row: `lastMessageAt` rather than `timestamp`,
 * `isUnread` rather than `unread`, `Option<String>` where the UI wants a string.
 * Mapping them here rather than teaching components two vocabularies is the
 * whole point of the seam; every field the UI reads is produced in one place,
 * so a rename on either side breaks exactly one file.
 *
 * The transport is injected. `invoke`, `listen` and the opener plugin are
 * dynamic imports behind `tauriTransport`, which keeps them out of a plain
 * browser bundle's critical path and lets the tests drive this module with a
 * fake and no Tauri runtime at all.
 */

import type {
  Account,
  AccountId,
  AccountSyncStatus,
  Attachment,
  Calendar,
  CalendarEvent,
  ColorIndex,
  Label,
  Message,
  Participant,
  PendingAuthorization,
  Rsvp,
  SyncPhase,
  SyncStatus,
  Thread,
  ThreadDetail,
  ThreadId,
  ThreadPage,
  ThreadQuery,
  TimeRange,
} from "@/types";
import {
  MachError,
  type Command,
  type CommandResult,
  type CommandSpec,
  type MachDataSource,
  type Unsubscribe,
} from "./data";

/* -------------------------------------------------------------------------- */
/* Transport                                                                   */
/* -------------------------------------------------------------------------- */

export interface IpcTransport {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, handler: (payload: T) => void): Promise<Unsubscribe>;
  openExternal(url: string): Promise<void>;
}

/** The two push channels. Neither is polled; both refetch on arrival. */
export const SYNC_STATUS_EVENT = "sync-status";
export const THREADS_CHANGED_EVENT = "threads-changed";

/** True inside a Tauri window, false in a browser tab running `bun run dev`. */
export function isTauri(): boolean {
  if (typeof window === "undefined") return false;
  const w = window as unknown as Record<string, unknown>;
  return "__TAURI_INTERNALS__" in w || "__TAURI__" in w;
}

export const tauriTransport: IpcTransport = {
  async invoke(command, args) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke(command, args);
  },
  async listen(event, handler) {
    const { listen } = await import("@tauri-apps/api/event");
    const unlisten = await listen(event, (e) => handler(e.payload as never));
    return () => void unlisten();
  },
  async openExternal(url) {
    const { openUrl } = await import("@tauri-apps/plugin-opener");
    await openUrl(url);
  },
};

/* -------------------------------------------------------------------------- */
/* Wire shapes — what Rust actually serializes                                 */
/* -------------------------------------------------------------------------- */

type Nullable<T> = T | null | undefined;

interface WireParticipant {
  name?: Nullable<string>;
  email?: Nullable<string>;
}

interface WireAccount {
  id: number;
  email: string;
  displayName?: Nullable<string>;
  /** British spelling, as in `db::models::Account`. */
  colourIndex?: Nullable<number>;
  colorIndex?: Nullable<number>;
}

interface WireLabel {
  id: number;
  accountId: number;
  gmailLabelId: string;
  name: string;
  labelType?: Nullable<string>;
}

interface WireCalendar {
  id: string;
  accountId: number;
  name?: Nullable<string>;
  summary?: Nullable<string>;
  colourIndex?: Nullable<number>;
  colorIndex?: Nullable<number>;
}

interface WireThread {
  id: number;
  accountId: number;
  subject?: Nullable<string>;
  snippet?: Nullable<string>;
  participants?: Nullable<WireParticipant[]>;
  lastMessageAt?: Nullable<number>;
  isUnread?: Nullable<boolean>;
  messageCount?: Nullable<number>;
  hasAttachments?: Nullable<boolean>;
  labelIds?: Nullable<string[]>;
}

interface WireAttachment {
  id: number;
  messageId: number;
  filename?: Nullable<string>;
  mimeType?: Nullable<string>;
  sizeBytes?: Nullable<number>;
}

interface WireMessage {
  id: number;
  threadId: number;
  accountId: number;
  from?: Nullable<WireParticipant>;
  to?: Nullable<WireParticipant[]>;
  cc?: Nullable<WireParticipant[]>;
  internalDate?: Nullable<number>;
  bodyText?: Nullable<string>;
  bodyHtml?: Nullable<string>;
  snippet?: Nullable<string>;
  attachments?: Nullable<WireAttachment[]>;
}

interface WireThreadDetail {
  thread: WireThread;
  messages?: Nullable<WireMessage[]>;
}

interface WireThreadPage {
  /** `ipc::types::ThreadPage` calls it `items`. */
  items?: Nullable<WireThread[]>;
  nextCursor?: Nullable<{ lastMessageAt: number; id: number }>;
}

interface WireEvent {
  id: number;
  accountId: number;
  calendarId: string;
  title?: Nullable<string>;
  description?: Nullable<string>;
  location?: Nullable<string>;
  startTs?: Nullable<number>;
  endTs?: Nullable<number>;
  isAllDay?: Nullable<boolean>;
  organizer?: Nullable<WireParticipant>;
  attendees?: Nullable<WireParticipant[]>;
  rsvpStatus?: Nullable<string>;
  status?: Nullable<string>;
  /** `db::models::Event.recurring_event_id` — set on an expanded occurrence. */
  recurringEventId?: Nullable<string>;
  /** `db::models::Event.html_link` — Google's own URL for this event. */
  htmlLink?: Nullable<string>;
  sourceThreadId?: Nullable<number>;
  /** RRULE lines. Only ever set on a series master; see migration 5. */
  recurrence?: Nullable<string[]>;
  /**
   * Google's three-state shape, kept whole. "The calendar's default" and "no
   * alert at all" are different answers and a bare minute count cannot tell
   * them apart.
   */
  reminders?: Nullable<{
    useDefault?: Nullable<boolean>;
    overrides?: Nullable<{ method?: Nullable<string>; minutes?: Nullable<number> }[]>;
  }>;
  /** Stable identity across accounts — what makes cross-account merge exact. */
  iCalUID?: Nullable<string>;
  organizerSelf?: Nullable<boolean>;
  guestsCanModify?: Nullable<boolean>;
}

interface WireAccountSyncStatus {
  accountId: number;
  email?: Nullable<string>;
  phase?: Nullable<string>;
  backfillTotal?: Nullable<number>;
  backfillDone?: Nullable<number>;
  messagesWritten?: Nullable<number>;
  eventsWritten?: Nullable<number>;
  lastError?: Nullable<string>;
  lastSuccessAt?: Nullable<number>;
  updatedAt?: Nullable<number>;
}

interface WireSyncStatus {
  running?: Nullable<boolean>;
  accounts?: Nullable<WireAccountSyncStatus[]>;
  lastPassStartedAt?: Nullable<number>;
  lastPassFinishedAt?: Nullable<number>;
  configured?: Nullable<boolean>;
  configurationError?: Nullable<string>;
  needsReauthorization?: Nullable<string[]>;
}

/* -------------------------------------------------------------------------- */
/* Mapping                                                                     */
/* -------------------------------------------------------------------------- */

const GMAIL_STARRED = "STARRED";
const PERSONAL_DOMAINS = new Set(["gmail.com", "googlemail.com"]);
const RSVP_VALUES = new Set<Rsvp>(["accepted", "declined", "tentative", "needsAction"]);
const SYNC_PHASES = new Set<SyncPhase>([
  "idle",
  "labels",
  "backfill",
  "incremental",
  "calendar",
  "done",
  "failed",
  "cancelled",
]);

function num(value: Nullable<number>, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function text(value: Nullable<string>, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function optional(value: Nullable<string>): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function clampColor(value: number): ColorIndex {
  return Math.min(5, Math.max(1, Math.round(value))) as ColorIndex;
}

function mapParticipant(wire: Nullable<WireParticipant>): Participant {
  const email = text(wire?.email);
  const name = text(wire?.name).trim();
  // A display name is optional on the wire and mandatory on screen; the address
  // is the honest fallback, never an empty cell.
  return { name: name || email, email };
}

function mapParticipants(wire: Nullable<WireParticipant[]>): Participant[] {
  return (wire ?? []).map(mapParticipant);
}

/**
 * Accounts, with the colour ramp normalized.
 *
 * `accounts.colour_index` defaults to `0` in the schema and the UI ramp is
 * 1..5, so a set containing a zero is read as 0-based and shifted. Mapping the
 * whole list at once is what makes that decidable.
 */
export function mapAccounts(wire: WireAccount[]): Account[] {
  const raw = wire.map((a) => a.colourIndex ?? a.colorIndex);
  const zeroBased = raw.some((v) => v === 0);
  return wire.map((account, index) => {
    const value = raw[index];
    const colorIndex =
      typeof value === "number" && Number.isFinite(value)
        ? clampColor(zeroBased ? value + 1 : value)
        : clampColor(index + 1);
    const domain = account.email.split("@")[1]?.toLowerCase() ?? "";
    return {
      id: account.id,
      email: account.email,
      name: optional(account.displayName) ?? account.email.split("@")[0] ?? account.email,
      colorIndex,
      // Workspace domains can use an Internal OAuth app; consumer ones cannot.
      kind: PERSONAL_DOMAINS.has(domain) ? "personal" : "workspace",
    };
  });
}

/**
 * Labels, collapsed to the ids the UI filters on.
 *
 * Gmail's system label ids (`INBOX`, `STARRED`) are identical across accounts,
 * so five rows become one unified row with `accountId: null` — the synthetic
 * unified label the rail already understands. User labels have per-account ids,
 * so they stay per-account and keep the account they came from.
 */
export function mapLabels(wire: WireLabel[]): Label[] {
  const byGmailId = new Map<string, Label>();
  const owners = new Map<string, Set<number>>();

  for (const label of wire) {
    const owned = owners.get(label.gmailLabelId) ?? new Set<number>();
    owned.add(label.accountId);
    owners.set(label.gmailLabelId, owned);

    if (!byGmailId.has(label.gmailLabelId)) {
      byGmailId.set(label.gmailLabelId, {
        id: label.gmailLabelId,
        accountId: label.accountId,
        name: label.name,
        kind: label.labelType === "system" ? "system" : "user",
      });
    }
  }

  return [...byGmailId.values()].map((label) => ({
    ...label,
    accountId: (owners.get(label.id)?.size ?? 1) > 1 ? null : label.accountId,
  }));
}

export function mapCalendars(wire: WireCalendar[], accounts: Account[]): Calendar[] {
  const colorByAccount = new Map(accounts.map((a) => [a.id, a.colorIndex]));
  return wire.map((calendar, index) => {
    const explicit = calendar.colourIndex ?? calendar.colorIndex;
    return {
      id: calendar.id,
      accountId: calendar.accountId,
      name: optional(calendar.name) ?? optional(calendar.summary) ?? calendar.id,
      colorIndex:
        typeof explicit === "number" && Number.isFinite(explicit)
          ? clampColor(explicit)
          : (colorByAccount.get(calendar.accountId) ?? clampColor(index + 1)),
    };
  });
}

export function mapThread(wire: WireThread): Thread {
  const labelIds = wire.labelIds ?? [];
  return {
    id: wire.id,
    accountId: wire.accountId,
    subject: text(wire.subject, "(no subject)"),
    snippet: text(wire.snippet),
    participants: mapParticipants(wire.participants),
    timestamp: num(wire.lastMessageAt),
    unread: wire.isUnread === true,
    // Gmail has no starred column; the star *is* a label.
    starred: labelIds.includes(GMAIL_STARRED),
    hasAttachment: wire.hasAttachments === true,
    messageCount: Math.max(1, num(wire.messageCount, 1)),
    labelIds,
  };
}

function mapAttachment(wire: WireAttachment): Attachment {
  return {
    id: wire.id,
    messageId: wire.messageId,
    filename: text(wire.filename, "attachment"),
    mimeType: text(wire.mimeType, "application/octet-stream"),
    sizeBytes: num(wire.sizeBytes),
  };
}

export function mapMessage(wire: WireMessage): Message {
  return {
    id: wire.id,
    threadId: wire.threadId,
    accountId: wire.accountId,
    from: mapParticipant(wire.from),
    to: mapParticipants(wire.to),
    cc: mapParticipants(wire.cc),
    timestamp: num(wire.internalDate),
    // A message with only HTML still has to render something in the plaintext
    // pane until the sandboxed iframe lands; the snippet is what Gmail shows.
    bodyText: text(wire.bodyText) || text(wire.snippet),
    bodyHtml: optional(wire.bodyHtml),
    attachments: (wire.attachments ?? []).map(mapAttachment),
  };
}

export function mapThreadPage(wire: Nullable<WireThreadPage>): ThreadPage {
  return {
    threads: (wire?.items ?? []).map(mapThread),
    nextCursor: wire?.nextCursor ?? null,
  };
}

export function mapEvent(wire: WireEvent): CalendarEvent {
  const rsvp = wire.rsvpStatus as Rsvp | null | undefined;
  return {
    id: wire.id,
    calendarId: wire.calendarId,
    accountId: wire.accountId,
    title: text(wire.title, "(no title)"),
    start: num(wire.startTs),
    end: num(wire.endTs),
    allDay: wire.isAllDay === true,
    location: optional(wire.location),
    description: optional(wire.description),
    organizer: wire.organizer ? mapParticipant(wire.organizer) : undefined,
    attendees: mapParticipants(wire.attendees),
    rsvp: rsvp && RSVP_VALUES.has(rsvp) ? rsvp : undefined,
    // These two were dropped here for a long time, and three separate paper
    // cuts were downstream of it: "is this one of a series?" had to be guessed
    // from titles and durations, "Open in Google" could only open the *day*,
    // and the modal could not say what an existing series' rule was. The rows
    // carried both fields the whole time.
    recurringEventId: optional(wire.recurringEventId),
    htmlLink: optional(wire.htmlLink),
    sourceThreadId: typeof wire.sourceThreadId === "number" ? wire.sourceThreadId : undefined,
    /*
     * And the same trap again, one migration later. Every field below is
     * stored, synced, tested and read by the modal — but this function builds a
     * fresh object literal, so anything not named here is dropped in silence
     * between Rust and the UI. There is no type error, because `WireEvent` is
     * our own description of the wire rather than something generated from it.
     *
     * That is what makes this mapper the place a column goes to die: adding one
     * touches the schema, the model, the sync and the component, all of which
     * fail loudly if you get them wrong, and then this, which does not.
     */
    recurrence: wire.recurrence ?? undefined,
    reminders: wire.reminders
      ? {
          // Absent is not the same as false anywhere else in this mapper, but
          // it is here: Google omits `useDefault` when it is not set.
          useDefault: wire.reminders.useDefault === true,
          overrides: (wire.reminders.overrides ?? []).map((r) => ({
            method: text(r.method, "popup"),
            minutes: num(r.minutes),
          })),
        }
      : undefined,
    iCalUID: optional(wire.iCalUID),
    // `undefined` means "Google did not tell us", which every consumer treats
    // as permissive. Collapsing it to `false` would make every row written
    // before migration 5 read-only on first launch.
    organizerSelf: typeof wire.organizerSelf === "boolean" ? wire.organizerSelf : undefined,
    guestsCanModify:
      typeof wire.guestsCanModify === "boolean" ? wire.guestsCanModify : undefined,
  };
}

function mapAccountSyncStatus(wire: WireAccountSyncStatus): AccountSyncStatus {
  const phase = wire.phase as SyncPhase | null | undefined;
  return {
    accountId: wire.accountId,
    email: text(wire.email),
    phase: phase && SYNC_PHASES.has(phase) ? phase : "idle",
    backfillTotal: num(wire.backfillTotal),
    backfillDone: num(wire.backfillDone),
    messagesWritten: num(wire.messagesWritten),
    eventsWritten: num(wire.eventsWritten),
    lastError: optional(wire.lastError) ?? null,
    lastSuccessAt: typeof wire.lastSuccessAt === "number" ? wire.lastSuccessAt : null,
    updatedAt: num(wire.updatedAt),
  };
}

export function mapSyncStatus(wire: Nullable<WireSyncStatus>): SyncStatus {
  return {
    running: wire?.running === true,
    accounts: (wire?.accounts ?? []).map(mapAccountSyncStatus),
    lastPassStartedAt: typeof wire?.lastPassStartedAt === "number" ? wire.lastPassStartedAt : null,
    lastPassFinishedAt:
      typeof wire?.lastPassFinishedAt === "number" ? wire.lastPassFinishedAt : null,
    // Absent means "an older backend that cannot tell us" — assume configured
    // rather than accusing a working app of being unconfigured.
    configured: wire?.configured !== false,
    configurationError: optional(wire?.configurationError) ?? null,
    needsReauthorization: wire?.needsReauthorization ?? [],
  };
}

/* -------------------------------------------------------------------------- */
/* Errors                                                                      */
/* -------------------------------------------------------------------------- */

/** Text out of whatever Rust returned: `{kind, message}`, a string, or an Error. */
export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object") {
    const record = error as Record<string, unknown>;
    if (typeof record.message === "string") return record.message;
    if (typeof record.kind === "string") return record.kind;
  }
  return "Something went wrong talking to the backend.";
}

/**
 * "There are no Google credentials" is a different problem from "the sync
 * failed", and the UI has to say so: the fix is an environment variable, not a
 * retry. `ClientConfig::from_env` reports it as a missing `MACH_GOOGLE_CLIENT_ID`.
 */
const NOT_CONFIGURED = /MACH_GOOGLE_CLIENT_ID|MACH_GOOGLE_CLIENT_SECRET|missing config|not configured|no oauth client/i;

export function toMachError(error: unknown): MachError {
  if (error instanceof MachError) return error;
  const message = errorMessage(error);
  return new MachError(NOT_CONFIGURED.test(message) ? "notConfigured" : "backend", message, error);
}

/* -------------------------------------------------------------------------- */
/* The source                                                                  */
/* -------------------------------------------------------------------------- */

export function createIpcSource(transport: IpcTransport): MachDataSource {
  async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    try {
      return await transport.invoke<T>(command, args);
    } catch (error) {
      throw toMachError(error);
    }
  }

  return {
    kind: "tauri",

    async listAccounts() {
      return mapAccounts(await call<WireAccount[]>("list_accounts"));
    },

    async listLabels(accountId) {
      return mapLabels(
        await call<WireLabel[]>("list_labels", accountId == null ? {} : { accountId }),
      );
    },

    async listCalendars() {
      const [calendars, accounts] = await Promise.all([
        call<WireCalendar[]>("list_calendars"),
        call<WireAccount[]>("list_accounts"),
      ]);
      return mapCalendars(calendars, mapAccounts(accounts));
    },

    async listThreads(query: ThreadQuery) {
      return mapThreadPage(await call<WireThreadPage>("list_threads", { query: wireQuery(query) }));
    },

    async getThread(threadId: ThreadId): Promise<ThreadDetail | null> {
      const detail = await call<WireThreadDetail | null>("get_thread", { threadId });
      if (!detail?.thread) return null;
      return {
        thread: mapThread(detail.thread),
        messages: (detail.messages ?? []).map(mapMessage),
      };
    },

    async searchThreads(text_, limit, options) {
      // `filter` is what turns this into the operator search on the Rust side;
      // without it the command answers exactly as it always has. Absent keys
      // rather than nulls, so an older backend deserializes the same payload.
      const args: Record<string, unknown> = { query: text_, limit };
      if (options?.filter) args.filter = options.filter;
      if (options?.accountId != null) args.accountId = options.accountId;
      if (options?.cursor) args.cursor = options.cursor;
      return mapThreadPage(await call<WireThreadPage>("search_threads", args));
    },

    async listEvents(range: TimeRange) {
      const events = await call<WireEvent[]>("list_events", {
        startMs: range.start,
        endMs: range.end,
      });
      return events
        .filter((e) => e.status !== "cancelled")
        .map(mapEvent)
        .sort((a, b) => a.start - b.start);
    },

    async execute(command: Command, source?: string): Promise<CommandResult> {
      const result = await call<Partial<CommandResult>>("execute_command", { command, source });
      // `applied` and `failed` decide what the UI reverts, so neither may be
      // left undefined by a backend that skipped an empty field.
      return {
        ok: result.ok !== false,
        message: text(result.message, "Done"),
        undo: result.undo,
        applied: result.applied ?? [],
        failed: result.failed ?? [],
      };
    },

    async commandCatalogue() {
      return (await call<CommandSpec[]>("command_catalogue")) ?? [];
    },

    async syncStatus() {
      return mapSyncStatus(await call<WireSyncStatus>("sync_status"));
    },

    async syncNow() {
      await call<void>("sync_now");
    },

    async beginAddAccount(): Promise<PendingAuthorization> {
      const pending = await call<PendingAuthorization>("begin_add_account");
      return { url: text(pending?.url), pendingId: text(pending?.pendingId) };
    },

    async completeAddAccount(pendingId: string): Promise<Account> {
      const account = await call<WireAccount>("complete_add_account", { pendingId });
      return mapAccounts([account])[0]!;
    },

    async removeAccount(accountId: AccountId) {
      await call<void>("remove_account", { accountId });
    },

    async openExternal(url: string) {
      try {
        await transport.openExternal(url);
      } catch (error) {
        throw toMachError(error);
      }
    },

    async onSyncStatus(handler) {
      return transport.listen<WireSyncStatus>(SYNC_STATUS_EVENT, (payload) =>
        handler(mapSyncStatus(payload)),
      );
    },

    async onThreadsChanged(handler) {
      return transport.listen(THREADS_CHANGED_EVENT, () => handler());
    },
  };
}

/**
 * The query as Rust wants it: no `undefined` keys, no client-only fields, and
 * `accountId: null` for the unified stream rather than an absent key.
 */
function wireQuery(query: ThreadQuery): Record<string, unknown> {
  // `cursor` is the canonical name on `ipc::types::ThreadQuery`; it also accepts
  // `after`, but sending both would be a duplicate field to serde.
  const out: Record<string, unknown> = {
    accountId: query.accountId ?? null,
    unreadOnly: query.unreadOnly ?? false,
    limit: query.limit ?? 0,
    cursor: query.after ?? null,
  };
  if (query.labelId) out.labelId = query.labelId;
  return out;
}

/** The real thing, wired to the Tauri runtime. */
export function createTauriSource(): MachDataSource {
  return createIpcSource(tauriTransport);
}
