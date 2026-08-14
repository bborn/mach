import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { useKeymap } from "@/hooks/useKeymap";
import type {
  Account,
  AccountId,
  Calendar,
  CalendarEvent,
  CalendarId,
  EventId,
  Label,
  LabelId,
  SyncStatus,
  Thread,
  ThreadDetail,
  ThreadId,
} from "@/types";
import {
  describeResult,
  describeWakeFailure,
  failedIds,
  getDataSource,
  isCalendarCommand,
  FAILURE_LABELS,
  type Command,
  type CommandResult,
  type MailCommand,
} from "@/lib/data";
import { unsubscribeAction } from "@/components/mail/unsubscribe-offer";
import {
  applyEventGuesses,
  applyGuess,
  byRecency,
  guessedEventIds,
  leavesMailbox,
  leavingIds,
  pendingEventId,
  placeholderEvent,
  project,
  projectEvent,
  returningRows,
  settledEventGuesses,
  settledGuesses,
  settledPendingEvents,
  INBOX,
  READ_GUESS,
  type EventGuesses,
  type Guesses,
  type PendingEvent,
} from "@/lib/projection";
import {
  emptyUndo,
  pushUndo,
  recordUndo,
  runRedo,
  runUndo,
  type UndoHost,
  type UndoOutcome,
  type UndoPlace,
  type UndoState,
} from "@/lib/undo-stack";
import {
  mailboxState,
  syncProgress,
  type MailboxError,
  type MailboxState,
  type SyncProgress,
} from "@/lib/mailbox-state";
import {
  favoriteKey,
  isFavorited,
  loadFavorites,
  removeFavorite,
  saveFavorites,
  toggleFavorite,
  type Favorite,
} from "@/lib/favorites";
import {
  clear as clearSelection,
  commandTargets,
  emptySelection,
  extendTo,
  nextAfterRemoval,
  prune,
  reanchor,
  anchorAt,
  selectAllMessage,
  selectOnly,
  toggle as toggleSelection,
  toggleAll,
  type Selection,
} from "@/lib/selection";
import { mailboxName, withVirtualMailboxes } from "@/lib/mailboxes";
import type { Contact } from "@/lib/contacts";
import type { Artifact } from "@/lib/agent";
import { connectNotificationOpen } from "@/lib/notification-open";
import { beginForcedSync, endForcedSync, forcedSyncMessage } from "@/lib/force-sync";
import { toMailboxError, useThreadStream } from "@/hooks/useThreadStream";
import { DAY, addDays, addMonths, startOfWeek } from "@/lib/time";
import { snoozeLabel } from "@/lib/snooze";
import { undoWindowMs, type WeekStart } from "@/lib/prefs";
import {
  setPreferenceFromAnywhere,
  usePreferences,
} from "@/components/prefs/PreferencesProvider";

export type Mode = "mail" | "calendar";
export type CalendarView = "day" | "week" | "month";
export type Theme = "system" | "light" | "dark";
/** Which pane the keyboard is driving. Mail's `j`/`k` belong to exactly one. */
export type MailFocus = "list" | "rail";

/** What the status bar says, and how loudly. */
export interface StatusMessage {
  message: string;
  /**
   * The inverse of whatever this message is reporting.
   *
   * It is no longer *how* undo works — the stack in `undo-stack.ts` is — but it
   * is still how an action that did not go through `run` gets onto that stack:
   * a status carrying an inverse is a claim that something reversible just
   * happened, and `dispatch` below records one when it sees it. The calendar's
   * write path and the plugin host both build these.
   *
   * A list when one gesture was several commands — a plugin action that labels
   * and then archives is one thing the user did, so it has to be one thing they
   * can undo. The commands are stored in the order they ran and applied in
   * reverse, because unarchiving before un-labelling would put the thread back
   * with the label still on it.
   */
  undo?: Command | Command[];
  /**
   * Which traversal button the toast should hold out beside this message.
   *
   * Almost nothing sets it. A message carrying an `undo` above is already a
   * claim that something reversible happened, so the toast offers ⌘Z on the
   * strength of that alone — see `offerFor` in `chrome/Toast.tsx`. This exists
   * for the one case the inverse cannot express: a message *produced by* a
   * traversal. "Undid archived 3 conversations" has no `undo` of its own to
   * carry, and the thing worth offering after it is ⇧⌘Z.
   */
  offer?: "undo" | "redo";
  /**
   * A button beside this message that is neither ⌘Z nor ⇧⌘Z.
   *
   * `offer` above names a *traversal*, and everything about how it renders —
   * its word, its sentence, its binding — is read back off the undo stack,
   * which is what keeps it from ever promising a step the stack no longer has.
   * That is the whole of what it can express, and one message needs more: an
   * unsubscribe the sender refused has one thing left to try, and it is not on
   * any stack. So the message carries the button itself.
   *
   * Rare, and it should stay rare. A status with a button of its own is a small
   * dialog in the corner of the window, and the reason there is one here is
   * that the alternative was a failure with nothing to do about it.
   */
  action?: StatusAction;
  tone: "info" | "error";
}

/** The button a {@link StatusMessage} carries, when it carries one. */
export interface StatusAction {
  /** The word on it — "Open page". */
  word: string;
  /** The whole sentence, for the accessible name and the tooltip. */
  title: string;
  run: () => void;
}

/**
 * How long a status message stays on screen.
 *
 * One clock and one preference. `undoWindowSeconds` answers "how long is the
 * offer good for", and the toast *is* that offer, so its lifetime is that
 * number and there is no second timing concept to keep in step with it.
 *
 * A failure is the one thing that gets longer, and it is a multiple of the same
 * preference rather than a constant of its own for exactly that reason. An
 * error is read twice — once to see that something went wrong, once to work out
 * what — and a message that leaves at the speed of a confirmation makes the
 * second reading impossible. Whatever window the user chose for a success, a
 * failure gets two of them.
 */
export const ERROR_HOLD = 2;

/**
 * The longest a toast sits on screen, however long undo stays offered.
 *
 * These were the same number, and that was wrong. "How long is undo available"
 * and "how long should a card cover the corner of the window" are different
 * questions that happen to have been answered by one preference, and the
 * default of twenty seconds is a reasonable answer to the first and an absurd
 * one to the second — long enough that the toast reads as stuck rather than
 * transient.
 *
 * Capping rather than replacing keeps the preference meaningful in the
 * direction it can still be right: someone who sets a *three second* undo
 * window has said something about how long they want to be interrupted, and
 * the toast should not outlive the offer it is making. Nothing is lost when it
 * goes — the stack never expires, ⌘Z keeps working, and the status bar goes on
 * naming what it would undo.
 */
export const TOAST_MAX_MS = 6_000;

export function statusLifetime(status: StatusMessage, undoWindow: number): number {
  const base = Math.min(undoWindow, TOAST_MAX_MS);
  return status.tone === "error" ? base * ERROR_HOLD : base;
}

/**
 * "Could not unsubscribe from Whiny Nil — Google refused".
 *
 * `describeResult` is the wrong sentence for this one: it counts ids, and an
 * unsubscribe addresses no row, so it would report "0 failed". What matters is
 * which list is still sending and why the request did not land.
 */
export function unsubscribeRefusal(result: CommandResult, sender: string): string {
  const failure = result.failed[0];
  const reason = failure?.message || FAILURE_LABELS[failure?.kind ?? "unexpected"];
  return `Could not unsubscribe from ${sender} — ${reason}`;
}

interface UiState {
  mode: Mode;
  calendarView: CalendarView;
  /** The day the calendar is anchored on; views derive their range from it. */
  anchor: number;
  accountId: AccountId | null;
  labelId: LabelId;
  threadId: ThreadId | null;
  /** Multi-select. `threadId` is the cursor; this is what a command acts on. */
  selection: Selection;
  /** Which mail pane has the keyboard, so `j`/`k` are never ambiguous. */
  focus: MailFocus;
  /** Cursor row in the rail. `-1` means "wherever the current mailbox is". */
  railIndex: number;
  eventId: EventId | null;
  paletteOpen: boolean;
  /**
   * How many modal surfaces are on screen. Zero means the app has the keyboard.
   *
   * Mirrored from the keymap's claim stack rather than reduced, because the
   * thing that knows a dialog is up is the dialog — `Overlay` claims the
   * keyboard as it opens and releases it as it closes, and every surface in the
   * app is an `Overlay`. There is deliberately no action for it: a reducer case
   * would be a second way to say a dialog is open, and a second way to get it
   * wrong. See `overlayOwnsKeyboard` for what reads it.
   */
  overlays: number;
  addAccountOpen: boolean;
  /**
   * The address `<AddAccountDialog/>` is signing in, when it was opened to
   * repair one rather than to connect a new account.
   *
   * Null for "Add account", which has nobody in mind. Set by "Sign in again"
   * beside a row that has lost its Keychain entry, and it is what makes that
   * button target *that* address: it becomes Google's `login_hint`, and Rust
   * refuses the handshake if a different account comes back.
   */
  addAccountEmail: string | null;
  /**
   * Whether the snooze picker is asking for a wake time.
   *
   * It has to be here rather than inside the picker because two other surfaces
   * open it — the reading pane's clock button and the ⌘K command — and neither
   * can reach into another component's `useState`.
   *
   * The threads it will act on are deliberately *not* stored alongside it. The
   * picker commits through the same `snoozeSelected` every other caller uses,
   * which reads the selection at the moment of the commit; opening an overlay
   * does not disturb the selection, so the set is the same one the user was
   * looking at when they pressed `b`.
   */
  snoozeOpen: boolean;
  listWidth: number;
  hiddenCalendars: CalendarId[];
  theme: Theme;
  /**
   * What has happened to a conversation that the loaded list does not say yet.
   *
   * This was three fields — a list of hidden ids, a list of read ids and a map
   * of stars — each written at a different call site, each with its own idea of
   * when it stopped applying, and only the last of them ever retired at all.
   * Label had none, and neither did the inverse a ⌘Z dispatches, so undoing a
   * star flashed for the same 600ms the star itself used to.
   *
   * One map now, keyed by thread, holding the label delta the command applied.
   * `lib/projection.ts` is where a command becomes one and where the list
   * agreeing with one retires it; the reducer here only stores what it is told.
   */
  guesses: Guesses;
  /**
   * The rows a command has spoken for, kept after the list drops them.
   *
   * A guess is a delta and needs something to land on. Everything the list
   * shows comes out of `list_threads`, so the instant a refetch stops carrying
   * an archived conversation there is no row for `{ add: [INBOX] }` to be
   * applied to — and a ⌘Z arriving after that repainted nothing and left the
   * user watching most of a second go by while the write and the next refetch
   * happened. Measured in the real window at 10ms to the row when the undo came
   * straight after the archive and 966ms when it came after the refetch,
   * against a 300ms command.
   *
   * So the list keeps its own copy of every row a command names, and
   * `returningRows` draws from it when a guess puts a conversation back into the
   * mailbox on screen. It is a memory of rows already shown rather than a second
   * store: the loaded list always wins where the two overlap, and a copy is only
   * ever on screen for the span between the keystroke and the refetch that makes
   * it real.
   *
   * Bounded, and in insertion order, which is why it is a `Map` — an object with
   * numeric keys iterates smallest-first and could not be trimmed oldest-first
   * at all.
   */
  remembered: Map<ThreadId, Thread>;
  /**
   * The list each guess was made against, as a version count.
   *
   * A guess retires when the loaded list agrees with it, and the list is always
   * a copy fetched at some point in the past — so a copy taken *before* the
   * command ran is not evidence about it. It agrees, when it agrees, because it
   * has not heard yet. See {@link settledGuesses}, which is where the rule is
   * written down and what the number is for.
   */
  guessedAt: Record<ThreadId, number>;
  /**
   * The same thing for events, and it exists for the same reason.
   *
   * The calendar had none of this: `rsvp`, `createEvent`, `updateEvent`,
   * `deleteEvent` and `moveEvent` all went out and the grid did not move until
   * Google had answered and the event window had been refetched. Answering
   * "Going" from the right-click menu changed nothing on screen at all.
   *
   * `lib/projection.ts` owns the rule for these exactly as it does for
   * conversations — what a command claims, how it lands on a row, and when the
   * store agreeing retires it. The reducer only stores what it is told.
   */
  eventGuesses: EventGuesses;
  /**
   * Blocks drawn for creates that are still in flight.
   *
   * Separate from `eventGuesses` because a create has no id to key a guess by:
   * the row it makes does not exist until the command layer has minted one. See
   * `placeholderEvent`.
   */
  pendingEvents: PendingEvent[];
  status: StatusMessage | null;
}

type UiAction =
  | { type: "mode"; mode: Mode }
  | { type: "calendarView"; view: CalendarView }
  | { type: "anchor"; anchor: number }
  | { type: "account"; accountId: AccountId | null }
  | { type: "label"; labelId: LabelId }
  /**
   * Move the cursor. `keepAnchor` is what ⇧J/⇧K set: the cursor moves but the
   * range being dragged still grows from where shift was first pressed.
   */
  | { type: "thread"; threadId: ThreadId | null; keepAnchor?: boolean }
  | { type: "selection"; selection: Selection }
  | { type: "focus"; focus: MailFocus }
  | { type: "railIndex"; index: number }
  | { type: "event"; eventId: EventId | null }
  | { type: "palette"; open: boolean }
  | { type: "addAccount"; open: boolean; email?: string | null }
  | { type: "snooze"; open: boolean }
  | { type: "listWidth"; width: number }
  | { type: "toggleCalendar"; calendarId: CalendarId }
  | { type: "theme"; theme: Theme }
  /**
   * Show what a command did, before the list has been refetched.
   *
   * `rows` are the loaded rows the guess names, which the reducer keeps so a
   * later guess about the same conversation has something to land on once the
   * list has dropped it. See `UiState.remembered`. `listVersion` is the list it
   * was made against — see `UiState.guessedAt`.
   */
  | {
      type: "project";
      guesses: Guesses;
      rows?: readonly Thread[];
      listVersion?: number;
    }
  /**
   * Drop guesses — the list agrees now, or the write was refused.
   *
   * `settled` is the guess each id was judged on, and an id is dropped only if
   * that is still the guess standing for it. Without it, an id is enough to
   * delete a guess made *after* the judgement: the settle effect runs from a
   * committed render, React flushes it at the head of the next one, and a ⌘Z
   * pressed in that gap had its own guess deleted by the archive's settling.
   * Measured in the real window — the undo's `{ add: ["INBOX"] }` never reached
   * a paint. Omitted by the failure path and by a traversal retracting the
   * previous guess, both of which mean "whatever is there, drop it".
   */
  | { type: "forget"; threadIds: ThreadId[]; settled?: Guesses }
  /** Show what a calendar command did, before the event window was refetched. */
  | { type: "projectEvents"; guesses: EventGuesses }
  /** Draw a block for a create that has not come back yet. */
  | { type: "pendEvent"; event: CalendarEvent }
  /** The command layer minted a row id for a pending create. */
  | { type: "resolvePending"; eventId: EventId; realId: EventId | null }
  /**
   * Drop event guesses and pending blocks — the store agrees now, or the write
   * was refused. One action for both because a refusal has to take back
   * whichever of the two the command produced, and the caller should not have
   * to know which that was.
   */
  | { type: "forgetEvents"; eventIds: EventId[] }
  | { type: "status"; status: UiState["status"] };

export const initialUi: UiState = {
  mode: "mail",
  calendarView: "week",
  anchor: Date.now(),
  accountId: null,
  labelId: "INBOX",
  threadId: null,
  selection: emptySelection,
  focus: "list",
  railIndex: -1,
  eventId: null,
  paletteOpen: false,
  overlays: 0,
  addAccountOpen: false,
  addAccountEmail: null,
  snoozeOpen: false,
  listWidth: 520,
  hiddenCalendars: [],
  theme: "system",
  guesses: {},
  remembered: new Map(),
  guessedAt: {},
  eventGuesses: {},
  pendingEvents: [],
  status: null,
};

/**
 * How many dropped rows are kept for a guess to land on.
 *
 * The undo stack is fifty deep and one entry can be a bulk archive, so this is
 * not "fifty rows". It is a few screens' worth of conversations, which is what
 * a triage session actually produces, and a thread row is a subject, a snippet
 * and a handful of participants — not the messages, which the reading pane
 * fetches by id.
 */
const REMEMBERED = 500;

/**
 * Adds rows to the memory, newest last, and drops the oldest over the bound.
 *
 * A row already remembered is moved to the end rather than left where it was: a
 * conversation acted on twice is the one most likely to be acted on again, and
 * the newer copy is the more accurate one.
 */
function remember(
  previous: Map<ThreadId, Thread>,
  rows: readonly Thread[] | undefined,
): Map<ThreadId, Thread> {
  if (!rows || rows.length === 0) return previous;
  const next = new Map(previous);
  for (const row of rows) {
    next.delete(row.id);
    next.set(row.id, row);
  }
  if (next.size > REMEMBERED) {
    let overflow = next.size - REMEMBERED;
    for (const id of next.keys()) {
      if (overflow-- <= 0) break;
      next.delete(id);
    }
  }
  return next;
}

/** The loaded rows a set of guesses is about, for the reducer to remember. */
function namedRows(guesses: Guesses, rows: readonly Thread[]): Thread[] {
  return rows.filter((row) => guesses[row.id] !== undefined);
}

/**
 * Puts a promise in `pending` and hands back the function that settles it.
 *
 * A `Promise.withResolvers` this project's target does not have yet. Callers
 * use it to say "something is outstanding" without holding the resolver and the
 * set membership apart, which is the pair that goes wrong.
 */
function awaitable(pending: Set<Promise<void>>): () => void {
  let settle = () => {};
  const promise = new Promise<void>((resolve) => {
    settle = resolve;
  });
  pending.add(promise);
  return () => {
    pending.delete(promise);
    settle();
  };
}

export function uiReducer(state: UiState, action: UiAction): UiState {
  switch (action.type) {
    case "mode":
      return { ...state, mode: action.mode };
    case "calendarView":
      return { ...state, calendarView: action.view };
    case "anchor":
      return { ...state, anchor: action.anchor };
    // Changing what the list is *of* invalidates anything selected in it.
    case "account":
      return {
        ...state,
        accountId: action.accountId,
        threadId: null,
        selection: emptySelection,
      };
    case "label":
      return { ...state, labelId: action.labelId, threadId: null, selection: emptySelection };
    case "thread":
      return {
        ...state,
        threadId: action.threadId,
        // Opening a conversation is a claim that it has been read, made before
        // the `markRead` command it will produce has run — and made through the
        // same map that command's own guess lands in, so the two cannot
        // disagree and the reading of it is retired by the one rule.
        guesses:
          action.threadId !== null && !(action.threadId in state.guesses)
            ? { ...state.guesses, [action.threadId]: READ_GUESS }
            : state.guesses,
        // Moving the cursor re-points the anchor without selecting anything:
        // walking past a row you ticked must never untick it.
        selection:
          action.threadId === null || action.keepAnchor
            ? state.selection
            : reanchor(state.selection, action.threadId),
      };
    case "selection":
      return { ...state, selection: action.selection };
    case "focus":
      return {
        ...state,
        focus: action.focus,
        // Leaving the rail forgets where the cursor was in it, so coming back
        // starts on the mailbox you are actually in rather than a stale row.
        railIndex: action.focus === "rail" ? state.railIndex : -1,
      };
    case "railIndex":
      return { ...state, railIndex: action.index };
    case "event":
      return { ...state, eventId: action.eventId };
    case "palette":
      return { ...state, paletteOpen: action.open };
    case "addAccount":
      return {
        ...state,
        addAccountOpen: action.open,
        // Cleared on close, so the next "Add account" cannot inherit the
        // address the last repair was for.
        addAccountEmail: action.open ? (action.email ?? null) : null,
        paletteOpen: false,
      };
    // Closing the palette on the way, for the same reason `addAccount` does:
    // the ⌘K command that opens this is chosen *from* the palette, and two
    // overlays stacked on each other is a surface nobody asked for.
    case "snooze":
      return { ...state, snoozeOpen: action.open, paletteOpen: false };
    case "listWidth":
      return { ...state, listWidth: clamp(action.width, 280, 640) };
    case "toggleCalendar":
      return {
        ...state,
        hiddenCalendars: state.hiddenCalendars.includes(action.calendarId)
          ? state.hiddenCalendars.filter((id) => id !== action.calendarId)
          : [...state.hiddenCalendars, action.calendarId],
      };
    case "theme":
      return { ...state, theme: action.theme };
    // A later guess about the same conversation replaces the earlier one
    // outright rather than merging with it. Two commands in a row are two
    // statements about the same thread, and the second is the current one —
    // starring and then archiving must not leave the star's delta behind to be
    // re-applied to a row that has since been refetched.
    case "project": {
      const guessedAt = { ...state.guessedAt };
      for (const id of Object.keys(action.guesses)) {
        guessedAt[Number(id)] = action.listVersion ?? -1;
      }
      return {
        ...state,
        guesses: { ...state.guesses, ...action.guesses },
        guessedAt,
        remembered: remember(state.remembered, action.rows),
      };
    }
    case "forget": {
      const settled = action.settled;
      const going = action.threadIds.filter(
        (id) => id in state.guesses && (!settled || state.guesses[id] === settled[id]),
      );
      if (going.length === 0) return state;
      const next = { ...state.guesses };
      const guessedAt = { ...state.guessedAt };
      for (const id of going) {
        delete next[id];
        delete guessedAt[id];
      }
      return { ...state, guesses: next, guessedAt };
    }
    // Same rule as `project` above: a later claim about one event replaces the
    // earlier one outright. Dragging a block and then answering "Going" are two
    // statements about it, and the second is the current one.
    case "projectEvents":
      return { ...state, eventGuesses: { ...state.eventGuesses, ...action.guesses } };
    case "pendEvent":
      return {
        ...state,
        pendingEvents: [...state.pendingEvents, { event: action.event, realId: null }],
      };
    case "resolvePending": {
      const index = state.pendingEvents.findIndex((p) => p.event.id === action.eventId);
      if (index === -1) return state;
      const pendingEvents = [...state.pendingEvents];
      pendingEvents[index] = { ...pendingEvents[index]!, realId: action.realId };
      return { ...state, pendingEvents };
    }
    case "forgetEvents": {
      const ids = new Set(action.eventIds);
      const hitGuess = action.eventIds.some((id) => id in state.eventGuesses);
      const pendingEvents = state.pendingEvents.filter((p) => !ids.has(p.event.id));
      if (!hitGuess && pendingEvents.length === state.pendingEvents.length) return state;
      const eventGuesses = { ...state.eventGuesses };
      for (const id of action.eventIds) delete eventGuesses[id];
      return { ...state, eventGuesses, pendingEvents };
    }
    case "status":
      return { ...state, status: action.status };
  }
}

/**
 * True while a dialog is up, and therefore while no mode binding may fire.
 *
 * The one place the rule is written down, so that "is anything covering the
 * list?" is a question with a single answer rather than one `!ui.somethingOpen`
 * per surface, appended to as dialogs are added and forgotten as often as not.
 * The keymap enforces the same rule underneath (see `claimKeyboard`); this is
 * how a mode says it out loud, and what the archive-behind-a-dialog regression
 * test holds on to.
 */
export function overlayOwnsKeyboard(ui: Pick<UiState, "overlays">): boolean {
  return ui.overlays > 0;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

interface MachValue {
  ui: UiState;
  accounts: Account[];
  labels: Label[];
  calendars: Calendar[];
  allThreads: Thread[];
  visibleThreads: Thread[];
  visibleEvents: CalendarEvent[];
  events: CalendarEvent[];
  detail: ThreadDetail | null;
  detailLoading: boolean;
  /**
   * The store's address book, or `[]` until it arrives.
   *
   * Empty is a real and usable state — `useContacts` still has the open
   * conversation and the loaded list to complete from — which is what lets the
   * composer open and take typing before a scan of forty thousand messages has
   * finished.
   */
  addressBook: Contact[];
  /** What the user pinned to the sidebar, in the order they pinned it. */
  favorites: Favorite[];
  /** The mailbox-plus-account-scope favorite the current view would produce. */
  viewFavorite: Favorite;
  /** The conversation favorite the open thread would produce, if any. */
  threadFavorite: Favorite | null;
  isFavorite: (favorite: Favorite | null) => boolean;
  selectedIndex: number;
  /** The ids the next command will act on: the selection, or the cursor row. */
  commandTargets: ThreadId[];
  isRowSelected: (id: ThreadId) => boolean;
  selectedEvent: CalendarEvent | null;
  /** Which of the four empty/loading situations the mail pane is in. */
  state: MailboxState;
  sync: SyncStatus | null;
  progress: SyncProgress;
  /** `true` when the app is talking to Rust, `false` on fixture data. */
  live: boolean;
  hasMore: boolean;
  loadingMore: boolean;
  /**
   * Everything ⌘Z and ⇧⌘Z could do, so the status bar can say which.
   *
   * Deliberately the whole stack rather than a pre-rendered string: the status
   * bar is not the only thing that will want to ask, and `describeUndo` is the
   * one place that decides how an entry reads.
   */
  undoState: UndoState;
  accountById: (id: AccountId) => Account | undefined;
  calendarById: (id: CalendarId) => Calendar | undefined;
  isUnread: (thread: Thread) => boolean;
  dispatch: (action: UiAction) => void;
  actions: MachActions;
}

export interface MachActions {
  setMode: (mode: Mode) => void;
  toggleMode: () => void;
  setCalendarView: (view: CalendarView) => void;
  moveCursor: (delta: number) => void;
  openSelected: () => void;
  closeThread: () => void;
  selectThread: (id: ThreadId) => void;
  /** A click on a row, told which modifiers were held. */
  clickThread: (id: ThreadId, modifiers: { extend: boolean; toggle: boolean }) => void;
  /** `x`: tick the cursor row and move on. */
  toggleAtCursor: () => void;
  /** ⇧J / ⇧K: move the cursor and drag the range with it. */
  extendCursor: (delta: number) => void;
  /** ⌘A: select every loaded conversation, or clear if they all already are. */
  selectAllThreads: () => void;
  clearSelection: () => void;
  archiveSelected: () => void;
  trashSelected: () => void;
  /**
   * `!` — Gmail's Report spam, over the same selection every other triage verb
   * acts on. No confirmation, for the same reason trash has none: the inverse
   * is exact and it goes on the stack ⌘Z reads.
   */
  reportSpamSelected: () => void;
  /**
   * ⌘⇧U — ask the sender of the open conversation to stop.
   *
   * It takes no argument, and that is the design rather than a convenience.
   * Which message of a conversation carries the usable `List-Unsubscribe` is a
   * real question with a real answer — see `unsubscribeAction` in
   * `components/mail/unsubscribe-offer.ts` — and if the reading pane's button,
   * the key and the ⌘K entry each answered it for themselves, three surfaces
   * would eventually disagree about which sender they were writing to.
   *
   * Three outcomes, and the offer Rust computed decides which:
   *
   *  * `reportSpam` — the header is there and nothing vouches for the sender,
   *    so the honest gesture is Gmail's spam report. Nothing is unsubscribed
   *    from; an unsubscribe would confirm to a stranger that the address is
   *    read.
   *  * `unsubscribe` by `link` — a page only a person can complete, opened
   *    from Rust in Mach's own page window. No command is dispatched.
   *  * `unsubscribe` by `oneClick` or `mail` — the conversation is archived
   *    (which is the half ⌘Z can take back) and the request goes out behind it.
   *
   * The request itself is not awaited. It has no inverse and it is somebody
   * else's server, so making the keyboard wait for it would be paying a
   * stranger's latency for an action whose whole point is to stop thinking
   * about them.
   */
  unsubscribe: () => void;
  /**
   * The same page, in the browser he already trusts.
   *
   * `unsubscribe` shows a `link` offer inside Mach, in a window that has no
   * capability grant and an empty cookie jar — which is right for reading a
   * form and wrong for a page that wants him signed in first. This is the way
   * out of that, and it is a separate entry rather than a modifier because it
   * is the answer to a question ("it wants me to log in") rather than a
   * variation on a gesture.
   */
  unsubscribePageInBrowser: () => void;
  starSelected: () => void;
  /**
   * Snooze to a named instant.
   *
   * The instant is a parameter and has no default on purpose. This used to
   * take none and commit `Date.now() + DAY`, which meant the answer to "when
   * does this come back" depended on the second the key was pressed and was
   * never shown to anybody. Every caller now goes through the picker, and the
   * picker resolves the instant before it calls this.
   */
  snoozeSelected: (until: number) => void;
  replySelected: () => void;
  /** Move the keyboard between the rail and the list. */
  setFocus: (focus: MailFocus) => void;
  toggleFocus: () => void;
  /**
   * Put the owner in front of something the agent made.
   *
   * The routing half of the artifact seam: the drawer knows *what* was made,
   * this knows where that lives. A draft navigates to its conversation and
   * asks the composer to resume it — through the same `mach:compose` event the
   * reading pane's reply button uses, because the composer owns the draft and
   * this hook does not.
   */
  openArtifact: (artifact: Artifact) => void;
  /** ⌘Z, and `z`. Takes back the last recorded action, however long ago. */
  undo: () => void;
  /** ⇧⌘Z. Re-applies the last thing undo took back. */
  redo: () => void;
  /**
   * Record several inverses as one undoable step.
   *
   * The plugin host calls this: it collects the inverse of every `mach.run` an
   * action made and hands them over as a group labelled with the action's
   * title. A plugin never constructs an inverse — the command layer already
   * returned the exact one.
   */
  pushUndoGroup: (label: string, inverses: Command[]) => void;
  shiftPeriod: (delta: number) => void;
  goToday: () => void;
  setPalette: (open: boolean) => void;
  /** `email` repairs that address; without one, it connects a new account. */
  setAddAccount: (open: boolean, email?: string | null) => void;
  /** Open or shut the snooze picker. `b`, the clock button and ⌘K all call it. */
  setSnooze: (open: boolean) => void;
  setStatus: (message: string, tone?: StatusMessage["tone"]) => void;
  /** Pin or unpin the mailbox being looked at, account scope included. */
  toggleFavoriteView: () => void;
  /** Pin or unpin the open conversation. */
  toggleFavoriteThread: () => void;
  /** What `f` does: the conversation if one is open, otherwise the mailbox. */
  toggleFavoriteFocused: () => void;
  unfavorite: (key: string) => void;
  openFavorite: (favorite: Favorite) => void;
  cycleTheme: () => void;
  loadMore: () => void;
  /** Sync now. With no argument, every account; with one, that account alone. */
  syncNow: (accountId?: AccountId) => void;
  /** After an account is added or removed, everything is stale. */
  reload: () => void;
  /**
   * A draft has been queued for sending, so it is no longer a draft.
   *
   * `⌘⏎` is local — build the bytes, write one row, drop the draft — and it is
   * still a write, and SQLite takes one writer at a time. Under a sync pass that
   * write can wait seconds for the lock, and the conversation used to wait with
   * it: the reply appeared, the `DRAFT` row of the same words stayed above it,
   * and the owner could not tell whether he had sent anything. So the row goes
   * now, on the same frame as the composer closing, and the refetch behind it
   * confirms rather than causes.
   *
   * Its inverse is {@link draftRecalled}, for `⌘Z` and for a queue that failed.
   */
  draftSent: (draftId: string) => void;
  /** The draft is a draft again — undo, or the queue write refusing. */
  draftRecalled: (draftId: string) => void;
  /**
   * Dispatch one command through the app's only write path.
   *
   * The calendar had its own — `CalendarMode` executed against the data source
   * directly — and that is why none of its commands was optimistic: the guess
   * is made here, before the first `await`, and a caller that goes around this
   * goes around that. It also loses the undo stack's better entry, which holds
   * the original command rather than only the inverse a status message can
   * carry.
   *
   * Resolves with what the command layer said, or `null` when it was never
   * reached. `onRefused` is for a caller with somewhere better than the status
   * rail to put a failure.
   */
  execute: (
    command: Command,
    options?: { onRefused?: (failure: { message: string; command: Command }) => void },
  ) => Promise<CommandResult | null>;
  /**
   * Refetch just the calendar window.
   *
   * A calendar write changes one row. `reload()` answers that by refetching
   * accounts, labels, calendars, the sync snapshot *and* the whole thread list,
   * which is a lot of work and a visible stutter for redrawing one block.
   */
  reloadEvents: () => void;
}

const MachContext = createContext<MachValue | null>(null);

/** How far either side of the anchor the calendar keeps events loaded. */
const CALENDAR_WINDOW = 60 * DAY;

export function MachProvider({ children }: { children: ReactNode }) {
  // The reducer's own dispatch. Everything outside this file — and almost
  // everything inside it — goes through the recording `dispatch` defined below
  // instead; this one is what that wrapper calls, and what `run` uses to avoid
  // recording an action twice.
  const [ui, dispatchUi] = useReducer(uiReducer, initialUi);
  /*
   * The live state, for the handful of callbacks that must read it without
   * being rebuilt when it changes.
   *
   * `run` and the undo host are both memoised on nothing that moves, which is
   * what keeps every action in the app from being rebuilt sixty rows at a time
   * — and both of them have to know where the cursor is. A ref is the only way
   * to have both.
   */
  const uiRef = useRef(ui);
  uiRef.current = ui;
  /** Where the list stands: the cursor, the ticked rows, and the mailbox both are in. */
  const livePlace = useCallback(
    (): UndoPlace => ({
      threadId: uiRef.current.threadId,
      selection: uiRef.current.selection,
      labelId: uiRef.current.labelId,
      accountId: uiRef.current.accountId,
    }),
    [],
  );
  /*
   * The open-dialog count, read off the registry the dialogs claim.
   *
   * Subscribed rather than polled because a gate written as `when: () => …` is
   * only re-read when the component that declared it renders, and a dialog
   * opening two levels away renders nothing here on its own. Every consumer of
   * this hook already re-renders together, so one subscription puts the whole
   * app's gates back in step at once.
   */
  const keymap = useKeymap();
  const overlays = useSyncExternalStore(keymap.subscribe, keymap.claims, keymap.claims);
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [labels, setLabels] = useState<Label[]>([]);
  const [calendars, setCalendars] = useState<Calendar[]>([]);
  const [events, setEvents] = useState<CalendarEvent[]>([]);
  const [detail, setDetail] = useState<ThreadDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  /**
   * Drafts that have been queued and whose mirror row the store has not dropped
   * yet — the same guess `lib/projection.ts` makes about a list row, made about
   * a message inside the open conversation. See `draftSent`.
   */
  const [sentDrafts, setSentDrafts] = useState<ReadonlySet<string>>(() => new Set());
  const [addressBook, setAddressBook] = useState<Contact[]>([]);
  const [sync, setSync] = useState<SyncStatus | null>(null);
  const [booted, setBooted] = useState(false);
  const [bootError, setBootError] = useState<MailboxError | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  /** Bumped by anything that writes an event — see `reloadEvents`. */
  const [eventsKey, setEventsKey] = useState(0);
  /**
   * Bumped when a background write may have changed the open conversation.
   *
   * Separate from `reloadKey` because it costs one local `get_thread` rather
   * than refetching accounts, labels, calendars and the whole thread list.
   */
  const [detailKey, setDetailKey] = useState(0);
  // Favorites are the one piece of state here the user owns, so they outlive
  // the window. Read once at mount, written back whenever they change.
  const [favorites, setFavorites] = useState<Favorite[]>(() => loadFavorites());
  useEffect(() => saveFavorites(favorites), [favorites]);

  /*
   * The undo stack.
   *
   * Kept in a ref *and* in state. The ref is the truth, because ⌘Z has to be
   * correct when it arrives twice for one press — a key held down, or the
   * macOS menu replaying a token behind the real keystroke — and two handlers
   * reading the same not-yet-rendered React state would both pop the same
   * entry and dispatch its inverse twice. The state exists so the status bar
   * re-renders when the stack changes.
   */
  const undoRef = useRef<UndoState>(emptyUndo());
  const [undoState, setUndoState] = useState<UndoState>(undoRef.current);
  const commitUndo = useCallback((next: UndoState) => {
    undoRef.current = next;
    setUndoState(next);
  }, []);

  /**
   * The actions whose undo entry has not been recorded yet.
   *
   * An entry is made from the command layer's answer — `result.undo` is the
   * exact inverse, and nothing here guesses at one — so it does not exist until
   * the round trip is over. For that whole span ⌘Z popped an empty stack and
   * returned: `handled: true` from the keymap, and nothing on screen, nothing
   * said. Reproduced against a 5s command layer in the real window — archive,
   * ⌘Z 300ms later, and the marks show `undo:start` followed by nothing at all.
   *
   * A traversal therefore waits for these before it reads the stack. It is not
   * a lock: the set is empty in the ordinary case and {@link traverse} keeps the
   * synchronous path for that, because `runUndo` putting the whole undo on
   * screen in the tick the keystroke produced is the thing being protected.
   */
  const unrecorded = useRef(new Set<Promise<void>>());

  /**
   * The dispatch everything outside `run` uses — and the seam where an action
   * that happened somewhere else gets onto the undo stack.
   *
   * A status message carrying an inverse is a claim that something reversible
   * just happened, and there are two things that make one without going
   * anywhere near `run`: the calendar's write path, which dispatches its own
   * status so that a drag reverses as readily as an archive, and the plugin
   * host, which hands over a whole action's worth of inverses as one group.
   * Watching this one action is what covers both without a `pushUndo` call
   * sitting next to every command in the app.
   *
   * `run` deliberately does *not* come through here. It holds the original
   * command and the result, so it records a better entry than a status message
   * can carry — and coming through both doors would record it twice.
   */
  const dispatch = useCallback(
    (action: UiAction) => {
      if (action.type === "status" && action.status?.undo) {
        commitUndo(
          recordUndo(undoRef.current, action.status.message, action.status.undo, Date.now()),
        );
      }
      dispatchUi(action);
    },
    [commitUndo],
  );

  const stream = useThreadStream(ui.accountId, ui.labelId);
  // Actions close over the stream without being rebuilt on every page it loads.
  const streamRef = useRef(stream);
  streamRef.current = stream;
  /*
   * The loaded rows, for `run` to build a guess against.
   *
   * A ref rather than a dependency, so `run` — and therefore every action built
   * on it — is not rebuilt on each of the sixty-row pages the stream appends.
   * Only two commands read it at all: `unarchive` and `untrash` carrying the
   * exact label set undo restores, which has to be diffed against what the row
   * has now to become a delta.
   */
  const threadsRef = useRef(stream.threads);
  threadsRef.current = stream.threads;

  /*
   * A backfill can emit `threads-changed` hundreds of times a minute. Coalesce:
   * one refetch per window, however many events arrived in it.
   *
   * **The open conversation is refetched too.** It was not, and the gap is what
   * "I clicked discard but the draft still shows" looked like: this event was
   * wired to the list only, so anything that changed the thread on screen —
   * a sync pass removing a draft, another window, the agent — repainted the
   * list beside a reading pane still showing the old messages. The list has
   * always refetched here; the pane was the half nobody told.
   *
   * **This window is no longer in front of a keystroke.** It used to be: a
   * command emitted `threads-changed` on its way out, and anything the frontend
   * had not guessed at — a label, the inverse a ⌘Z dispatched — waited out these
   * 600ms and then a `list_threads` round trip before it showed. What is left
   * for the timer to do is what it was for: coalescing a backfill that emits
   * hundreds of these a minute, and giving a guess something to be retired
   * against. Both are background work, and a refetch that carries no change now
   * costs nothing to apply — `reconcile` in `useThreadStream` keeps the object
   * for every row that did not move, so the list does not re-render.
   */
  const refreshTimer = useRef<number | null>(null);
  const scheduleRefresh = useCallback(() => {
    if (refreshTimer.current !== null) return;
    refreshTimer.current = window.setTimeout(() => {
      refreshTimer.current = null;
      streamRef.current.refresh();
      setDetailKey((k) => k + 1);
    }, 600);
  }, []);
  useEffect(
    () => () => {
      if (refreshTimer.current !== null) window.clearTimeout(refreshTimer.current);
    },
    [],
  );

  // Boot. Accounts, labels, calendars and the current sync snapshot; the thread
  // list has its own pager, and the calendar its own window.
  useEffect(() => {
    let live = true;
    void (async () => {
      const source = getDataSource();
      try {
        const [a, l, c, s] = await Promise.all([
          source.listAccounts(),
          source.listLabels(),
          source.listCalendars(),
          source.syncStatus(),
        ]);
        if (!live) return;
        setAccounts(a);
        // The one seam where the label list is built, so the rail, ⌘K, the
        // favorites and the list header cannot disagree about which mailboxes
        // exist. See `withVirtualMailboxes`.
        setLabels(withVirtualMailboxes(l));
        setCalendars(c);
        setSync(s);
        setBootError(null);
      } catch (caught) {
        if (!live) return;
        setBootError(toMailboxError(caught));
      } finally {
        if (live) setBooted(true);
      }
    })();
    return () => {
      live = false;
    };
  }, [reloadKey]);

  /*
   * The address book, once.
   *
   * Its own effect rather than a fifth `Promise.all` member in boot above,
   * because it is the one read here that scans the whole `messages` table and
   * nothing may wait on it: `booted` gates the first paint, and a composer that
   * would not open until every message had been read is a worse bug than the
   * one this fixes. It arrives when it arrives, `useContacts` merges it, and
   * completion gets better mid-session without anything on screen moving.
   *
   * Reloaded with `reloadKey` — adding or removing an account changes whose
   * address book this is.
   */
  useEffect(() => {
    let live = true;
    void getDataSource()
      .listContacts()
      .then((rows) => {
        if (live) setAddressBook(rows);
      })
      .catch(() => {
        // Nothing to report and nothing to act on. Completion falls back to
        // the conversation and the list, which is what it was before this
        // read existed; a store broken enough to fail here has already put
        // its error on screen through `bootError`.
      });
    return () => {
      live = false;
    };
  }, [reloadKey]);

  // Calendar events for a window around the anchor, refetched when the anchor
  // leaves it rather than on every arrow key.
  const windowKey = Math.round(ui.anchor / (30 * DAY));
  useEffect(() => {
    let live = true;
    const centre = windowKey * 30 * DAY;
    void getDataSource()
      .listEvents({ start: centre - CALENDAR_WINDOW, end: centre + CALENDAR_WINDOW })
      .then((rows) => {
        if (live) setEvents(rows);
      })
      .catch(() => {
        /* the mail half must not go dark because the calendar failed */
      });
    return () => {
      live = false;
    };
  }, [windowKey, reloadKey, eventsKey]);

  // Push, not poll: the sync engine tells us where it is, and a sync pass or a
  // command that changed threads tells us to refetch the list.
  useEffect(() => {
    let live = true;
    const disposers: (() => void)[] = [];
    const source = getDataSource();
    const keep = (off: () => void) => {
      if (live) disposers.push(off);
      else off();
    };

    void source.onSyncStatus(setSync).then(keep);
    void source.onThreadsChanged(scheduleRefresh).then(keep);

    /*
     * A snooze that came due and could not be woken.
     *
     * A conversation that *does* wake says nothing — it is simply back at the
     * top of the inbox, which is what was asked for and what Gmail does. One
     * that could not be is the opposite case: it is still hidden, still
     * labelled, and nothing on screen would ever say so. It goes on the status
     * line in the error tone, with no undo attached, because there is nothing
     * to take back.
     */
    void source
      .onWakeFailed((failure) =>
        dispatchUi({
          type: "status",
          status: { message: describeWakeFailure(failure), tone: "error" },
        }),
      )
      .then(keep);

    /*
     * Coming back from a notification banner. Mail mode first, then the
     * account, then the conversation — in that order, because selecting a
     * thread that the current account filter excludes would select nothing.
     *
     * `connectNotificationOpen` is synchronous and returns its own disposer,
     * so it is not a `keep()` candidate like the two above.
     */
    disposers.push(
      connectNotificationOpen((target) => {
        dispatch({ type: "mode", mode: "mail" });
        dispatch({ type: "account", accountId: target.accountId });
        dispatch({ type: "thread", threadId: target.threadId });
      }),
    );

    return () => {
      live = false;
      for (const off of disposers) off();
    };
  }, [reloadKey, scheduleRefresh]);

  // Which conversation the pane is currently showing, so a refetch of the same
  // one can be silent. A ref rather than state: nothing renders from it.
  const shownThread = useRef<ThreadId | null>(null);
  useEffect(() => {
    if (ui.threadId === null) {
      shownThread.current = null;
      setDetail(null);
      setDetailLoading(false);
      return;
    }
    let live = true;
    // Only a *different* conversation is worth a loading state. A background
    // refetch of the one already on screen must not blank it and put it back
    // — the pane would flash on every sync pass.
    if (shownThread.current !== ui.threadId) setDetailLoading(true);
    shownThread.current = ui.threadId;
    void getDataSource()
      .getThread(ui.threadId)
      .then((d) => {
        if (!live) return;
        setDetail(d);
        setDetailLoading(false);
        // A guess stops being a guess when the store agrees with it. Retiring on
        // the refetch rather than on a timer is what keeps ⌘Z honest: while the
        // row is still there, the guess is still hiding it.
        setSentDrafts((current) => {
          if (current.size === 0) return current;
          const present = new Set(
            (d?.messages ?? [])
              .map((message) => message.machDraftId)
              .filter((id): id is string => Boolean(id)),
          );
          const next = new Set([...current].filter((id) => present.has(id)));
          return next.size === current.size ? current : next;
        });
      })
      .catch(() => {
        if (!live) return;
        setDetail(null);
        setDetailLoading(false);
      });
    return () => {
      live = false;
    };
    // `reloadKey` is a dependency because sending writes an optimistic copy of
    // the reply straight into SQLite: the open conversation is exactly the
    // thing a reload has to refetch. `detailKey` is the same claim made by a
    // background write — a sync pass, another window — through
    // `threads-changed`.
  }, [ui.threadId, reloadKey, detailKey]);

  /*
   * The theme is a preference; `ui.theme` mirrors it.
   *
   * Two things read the live theme and neither goes through this state — the
   * `.dark` class below, and `use-is-dark.ts`, which watches that class to
   * build the calendar's eight hues. Unifying them on the *preference* rather
   * than on this field is what makes ⌘, ⌘K and the `t` key all mean the same
   * thing; keeping `ui.theme` as a mirror is what keeps every existing reader
   * of it working.
   */
  const prefs = usePreferences();
  useEffect(() => {
    if (prefs.theme !== ui.theme) dispatch({ type: "theme", theme: prefs.theme });
  }, [prefs.theme, ui.theme]);

  // Theme. The token layer defines both palettes; this decides which is live.
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const dark = ui.theme === "dark" || (ui.theme === "system" && media.matches);
      document.documentElement.classList.toggle("dark", dark);
      document.documentElement.style.colorScheme = dark ? "dark" : "light";
    };
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [ui.theme]);

  /*
   * Status messages are transient; the undo window is the only reason they
   * linger — so the preference that names that window is what times them out.
   *
   * It used to be a hardcoded six seconds, which was wrong for what it guards:
   * `bulk()` can archive every selected conversation in one keystroke, and the
   * only affordance for taking that back was this message. Six seconds is not
   * long enough to register that fifty rows just left.
   *
   * It is no longer the only affordance — the stack does not expire, and ⌘Z
   * still reaches an archive from an hour ago — and the two are not in tension.
   * This message is the *offer*: the loud, in-the-eye-line "that just happened,
   * and here is the button", which `chrome/Toast.tsx` renders as a toast above
   * the status bar. `undoWindowSeconds` says how long that offer stands, which
   * is a question about attention rather than about capability. When it lapses
   * the status bar goes back to naming what ⌘Z would still do, quietly. The
   * preference shortens the shout, never the memory.
   *
   * The clock lives here rather than in the toast on purpose: one message, one
   * owner. The toast reads `ui.status` and nothing else, so a message dismissed
   * by hand and a message that timed out are the same event to it.
   */
  const undoWindow = undoWindowMs(prefs);
  useEffect(() => {
    if (!ui.status) return;
    const timer = window.setTimeout(
      () => dispatchUi({ type: "status", status: null }),
      statusLifetime(ui.status, undoWindow),
    );
    return () => window.clearTimeout(timer);
  }, [ui.status, undoWindow]);

  const allThreads = stream.threads;

  /*
   * How many times `list_threads` has actually changed the list.
   *
   * What a guess is stamped with, so that a list fetched *before* the command
   * cannot be what retires its guess — see `UiState.guessedAt`. Counted rather
   * than timed because `reconcile` already answers "did anything move" exactly:
   * it returns the previous array when the refetch carried no change, so
   * identity is the whole test and a sync pass that changed nothing does not
   * make a guess decidable.
   */
  const listVersion = useRef(0);
  const lastList = useRef(allThreads);
  if (lastList.current !== allThreads) {
    lastList.current = allThreads;
    listVersion.current += 1;
  }

  /*
   * Retire a guess when the list catches up with it.
   *
   * The other half of never dropping one on a clock. A guess has to stop being
   * one at some point, or a star turned off later — on the phone, in another
   * window, by a filter — would be held out of the row forever by something
   * nobody remembers guessing, and a conversation unarchived elsewhere would
   * stay invisible here until the app was relaunched. `settledGuesses` owns the
   * rule and says what "the list agrees" means for each of a guess's two
   * claims; this only reports the answer to the reducer.
   */
  useEffect(() => {
    const settled = settledGuesses(
      allThreads,
      ui.guesses,
      ui.labelId,
      ui.guessedAt,
      listVersion.current,
    );
    // `settled: ui.guesses` — the ids alone would let this delete a guess made
    // between the judgement and the dispatch. See the `forget` action.
    if (settled.length > 0) {
      dispatchUi({ type: "forget", threadIds: settled, settled: ui.guesses });
    }
  }, [allThreads, ui.guesses, ui.labelId, ui.guessedAt]);

  /*
   * The list, with every outstanding guess projected onto it.
   *
   * Two things happen here and they are the same thing: a guessed row is drawn
   * as the command left it, and a guessed row whose labels no longer include
   * the mailbox on screen is dropped from it. That second clause is what
   * archive, trash and snooze ride on, and it is scoped to *this* mailbox —
   * the conversation goes on showing in All Mail and in its labels, which a
   * flat set of hidden ids could not express.
   *
   * A row with no guess is passed through untouched, identity included. Rows
   * are only rebuilt where a guess actually changes something, so a refetch
   * that returns the same data re-renders nothing.
   *
   * The third thing is the mirror of the second, and it is what an undo needs.
   * A guess that puts `INBOX` back on a conversation the list has already
   * dropped has no row here to be applied to, so `returningRows` draws it from
   * `remembered` — the list's own copy of the rows it has shown. Merged and
   * sorted only when there is something to merge, which is only ever the span
   * between a ⌘Z and the refetch that makes it true.
   */
  const visibleThreads = useMemo(() => {
    const guesses = ui.guesses;
    // Cheap when nothing is pending, which is almost always.
    if (Object.keys(guesses).length === 0) return allThreads;
    const rows: Thread[] = [];
    for (const thread of allThreads) {
      const guess = guesses[thread.id];
      if (!guess) {
        rows.push(thread);
        continue;
      }
      if (leavesMailbox(guess, ui.labelId)) continue;
      rows.push(applyGuess(thread, guess));
    }
    const returning = returningRows(
      ui.guesses,
      ui.remembered,
      ui.labelId,
      allThreads,
      ui.accountId,
    );
    if (returning.length === 0) return rows;
    return [...rows, ...returning].sort(byRecency);
  }, [allThreads, ui.guesses, ui.labelId, ui.remembered, ui.accountId]);

  /*
   * Retire an event guess when the store catches up with it.
   *
   * Asked of `events` — the whole loaded window — and never of `visibleEvents`,
   * because hiding a calendar in the sidebar takes rows off screen without
   * anything having happened to them, and retiring a guess on that would drop
   * one that is still doing work.
   */
  useEffect(() => {
    const settled = [
      ...settledEventGuesses(events, ui.eventGuesses),
      ...settledPendingEvents(events, ui.pendingEvents),
    ];
    if (settled.length > 0) dispatchUi({ type: "forgetEvents", eventIds: settled });
  }, [events, ui.eventGuesses, ui.pendingEvents]);

  /**
   * The event window with every outstanding guess on it, placeholders included.
   *
   * The calendar half of `visibleThreads`, and the one place a calendar guess
   * becomes a row — so the grid, the keyboard cursor, the right-click menu and
   * the detail panel are all looking at the same events rather than three of
   * them looking at the store and one at a private override map.
   */
  const projectedEvents = useMemo(
    () => applyEventGuesses(events, ui.eventGuesses, ui.pendingEvents),
    [events, ui.eventGuesses, ui.pendingEvents],
  );

  const visibleEvents = useMemo(() => {
    const hidden = new Set(ui.hiddenCalendars);
    return projectedEvents.filter((e) => !hidden.has(e.calendarId));
  }, [projectedEvents, ui.hiddenCalendars]);

  const selectedIndex = useMemo(
    () => visibleThreads.findIndex((t) => t.id === ui.threadId),
    [visibleThreads, ui.threadId],
  );

  /** The list, as ids, in the order the rows are painted. */
  const listIds = useMemo(() => visibleThreads.map((t) => t.id), [visibleThreads]);

  /*
   * Keep the selection honest against the list under it.
   *
   * `threads-changed` fires continuously during a sync and the list is
   * refetched underneath whatever is selected. Anything that left the mailbox —
   * archived on the phone, moved by a filter, optimistically hidden here — has
   * to leave the selection with it, or the count in the status bar describes
   * rows that no longer exist. `prune` returns the same object when nothing
   * changed, which is what keeps this from looping.
   */
  useEffect(() => {
    const pruned = prune(ui.selection, listIds);
    if (pruned !== ui.selection) dispatch({ type: "selection", selection: pruned });
  }, [ui.selection, listIds]);

  const commandTargetIds = useMemo(
    () => commandTargets(ui.selection, ui.threadId),
    [ui.selection, ui.threadId],
  );

  const isRowSelected = useCallback(
    (id: ThreadId) => ui.selection.ids.includes(id),
    [ui.selection],
  );

  // The event as it is *drawn*. An optimistic move is in the projection and not
  // yet in the store, and a cursor that read the store would be pointing at the
  // block's old time.
  const selectedEvent = useMemo(
    () => projectedEvents.find((e) => e.id === ui.eventId) ?? null,
    [projectedEvents, ui.eventId],
  );

  const accountById = useCallback(
    (id: AccountId) => accounts.find((a) => a.id === id),
    [accounts],
  );
  const calendarById = useCallback(
    (id: CalendarId) => calendars.find((c) => c.id === id),
    [calendars],
  );
  /*
   * Whether a row should be drawn unread, guess included.
   *
   * A row out of `visibleThreads` has already had the guess applied to it, so
   * this would be `thread.unread` for those. It reads the map anyway, because
   * the reading pane and the search view ask about rows that never went through
   * that projection, and the answer has to be the same in all three places.
   */
  const isUnread = useCallback(
    (thread: Thread) => ui.guesses[thread.id]?.unread ?? thread.unread,
    [ui.guesses],
  );

  // A favorited view is the label *and* the account filter it was pinned
  // under, so "Inbox" and "Inbox · Personal" can both live in the rail.
  const viewFavorite = useMemo<Favorite>(() => {
    const label = labels.find((l) => l.id === ui.labelId);
    const scope = accounts.find((a) => a.id === ui.accountId);
    const name = label ? mailboxName(label) : ui.labelId;
    return {
      kind: "mailbox",
      labelId: ui.labelId,
      accountId: ui.accountId,
      name: scope ? `${name} · ${scope.name}` : name,
    };
  }, [labels, accounts, ui.labelId, ui.accountId]);

  /**
   * The conversation as it will be once the queue write lands, which is what the
   * reading pane draws. Nothing here waits for that write; see `draftSent`.
   */
  const visibleDetail = useMemo<ThreadDetail | null>(() => {
    if (!detail || sentDrafts.size === 0) return detail;
    const messages = detail.messages.filter(
      (message) => !message.machDraftId || !sentDrafts.has(message.machDraftId),
    );
    return messages.length === detail.messages.length ? detail : { ...detail, messages };
  }, [detail, sentDrafts]);

  const focusedThread = useMemo(
    () =>
      detail?.thread ??
      (ui.threadId === null ? null : allThreads.find((t) => t.id === ui.threadId)) ??
      null,
    [detail, allThreads, ui.threadId],
  );

  const threadFavorite = useMemo<Favorite | null>(
    () =>
      focusedThread
        ? {
            kind: "thread",
            threadId: focusedThread.id,
            accountId: focusedThread.accountId,
            name: focusedThread.subject || "(no subject)",
          }
        : null,
    [focusedThread],
  );

  const isFavorite = useCallback(
    (favorite: Favorite | null) => (favorite ? isFavorited(favorites, favoriteKey(favorite)) : false),
    [favorites],
  );

  /**
   * Dispatch a command and reconcile the optimistic state with what actually
   * happened.
   *
   * `execute_command` can come back `ok: false` with some ids applied and the
   * rest rolled back — one account rate-limited out of five, say. The rolled
   * back ids have to come back on screen, or the user is looking at a list that
   * disagrees with their mailbox.
   *
   * `reselectFailed` is the bulk half of that: archive fifty, three roll back,
   * and those three come back *still selected*, so retrying is one keystroke
   * rather than a hunt through the list for which ones did not make it. The
   * status line says so too — `describeResult` refuses to report fifty.
   *
   * `quiet` means two things, and they are the same thing: the user is not
   * told, and it does not go on the undo stack. Opening a conversation marks it
   * read quietly, and a ⌘Z that answered by marking it unread again would be
   * answering a question nobody asked. It is also what keeps an undo's own
   * dispatches off the stack — undo runs quietly, and the entry it moves to the
   * redo side is already the record of it.
   *
   * Returns the result so a traversal can collect the inverses it hands back;
   * `null` means the command never reached the command layer.
   *
   * # The guess goes out first
   *
   * Before anything is awaited, so it is on screen in the frame the keystroke
   * produced rather than one round trip later. This is the only place it
   * happens, which is what makes every command optimistic at once: a keystroke,
   * a ⌘K entry, the snooze picker, the inverse a ⌘Z dispatches and the original
   * a ⇧⌘Z re-applies all arrive here, and none of them has to remember to say
   * what it is about to do to the list.
   *
   * **Both halves of the vocabulary**, since the calendar came through this
   * door. It used to have a write path of its own that executed against the
   * data source directly, which is the whole reason none of its five commands
   * was optimistic: the guess is made here, so a caller that went around this
   * went around that. `lib/projection.ts` answers for a conversation and for an
   * event, and this asks it both questions.
   *
   * `projected` is the one exception, and it is not a caller opting out of
   * being optimistic — it is a caller that has already been. A ⌘Z over a group
   * projects every step at once through `projectCommands` below, because one
   * gesture must land as one change; projecting again here, per step and a
   * round trip apart, would re-answer the same question against a list that may
   * have been refetched in between and repaint rows that were already right.
   * The guess is still *computed*, because the failure path below needs the
   * ids it covers.
   */
  const run = useCallback(
    async (
      command: Command,
      options: {
        quiet?: boolean;
        reselectFailed?: boolean;
        label?: string;
        /**
         * What the ⌘Z entry should say, when `label` would overstate it.
         *
         * The caller's half of {@link CommandResult.undoLabel}, and it exists
         * for one gesture: unsubscribing archives the conversation and then
         * asks the sender to stop, and the toast has to name the second thing
         * while ⌘Z can only take back the first. Without this the undo entry
         * inherits `label` and offers to undo an unsubscribe, which is a
         * promise nothing in the app can keep.
         */
        undoLabel?: string;
        projected?: boolean;
        /**
         * Where the list stood when the user did this, for ⌘Z to return to.
         *
         * Defaults to where it stands now, which is right for every caller that
         * dispatches nothing before it gets here. `bulk` is the exception and
         * has to say: it moves the cursor onward *before* calling this — that
         * is the behaviour being preserved — and a React dispatch is not
         * visible to `uiRef` until the render it causes, so reading it here
         * would be reading the pre-move state by accident rather than on
         * purpose. Saying so outright is what keeps that from being one
         * refactor away from silently recording the wrong row.
         */
        place?: UndoPlace;
        /**
         * Told about a command that did not entirely succeed.
         *
         * The status line says so already, and for most of the app that is
         * enough — it is transient because the undo offer beside it is. The
         * calendar's failures are the exception: a drag, a resize and an
         * arrow-key nudge have no dialog to fail in, so `CalendarMode` parks a
         * banner that stays until it is dismissed or retried. This is how it
         * hears, without a second execute path to hear it on.
         */
        onRefused?: (failure: { message: string; command: Command }) => void;
      } = {},
    ): Promise<CommandResult | null> => {
      /*
       * Where the list stood, read now rather than when the entry is recorded:
       * that happens after the round trip, by which time the cursor has moved
       * on and the selection has been cleared.
       *
       * Only for a command that names conversations. A calendar write has no
       * business remembering the mail cursor, and an entry that carried one
       * would move the list under a ⌘Z that was about an event.
       */
      const place = "threadIds" in command ? (options.place ?? livePlace()) : undefined;
      /*
       * Synchronous, and before the first `await`: everything down to here runs
       * in the same tick as the gesture that called it. Both halves of the
       * vocabulary — the conversation the command names, or the event.
       */
      const guesses = project(command, threadsRef.current);
      if (guesses && !options.projected) {
        dispatchUi({
          type: "project",
          guesses,
          rows: namedRows(guesses, threadsRef.current),
          listVersion: listVersion.current,
        });
      }
      const eventGuesses = projectEvent(command);
      if (eventGuesses && !options.projected) {
        dispatchUi({ type: "projectEvents", guesses: eventGuesses });
      }
      /*
       * A create has no id to guess about, so it gets a block instead — drawn
       * from the draft it carries, under an id nothing on Google could have.
       * Made here rather than in `projectCommands` because a placeholder is not
       * a guess about a row that exists: there is nothing for a second caller
       * to have already spoken for, so `projected` does not apply to it.
       */
      const placeholder =
        command.kind === "createEvent" ? pendingEventId() : null;
      if (placeholder !== null && command.kind === "createEvent") {
        dispatchUi({ type: "pendEvent", event: placeholderEvent(command, placeholder) });
      }
      /** Everything this command drew, for the failure path to take back. */
      const drawnEventIds = [
        ...guessedEventIds(command),
        ...(placeholder === null ? [] : [placeholder]),
      ];
      /*
       * Say that an entry is on its way, so a ⌘Z arriving now can wait for it.
       *
       * Only for a command that will record one — a traversal's own steps run
       * `quiet` and record nothing, and making ⌘Z wait for those would turn a
       * held key into one entry per round trip.
       */
      const recorded = options.quiet ? null : awaitable(unrecorded.current);
      try {
        const result = await getDataSource().execute(command);
        // A calendar command's whole effect is rows in the event window, and
        // nothing else refetches that window. Without this, `z` on a deleted
        // event puts it back on Google and in SQLite and leaves the grid empty —
        // undo that reports success and visibly does nothing.
        if (isCalendarCommand(command)) setEventsKey((k) => k + 1);
        /*
         * The exact inverse of what was drawn above, and it is the whole of it:
         * the write did not happen, so the block goes back to where it was and
         * a placeholder for an event that was never created is removed. Not
         * dropped on success, for the same reason a thread guess is not — the
         * refetch behind this carries the truth eventually, and until it does
         * the guess is what is right.
         */
        if (!result.ok && drawnEventIds.length > 0) {
          dispatchUi({ type: "forgetEvents", eventIds: drawnEventIds });
        } else if (result.ok && placeholder !== null) {
          // `applied` is where a create's new row id comes back. Handing it to
          // the placeholder is what lets the placeholder retire on the exact
          // row rather than on a guess at which event is the one it drew.
          dispatchUi({
            type: "resolvePending",
            eventId: placeholder,
            realId: result.applied[0] ?? null,
          });
        }
        if (!result.ok) {
          options.onRefused?.({ message: describeResult(result), command });
        }
        /*
         * The guess is deliberately *not* dropped here on success.
         *
         * It used to be, on the reasoning that "the refetch that follows
         * carries the truth". The refetch does — eventually. `execute_command`
         * emits `threads-changed` after it returns, `scheduleRefresh`
         * coalesces that over 600ms, and then `list_threads` has its own round
         * trip to make. For that whole span the row renders from a copy of the
         * list fetched *before* the write, so dropping the guess the instant
         * the command answered put the star out again for most of a second and
         * then lit it back up: "starring a msg flashes the star before it
         * sticks".
         *
         * What replaces it is the effect beside `allThreads` above: the guess
         * is dropped when the list actually agrees with it, which is the
         * condition the old code was trying to approximate by timing. Nothing
         * is pinned — an unstar arriving later from Gmail lands in a list the
         * guess has already been retired from.
         */
        const failed = failedIds(result);
        if (failed.length > 0) {
          // A rolled-back id is the one case where the guess has to go now:
          // the write did not happen, and the row has to say so rather than
          // wait for a refetch to contradict it. Exactly the rolled-back ids —
          // a partial failure leaves the rest of the set projected, because
          // for them it did happen.
          dispatchUi({ type: "forget", threadIds: failed });
          if (options.reselectFailed) {
            dispatchUi({
              type: "selection",
              selection: selectOnly(emptySelection, failed, failed),
            });
          }
        }
        /*
         * `label` is how a caller says something the command layer cannot
         * know. Only snooze uses it, and only because the interesting half of
         * a snooze is the wake time: Rust answers "Snoozed 3 conversations",
         * which leaves the status line and the ⌘Z entry unable to say whether
         * the user picked tomorrow or next week — the one thing they just
         * chose. It is ignored the moment anything fails, because
         * `describeResult` is then reporting a partial application and a
         * caller's optimistic phrasing would be a lie about what happened.
         */
        const message = options.label && result.ok ? options.label : describeResult(result);
        /*
         * The undo entry says less than the toast when the command did
         * something ⌘Z cannot take back. Only trash produces one today, and
         * only when the selection held a draft: `drafts.delete` is permanent,
         * so the toast reports the drafts and the button offers only the
         * conversations. See `CommandResult.undoLabel`.
         */
        const undoLabel =
          (result.ok ? options.undoLabel : undefined) ??
          (options.label && result.ok ? options.label : result.undoLabel ?? message);
        if (!options.quiet || !result.ok) {
          dispatchUi({
            type: "status",
            status: {
              message,
              undo: result.undo,
              tone: result.ok ? "info" : "error",
            },
          });
        }
        // Through the reducer's own dispatch above, so this is the only record
        // made — and it is the better one, because the original command is in
        // hand here and a status message could only ever carry the inverse.
        // A partial failure still records: the inverse the command layer
        // returned covers only the ids that actually applied.
        if (!options.quiet) {
          commitUndo(pushUndo(undoRef.current, command, result, undoLabel, Date.now(), place));
        }
        return result;
      } catch (caught) {
        // The command never ran, so nothing changed anywhere: drop the whole
        // guess, not part of it.
        if (guesses) {
          dispatchUi({ type: "forget", threadIds: Object.keys(guesses).map(Number) });
        }
        if (drawnEventIds.length > 0) {
          dispatchUi({ type: "forgetEvents", eventIds: drawnEventIds });
        }
        const message = toMailboxError(caught).message;
        dispatchUi({ type: "status", status: { message, tone: "error" } });
        options.onRefused?.({ message, command });
        return null;
      } finally {
        recorded?.();
      }
    },
    [commitUndo, livePlace],
  );

  /**
   * Every step of a traversal, guessed at once, in one dispatch.
   *
   * A group of three is one thing the user did and has to be one thing they see
   * taken back. Left to `run`, each step's guess went out after the previous
   * step's round trip, so a three-step ⌘Z repainted in three stages a few
   * hundred milliseconds apart.
   *
   * Merged rather than dispatched one after another so it is a single render,
   * and merged in dispatch order because that is the order `uiReducer` resolves
   * two claims about the same conversation in: the later one wins, exactly as it
   * would have if the steps had gone out separately.
   */
  const projectCommands = useCallback((commands: Command[]) => {
    const merged: Guesses = {};
    const mergedEvents: EventGuesses = {};
    let any = false;
    let anyEvent = false;
    for (const command of commands) {
      const guesses = project(command, threadsRef.current);
      if (guesses) {
        Object.assign(merged, guesses);
        any = true;
      }
      // A group can hold both halves — a plugin action that labels a thread and
      // deletes the event it named is one thing the user did, and undoing it
      // has to put both back in one frame.
      const events = projectEvent(command);
      if (events) {
        Object.assign(mergedEvents, events);
        anyEvent = true;
      }
    }
    if (any) {
      dispatchUi({
        type: "project",
        guesses: merged,
        rows: namedRows(merged, threadsRef.current),
        listVersion: listVersion.current,
      });
    }
    if (anyEvent) dispatchUi({ type: "projectEvents", guesses: mergedEvents });
  }, []);

  /**
   * What a traversal of the stack is allowed to do to the app.
   *
   * Undo is not a special dispatch path: it runs the inverse through the same
   * `run` every other command goes through, which is what makes a calendar
   * undo refetch the event window and a mail undo reconcile a partial failure
   * without either of them knowing they were an undo.
   */
  const undoHost = useMemo<UndoHost>(
    () => ({
      read: () => undoRef.current,
      write: commitUndo,
      // `projected`, because `project` has already spoken for every step.
      execute: (command) => run(command, { quiet: true, projected: true }),
      project: projectCommands,
      /*
       * Both of these are now the same thing: drop whatever guess is standing
       * for these conversations.
       *
       * They used to be the two halves of the optimistic hide — clear it for an
       * unarchive, set it for a redone archive — because `run` knew nothing
       * about what a command did to the list and the traversal had to say so on
       * its behalf. It does know now, and `project` above states the whole set a
       * line later. What is left for the traversal to do is retract the
       * *previous* guess, so the archive's delta is not still sitting on the row
       * the unarchive is about to describe.
       */
      restore: (threadIds) => dispatchUi({ type: "forget", threadIds }),
      hide: (threadIds) => dispatchUi({ type: "forget", threadIds }),
      place: livePlace,
      returnTo: (place, arriving) => {
        if (!place) return;
        /*
         * A cursor only means anything in the list it was in.
         *
         * Changed mailbox, changed account filter, and the row it names is not
         * on the screen the user is looking at — so the undo restores the
         * conversation where it belongs and leaves the cursor alone. Yanking
         * someone from Starred back to the Inbox because they pressed ⌘Z would
         * be a worse bug than the one this fixes.
         */
        if (place.labelId !== uiRef.current.labelId) return;
        if (place.accountId !== uiRef.current.accountId) return;
        /*
         * What can be on screen: the loaded list, the rows it remembers having
         * shown, and the rows this traversal is about to name. The third is the
         * one that matters for a ⌘Z pressed long after the fact — by then the
         * list has dropped the archived conversation and `remembered` may have
         * evicted it, and the unarchive being dispatched is the whole reason it
         * is coming back.
         */
        const reachable = new Set<ThreadId>(arriving);
        for (const thread of threadsRef.current) reachable.add(thread.id);
        for (const id of uiRef.current.remembered.keys()) reachable.add(id);

        // The two halves are answered separately, because a cursor that cannot
        // come back is no reason to leave the ticks off the rows that can.
        // `keepAnchor`, because the selection restored below carries its own
        // anchor — the one the action was taken with — and re-pointing it at
        // the cursor would lose the range a following ⇧J grows from.
        if (
          place.threadId !== null &&
          place.threadId !== uiRef.current.threadId &&
          reachable.has(place.threadId)
        ) {
          dispatchUi({ type: "thread", threadId: place.threadId, keepAnchor: true });
        }
        /*
         * The ticks come back too.
         *
         * Tick five, archive, ⌘Z: the five conversations return, and returning
         * them unticked would mean re-ticking all five to try again. It is the
         * same answer `reselectFailed` already gives when Google refuses part
         * of a bulk archive — the survivors come back selected — and undo is
         * making the larger claim, that things are as they were.
         *
         * Pruned against what is actually there, so a member the list has since
         * lost does not sit in the count as a row nobody can see.
         */
        const selection = prune(place.selection, [...reachable]);
        if (selection.ids.length > 0 || uiRef.current.selection.ids.length > 0) {
          dispatchUi({ type: "selection", selection });
        }
      },
      // A traversal's own message carries no inverse — the entry it moved is
      // the record of it — so it says which button to hold out next instead.
      say: (message, offer) =>
        dispatchUi({ type: "status", status: { message, tone: "info", offer } }),
    }),
    [commitUndo, livePlace, projectCommands, run],
  );

  /**
   * Run a traversal, once every action that owes the stack an entry has made it.
   *
   * The synchronous path is the one that matters and is kept exactly: with
   * nothing outstanding, `runUndo` is called from the keystroke's own tick, so
   * its pop and its `showSteps` land in the frame that keystroke produced. The
   * await is only for the case that used to lose the keystroke outright —
   * pressing ⌘Z while the archive it is taking back is still in flight, where
   * the stack is empty and a traversal has nothing to read.
   *
   * Waiting rather than recording an entry up front is what keeps the stack
   * from guessing: the inverse belongs to the command layer, and an entry made
   * before the answer would be a claim about a write that may yet be refused.
   * What it costs is that the ⌘Z still cannot finish before the command it
   * follows does.
   */
  const traverse = useCallback(
    (run: (host: UndoHost) => Promise<UndoOutcome>) => {
      const waiting = [...unrecorded.current];
      if (waiting.length === 0) {
        void run(undoHost);
        return;
      }
      void (async () => {
        await Promise.allSettled(waiting);
        await run(undoHost);
      })();
    },
    [undoHost],
  );

  // Opening an unread conversation marks it read — once, and quietly.
  const markedRead = useRef(new Set<ThreadId>());
  useEffect(() => {
    const thread = detail?.thread;
    if (!thread || !thread.unread || markedRead.current.has(thread.id)) return;
    markedRead.current.add(thread.id);
    void run({ kind: "markRead", threadIds: [thread.id], read: true }, { quiet: true });
  }, [detail, run]);

  const state = useMemo(
    () =>
      mailboxState({
        booted: booted && !stream.loading,
        error: bootError ?? stream.error,
        accountCount: accounts.length,
        threadCount: visibleThreads.length,
        sync,
        filtered: ui.accountId !== null || ui.labelId !== "INBOX",
      }),
    [
      booted,
      stream.loading,
      stream.error,
      bootError,
      accounts.length,
      visibleThreads.length,
      sync,
      ui.accountId,
      ui.labelId,
    ],
  );

  const progress = useMemo(() => syncProgress(sync), [sync]);

  const actions = useMemo<MachActions>(() => {
    const selectAt = (index: number) => {
      const next = visibleThreads[clamp(index, 0, Math.max(visibleThreads.length - 1, 0))];
      if (next) dispatch({ type: "thread", threadId: next.id });
    };

    /**
     * Run one command over everything the user pointed at.
     *
     * *One* command: the Rust layer groups the ids by (account, label delta)
     * and issues a single Gmail `batchModify` per group, which it can only do
     * if the whole set arrives together. Fifty archives dispatched one at a
     * time would be fifty round trips, fifty status messages, and an undo
     * stack fifty deep for one gesture.
     *
     * Every caller used to pass a `hides` flag saying whether the rows would
     * leave the list, because that decided both the optimistic hide and whether
     * the cursor had to move. `run` projects the hide off the command itself
     * now, and the same projection answers the cursor question better than the
     * flag did: archiving from a label the conversation still carries does not
     * take the row anywhere, and the cursor should not jump as though it had.
     */
    const bulk = (command: MailCommand, label?: string, undoLabel?: string) => {
      const ids = command.threadIds;
      if (ids.length === 0) return;
      // Read before the two dispatches below move it. This is what ⌘Z returns
      // to, and it is the only moment it can be read: by the time the command
      // answers and the entry is recorded, the cursor has moved on and the
      // selection has been cleared.
      const place: UndoPlace = {
        threadId: ui.threadId,
        selection: ui.selection,
        labelId: ui.labelId,
        accountId: ui.accountId,
      };
      const leaving = leavingIds(command, visibleThreads, ui.labelId);
      if (leaving.length > 0) {
        const nextFocus = nextAfterRemoval(listIds, leaving, ui.threadId);
        if (nextFocus !== ui.threadId) dispatch({ type: "thread", threadId: nextFocus });
      }
      dispatch({ type: "selection", selection: clearSelection(ui.selection) });
      // Synchronous up to its first `await`, so the rows are gone in the same
      // React batch as the cursor move above rather than a frame after it.
      void run(command, { reselectFailed: true, label, undoLabel, place });
    };

    const pin = (favorite: Favorite) => {
      const pinned = !isFavorite(favorite);
      setFavorites((list) => toggleFavorite(list, favorite));
      dispatch({
        type: "status",
        status: {
          message: pinned
            ? `Added ${favorite.name} to favorites`
            : `Removed ${favorite.name} from favorites`,
          tone: "info",
        },
      });
    };

    return {
      setMode: (mode) => dispatch({ type: "mode", mode }),
      toggleMode: () =>
        dispatch({ type: "mode", mode: ui.mode === "mail" ? "calendar" : "mail" }),
      setCalendarView: (view) => dispatch({ type: "calendarView", view }),
      moveCursor: (delta) => {
        const target = selectedIndex === -1 ? 0 : selectedIndex + delta;
        // Walking off the bottom of the loaded pages is the other half of
        // infinite scroll: the keyboard has to pull pages too.
        if (delta > 0 && target >= visibleThreads.length - 5) streamRef.current.loadMore();
        selectAt(target);
      },
      openSelected: () => {
        if (selectedIndex === -1) selectAt(0);
      },
      closeThread: () => dispatch({ type: "thread", threadId: null }),
      selectThread: (id) => dispatch({ type: "thread", threadId: id }),

      /**
       * A click on a row. Which of the three gestures it is depends entirely
       * on the modifiers, exactly as it does in Finder and in Gmail:
       *
       *  * plain — read this conversation; any selection goes away.
       *  * ⇧     — select from the anchor to here, replacing the last range.
       *  * ⌘/⌃   — tick this one row, leave everything else alone.
       *
       * The two modified gestures deliberately do **not** move the cursor into
       * the clicked row. Opening a conversation marks it read on Google, and a
       * modifier-click is a statement about selection, not about reading.
       */
      clickThread: (id, modifiers) => {
        // Touching the list is a claim on the keyboard, wherever it was.
        if (ui.focus !== "list") dispatch({ type: "focus", focus: "list" });
        if (modifiers.extend) {
          dispatch({ type: "selection", selection: extendTo(ui.selection, id, listIds) });
        } else if (modifiers.toggle) {
          dispatch({ type: "selection", selection: toggleSelection(ui.selection, id) });
        } else {
          dispatch({ type: "selection", selection: anchorAt(ui.selection, id) });
          dispatch({ type: "thread", threadId: id });
        }
      },

      // `x` — tick and move on, so a run of them selects a run of rows. The
      // cursor move re-anchors, which is what makes a following ⇧J extend from
      // here rather than from the first row of the run.
      toggleAtCursor: () => {
        if (ui.threadId === null) {
          if (visibleThreads.length > 0) selectAt(0);
          return;
        }
        // Selects, and leaves the cursor where it is.
        //
        // It used to advance, on the theory that ticking a run of rows is one
        // gesture. It reads as the app moving under you: `x` is the only key
        // that both changed the selection *and* moved, so a mis-tick left you
        // somewhere you did not choose and undoing it meant arrowing back. The
        // owner: "X shortcut should select but not move. that's a confusing UX.
        // I can move using arrows."
        //
        // Gmail's `x` does the same nothing, and a run of rows already has a
        // gesture that is *about* moving — ⇧J drags the range with the cursor.
        dispatch({ type: "selection", selection: toggleSelection(ui.selection, ui.threadId) });
      },

      // ⇧J / ⇧K — the cursor moves and the range follows it. `keepAnchor` is
      // the whole difference from `moveCursor`: the range still grows from
      // where shift was first pressed, so shrinking it back works.
      extendCursor: (delta) => {
        if (visibleThreads.length === 0) return;
        const from = selectedIndex === -1 ? 0 : selectedIndex;
        const target = visibleThreads[clamp(from + delta, 0, visibleThreads.length - 1)];
        if (!target) return;
        if (delta > 0 && from + delta >= visibleThreads.length - 5) streamRef.current.loadMore();
        // With no anchor yet, the row being left behind becomes one — ⇧J from a
        // cold start selects the row you were on *and* the row you land on.
        const anchored =
          ui.selection.anchor === null && ui.threadId !== null
            ? reanchor(ui.selection, ui.threadId)
            : ui.selection;
        dispatch({ type: "selection", selection: extendTo(anchored, target.id, listIds) });
        dispatch({ type: "thread", threadId: target.id, keepAnchor: true });
      },

      // ⌘A — and it says how much of the mailbox that actually was. The list is
      // a page of an infinite one; "all" can only ever mean all of what has
      // been fetched, and claiming otherwise before archiving is a lie.
      selectAllThreads: () => {
        if (listIds.length === 0) return;
        const next = toggleAll(ui.selection, listIds);
        dispatch({ type: "selection", selection: next });
        dispatch({
          type: "status",
          status: {
            message:
              next.ids.length > 0
                ? selectAllMessage(next.ids.length, streamRef.current.hasMore)
                : "Selection cleared",
            tone: "info",
          },
        });
      },

      clearSelection: () =>
        dispatch({ type: "selection", selection: clearSelection(ui.selection) }),

      archiveSelected: () => bulk({ kind: "archive", threadIds: commandTargetIds }),
      trashSelected: () => bulk({ kind: "trash", threadIds: commandTargetIds }),
      reportSpamSelected: () =>
        bulk({ kind: "reportSpam", threadIds: commandTargetIds }),
      unsubscribe: () => {
        const thread = visibleDetail?.thread;
        const target = visibleDetail ? unsubscribeAction(visibleDetail.messages) : null;
        /*
         * Quietly, and with one line rather than none.
         *
         * Almost every conversation in a mailbox has no `List-Unsubscribe`, so
         * the key and the ⌘K entry will be pressed against one sooner or later.
         * An error would be wrong — nothing failed — and silence reads as a
         * dropped keystroke, which is the thing that makes people press a key
         * twice.
         */
        if (!thread || !target) {
          dispatch({
            type: "status",
            status: { message: "No unsubscribe offered here", tone: "info" },
          });
          return;
        }

        // It looks like spam rather than like a newsletter, so this is the
        // whole gesture: report it, and never confirm the address to whoever
        // sent it. `reportSpam` has an exact inverse, so ⌘Z covers all of it.
        if (target.offer.offer === "reportSpam") {
          bulk({ kind: "reportSpam", threadIds: [thread.id] });
          return;
        }

        const openPage = (system = false) => {
          void getDataSource()
            .openUnsubscribePage(target.messageId, system)
            .catch((caught) =>
              dispatch({
                type: "status",
                status: { message: toMailboxError(caught).message, tone: "error" },
              }),
            );
        };

        /**
         * The failure, said out loud, with the one thing left to try beside it.
         *
         * A refused unsubscribe is the case worth being careful about: the
         * conversation is already archived, the request went nowhere, and the
         * sender goes on sending. The page is not a consolation prize — for a
         * `mailto:` list Google would not send to, it is usually the route that
         * works.
         */
        const refused = (message: string) =>
          dispatch({
            type: "status",
            status: {
              message,
              tone: "error",
              action: {
                word: "Open page",
                title: `Open ${target.sender}'s unsubscribe page`,
                run: openPage,
              },
            },
          });

        // A link is a page with a form on it. Rust will not act on one and
        // neither will this: the URL never reaches the webview, so the id goes
        // out and a window opens with the page in it.
        if (target.offer.method === "link") {
          openPage();
          return;
        }

        /*
         * The acknowledgement, and it is the archive.
         *
         * The row leaves the list in the frame the keystroke produced, because
         * `run` projects before its first `await` — so the conversation is gone
         * long before the sender has answered. `undoLabel` is what keeps the
         * ⌘Z entry honest: the toast says the unsubscribe is on its way, and
         * the button beside it offers back the one thing that can be given
         * back.
         */
        const inInbox = thread.labelIds.includes(INBOX);
        if (inInbox) {
          bulk(
            { kind: "archive", threadIds: [thread.id] },
            `Unsubscribing from ${target.sender}…`,
            "Archived 1 conversation",
          );
        } else {
          dispatch({
            type: "status",
            status: { message: `Unsubscribing from ${target.sender}…`, tone: "info" },
          });
        }

        /*
         * Fired, not awaited — and executed here rather than through `run`.
         *
         * There is nothing for `run` to do with it: no guess to project, no
         * inverse to record, and its own failure line has to carry a button
         * that the generic path has no way to attach. `projection-coverage`
         * allows this file to execute for exactly that reason, and the
         * exemption is written down in `NOT_PROJECTED`.
         */
        void getDataSource()
          .execute({ kind: "unsubscribe", messageId: target.messageId })
          .then((result) => {
            if (result.ok) {
              dispatch({
                type: "status",
                status: { message: `Unsubscribed from ${target.sender}`, tone: "info" },
              });
              return;
            }
            refused(unsubscribeRefusal(result, target.sender));
          })
          .catch((caught) =>
            refused(
              `Could not unsubscribe from ${target.sender} — ${toMailboxError(caught).message}`,
            ),
          );
      },
      /*
       * The same page, handed to the system browser.
       *
       * It resolves the target through `unsubscribeAction` exactly as
       * `unsubscribe` does, so the two can never write to different senders,
       * and it reports the same "nothing here" line rather than doing nothing.
       * `reportSpam` has no page at all — there is nothing to open, and opening
       * something would be the one thing that verdict exists to prevent.
       */
      unsubscribePageInBrowser: () => {
        const target = visibleDetail ? unsubscribeAction(visibleDetail.messages) : null;
        if (!target || target.offer.offer !== "unsubscribe") {
          dispatch({
            type: "status",
            status: { message: "No unsubscribe page here", tone: "info" },
          });
          return;
        }
        void getDataSource()
          .openUnsubscribePage(target.messageId, true)
          .catch((caught) =>
            dispatch({
              type: "status",
              status: { message: toMailboxError(caught).message, tone: "error" },
            }),
          );
      },
      snoozeSelected: (until) =>
        bulk(
          { kind: "snooze", threadIds: commandTargetIds, until },
          snoozeLabel(commandTargetIds.length, until, Date.now()),
        ),
      // Starring a mixed set stars all of it; only an already-all-starred set
      // unstars. Anything else and the same keystroke does opposite things to
      // different rows of one selection.
      starSelected: () => {
        const byId = new Map(visibleThreads.map((t) => [t.id, t]));
        const allStarred =
          commandTargetIds.length > 0 &&
          commandTargetIds.every((id) => byId.get(id)?.starred === true);
        bulk({ kind: "star", threadIds: commandTargetIds, starred: !allStarred });
      },

      setFocus: (focus) => dispatch({ type: "focus", focus }),
      toggleFocus: () =>
        dispatch({ type: "focus", focus: ui.focus === "list" ? "rail" : "list" }),
      // The composer owns the draft, and it lives in the reading pane's own
      // subtree; the reply button in `ReadingPane` belongs to another unit's
      // file, so this is the seam between them.
      replySelected: () =>
        window.dispatchEvent(new CustomEvent("mach:compose", { detail: { kind: "reply" } })),
      /*
       * ⌘Z and `z` are the same key.
       *
       * Both used to read the inverse off `ui.status`, which meant undo could
       * only ever reach the last action and only while that message was still
       * on screen. It reads the stack now, so it reaches as far back as the
       * user does — and the status message goes back to being what it says it
       * is, a note about what just happened.
       */
      openArtifact: (artifact) => {
        if (artifact.kind === "event") {
          dispatch({ type: "mode", mode: "calendar" });
          // The grid shows a window around the anchor, so an event next month
          // is only reachable if the anchor moves with it.
          dispatch({ type: "anchor", anchor: artifact.startMs });
          dispatch({ type: "event", eventId: artifact.eventId });
          return;
        }
        dispatch({ type: "mode", mode: "mail" });
        const threadId = artifact.kind === "thread" ? artifact.threadId : artifact.threadId;
        // The conversation may not be in the list being shown — a draft on an
        // archived thread, say. The reading pane fetches by id, so opening it
        // does not depend on the mailbox it is filed under.
        if (threadId != null) dispatch({ type: "thread", threadId });
        if (artifact.kind === "draft") {
          window.dispatchEvent(
            new CustomEvent("mach:compose", {
              detail: { kind: "draft", draftId: artifact.draftId },
            }),
          );
        }
      },
      undo: () => traverse(runUndo),
      redo: () => traverse(runRedo),

      pushUndoGroup: (label, inverses) => {
        if (inverses.length === 0) return;
        dispatch({
          type: "status",
          status: {
            message: label,
            undo: inverses.length === 1 ? inverses[0] : inverses,
            tone: "info",
          },
        });
      },
      shiftPeriod: (delta) => {
        const step =
          ui.calendarView === "day"
            ? addDays(ui.anchor, delta)
            : ui.calendarView === "week"
              ? addDays(ui.anchor, delta * 7)
              : addMonths(ui.anchor, delta);
        dispatch({ type: "anchor", anchor: step.getTime() });
      },
      goToday: () => dispatch({ type: "anchor", anchor: Date.now() }),
      setPalette: (open) => dispatch({ type: "palette", open }),
      setAddAccount: (open, email) => dispatch({ type: "addAccount", open, email }),
      setSnooze: (open) => dispatch({ type: "snooze", open }),
      setStatus: (message, tone = "info") =>
        dispatch({ type: "status", status: { message, tone } }),
      toggleFavoriteView: () => pin(viewFavorite),
      toggleFavoriteThread: () => {
        if (!threadFavorite) {
          dispatch({
            type: "status",
            status: { message: "Open a conversation first", tone: "info" },
          });
          return;
        }
        pin(threadFavorite);
      },
      // One key for both, because "favorite this" means whatever is in front of
      // you: the conversation you are reading, or the mailbox you are in.
      toggleFavoriteFocused: () => pin(threadFavorite ?? viewFavorite),
      unfavorite: (key) => setFavorites((list) => removeFavorite(list, key)),
      openFavorite: (favorite) => {
        if (favorite.kind === "mailbox") {
          dispatch({ type: "account", accountId: favorite.accountId });
          dispatch({ type: "label", labelId: favorite.labelId });
        } else {
          dispatch({ type: "mode", mode: "mail" });
          dispatch({ type: "thread", threadId: favorite.threadId });
        }
      },
      // Writes the preference rather than the state: the state is a mirror of
      // it, so setting the mirror would be undone by the next sync from the
      // thing being mirrored — and would not survive a relaunch either.
      cycleTheme: () =>
        setPreferenceFromAnywhere({
          theme: ui.theme === "system" ? "light" : ui.theme === "light" ? "dark" : "system",
        }),
      loadMore: () => streamRef.current.loadMore(),
      /*
       * Go and look at Google now — mail and calendar together.
       *
       * The promise resolves when the pass is *over*, not when the request was
       * accepted, which is what makes the line at the end worth anything.
       * Nothing on screen waits for it: every pane goes on rendering from
       * SQLite, and the sync indicator narrates the pass from the `sync-status`
       * event exactly as it does for a background one.
       *
       * `beginForcedSync` is the reason a second press is free. The engine
       * refuses to run two passes over one account regardless, so this is not
       * where correctness lives — it is what stops a pointless round trip and
       * what the palette reads to show the entry as busy.
       */
      syncNow: (accountId) => {
        const key = accountId ?? "all";
        if (!beginForcedSync(key)) return;
        void getDataSource()
          .syncNow(accountId)
          .then((outcome) => {
            const said = forcedSyncMessage(outcome);
            dispatch({ type: "status", status: { message: said.message, tone: said.tone } });
          })
          .catch((caught) =>
            dispatch({
              type: "status",
              status: { message: toMailboxError(caught).message, tone: "error" },
            }),
          )
          .finally(() => endForcedSync(key));
      },
      reload: () => {
        setReloadKey((k) => k + 1);
        streamRef.current.refresh();
      },
      draftSent: (draftId) =>
        setSentDrafts((current) => (current.has(draftId) ? current : new Set(current).add(draftId))),
      draftRecalled: (draftId) =>
        setSentDrafts((current) => {
          if (!current.has(draftId)) return current;
          const next = new Set(current);
          next.delete(draftId);
          return next;
        }),
      execute: (command, options) => run(command, options),
      reloadEvents: () => setEventsKey((k) => k + 1),
    };
  }, [
    ui.mode,
    ui.threadId,
    // `ui.status` was a dependency for as long as undo read the inverse off it.
    // The stack is the source now, and rebuilding every action on every status
    // message was only ever the cost of that.
    ui.anchor,
    ui.calendarView,
    ui.theme,
    ui.selection,
    ui.focus,
    selectedIndex,
    visibleThreads,
    // `unsubscribe` resolves which message carries the offer out of the open
    // conversation, so the action has to be rebuilt when that conversation is.
    visibleDetail,
    listIds,
    commandTargetIds,
    viewFavorite,
    threadFavorite,
    isFavorite,
    run,
    undoHost,
    traverse,
  ]);

  const uiWithOverlays = useMemo(() => ({ ...ui, overlays }), [ui, overlays]);

  const value: MachValue = {
    // The reducer's state plus the one field it does not own — see `UiState`.
    ui: uiWithOverlays,
    accounts,
    labels,
    calendars,
    allThreads,
    visibleThreads,
    visibleEvents,
    // The projection, not the store: an id looked up here has to resolve to the
    // block that is on screen, whatever the store still says about it.
    events: projectedEvents,
    detail: visibleDetail,
    detailLoading,
    addressBook,
    favorites,
    viewFavorite,
    threadFavorite,
    isFavorite,
    selectedIndex,
    commandTargets: commandTargetIds,
    isRowSelected,
    selectedEvent,
    state,
    sync,
    progress,
    live: getDataSource().kind === "tauri",
    hasMore: stream.hasMore,
    loadingMore: stream.loadingMore,
    undoState,
    accountById,
    calendarById,
    isUnread,
    dispatch,
    actions,
  };

  return <MachContext.Provider value={value}>{children}</MachContext.Provider>;
}

export function useMach(): MachValue {
  const value = useContext(MachContext);
  if (!value) throw new Error("useMach must be used inside <MachProvider>");
  return value;
}

/**
 * The days a calendar view covers, given the anchor.
 *
 * `weekStartsOn` is a parameter rather than a module-level setting because both
 * the week strip and the six-week month grid have to agree with it in the same
 * render — a mutable global would have them agreeing only until something
 * re-rendered one of them. It defaults to `startOfWeek`'s own Monday, so every
 * caller that has no opinion behaves exactly as it did.
 */
export function viewRange(
  view: CalendarView,
  anchor: number,
  weekStartsOn?: WeekStart,
): { start: Date; days: number } {
  if (view === "day") return { start: new Date(new Date(anchor).setHours(0, 0, 0, 0)), days: 1 };
  if (view === "week") return { start: startOfWeek(anchor, weekStartsOn), days: 7 };
  return {
    start: startOfWeek(new Date(new Date(anchor).setDate(1)), weekStartsOn),
    days: 42,
  };
}
