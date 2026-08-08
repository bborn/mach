/**
 * The shapes the UI renders from.
 *
 * These mirror the SQLite tables in the design doc (`accounts`, `threads`,
 * `messages`, `labels`, `attachments`, `events`) minus the storage-only
 * columns. `src/lib/ipc.ts` maps the Rust row types onto these; the fixture
 * source produces them directly.
 *
 * Timestamps are epoch milliseconds everywhere. No Date objects cross the
 * seam.
 *
 * **Ids are local SQLite row ids — `INTEGER PRIMARY KEY`, so `number` here.**
 * The exceptions are labels and calendars, whose ids are Google's own strings
 * (`INBOX`, `Label_12`, a calendar address): a thread carries its Gmail label
 * ids, and the `label` command names one, so a local row id would be the wrong
 * currency in both places.
 */

export type AccountId = number;
export type ThreadId = number;
export type MessageId = number;
export type AttachmentId = number;
export type EventId = number;
export type LabelId = string;
export type CalendarId = string;

/** 1..5 — indexes into the `--color-account-N` token ramp. */
export type ColorIndex = 1 | 2 | 3 | 4 | 5;

export interface Account {
  id: AccountId;
  email: string;
  /** Short human label used in the rail: "Northwind". */
  name: string;
  colorIndex: ColorIndex;
  /** Workspace domains can use an Internal OAuth app; personal cannot. */
  kind: "workspace" | "personal";
}

export interface Participant {
  name: string;
  email: string;
}

export type LabelKind = "system" | "user";

export interface Label {
  id: LabelId;
  /** `null` for the synthetic unified labels that span every account. */
  accountId: AccountId | null;
  name: string;
  kind: LabelKind;
}

export interface Attachment {
  id: AttachmentId;
  messageId: MessageId;
  filename: string;
  mimeType: string;
  sizeBytes: number;
}

export interface Thread {
  id: ThreadId;
  accountId: AccountId;
  subject: string;
  snippet: string;
  /** Ordered, most recent last. Row shows the last non-self participant. */
  participants: Participant[];
  /** Timestamp of the most recent message in the thread. */
  timestamp: number;
  unread: boolean;
  starred: boolean;
  hasAttachment: boolean;
  messageCount: number;
  labelIds: LabelId[];
}

export interface Message {
  id: MessageId;
  threadId: ThreadId;
  accountId: AccountId;
  from: Participant;
  to: Participant[];
  cc: Participant[];
  timestamp: number;
  /** Plaintext body. `bodyHtml` renders in a sandboxed iframe in a later unit. */
  bodyText: string;
  bodyHtml?: string;
  attachments: Attachment[];
}

export interface ThreadDetail {
  thread: Thread;
  messages: Message[];
}

export interface Calendar {
  id: CalendarId;
  accountId: AccountId;
  name: string;
  colorIndex: ColorIndex;
}

export type Rsvp = "accepted" | "declined" | "tentative" | "needsAction";

export interface CalendarEvent {
  id: EventId;
  calendarId: CalendarId;
  accountId: AccountId;
  title: string;
  start: number;
  end: number;
  allDay: boolean;
  location?: string;
  description?: string;
  organizer?: Participant;
  attendees: Participant[];
  rsvp?: Rsvp;
  /** Set when the event was created from a thread — the mail/calendar link. */
  sourceThreadId?: ThreadId;
}

export interface TimeRange {
  start: number;
  end: number;
}

/**
 * Keyset cursor over `(lastMessageAt DESC, id DESC)`.
 *
 * Not an offset: the sync loop inserts at the top of this list continuously, so
 * `LIMIT/OFFSET` would duplicate or skip rows whenever a sync pass lands between
 * two scroll fetches.
 */
export interface ThreadCursor {
  lastMessageAt: number;
  id: ThreadId;
}

export interface ThreadQuery {
  /** `null`/absent means the unified stream across every account. */
  accountId?: AccountId | null;
  /** A Gmail label id — `INBOX`, `STARRED`, `Label_12`. */
  labelId?: LabelId;
  unreadOnly?: boolean;
  /** 0/absent means "the backend's default page size". */
  limit?: number;
  /** Resume point. Rows strictly older than this are returned. */
  after?: ThreadCursor | null;
}

/** One page of the unified stream. `nextCursor === null` means the end. */
export interface ThreadPage {
  threads: Thread[];
  nextCursor: ThreadCursor | null;
}

/* -------------------------------------------------------------------------- */
/* Sync                                                                        */
/* -------------------------------------------------------------------------- */

export type SyncPhase =
  | "idle"
  | "labels"
  | "backfill"
  | "incremental"
  | "calendar"
  | "done"
  | "failed"
  | "cancelled";

/** The phases that mean work is happening right now. */
export const ACTIVE_PHASES: readonly SyncPhase[] = [
  "labels",
  "backfill",
  "incremental",
  "calendar",
];

export interface AccountSyncStatus {
  accountId: AccountId;
  email: string;
  phase: SyncPhase;
  /** Messages enumerated for the backfill; 0 when not backfilling. */
  backfillTotal: number;
  backfillDone: number;
  messagesWritten: number;
  eventsWritten: number;
  /** Cleared when a pass succeeds. One rate-limited account, four healthy. */
  lastError: string | null;
  lastSuccessAt: number | null;
  updatedAt: number;
}

export interface SyncStatus {
  /** Whether the background loop is alive. */
  running: boolean;
  accounts: AccountSyncStatus[];
  lastPassStartedAt: number | null;
  lastPassFinishedAt: number | null;
  /** False when no OAuth client is configured. Everything local still works. */
  configured: boolean;
  /** Set exactly when `configured` is false — a sentence to render. */
  configurationError: string | null;
  /** Accounts whose refresh token is gone from the Keychain. */
  needsReauthorization: string[];
}

/** What `begin_add_account` hands back: where to send the user, and a claim tag. */
export interface PendingAuthorization {
  url: string;
  pendingId: string;
}
