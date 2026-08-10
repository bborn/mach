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
  failedIds,
  getDataSource,
  isMailCommand,
  type Command,
  type CommandResult,
  type MailCommand,
} from "@/lib/data";
import {
  emptyUndo,
  pushUndo,
  recordUndo,
  runRedo,
  runUndo,
  type UndoHost,
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
import { mailboxName } from "@/lib/mailboxes";
import type { Artifact } from "@/lib/agent";
import { connectNotificationOpen } from "@/lib/notification-open";
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
  tone: "info" | "error";
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
  /** Threads hidden optimistically, until the command layer confirms. */
  archived: ThreadId[];
  readExtra: ThreadId[];
  /**
   * Stars the user has toggled but the store has not confirmed yet.
   *
   * Archiving hides the row instantly, so it always felt immediate. Starring
   * had no equivalent: it waited on the whole round trip — IPC, the local
   * write, the Gmail call, the threads-changed event and a refetch — before
   * the star appeared. Same optimistic idea as `readExtra`, applied to a
   * property rather than to membership of the list.
   */
  starOverrides: Record<ThreadId, boolean>;
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
  | { type: "addAccount"; open: boolean }
  | { type: "snooze"; open: boolean }
  | { type: "listWidth"; width: number }
  | { type: "toggleCalendar"; calendarId: CalendarId }
  | { type: "theme"; theme: Theme }
  | { type: "archive"; threadIds: ThreadId[] }
  /** Put optimistically-hidden threads back — undo, or a command that failed. */
  | { type: "restore"; threadIds: ThreadId[] }
  | { type: "read"; threadIds: ThreadId[] }
  /** Show a star before the store confirms it. */
  | { type: "star"; threadIds: ThreadId[]; starred: boolean }
  /** Drop the guess — the store agrees now, or the command failed. */
  | { type: "unstar"; threadIds: ThreadId[] }
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
  snoozeOpen: false,
  listWidth: 520,
  hiddenCalendars: [],
  theme: "system",
  archived: [],
  readExtra: [],
  starOverrides: {},
  status: null,
};

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
        readExtra:
          action.threadId !== null
            ? uniq([...state.readExtra, action.threadId])
            : state.readExtra,
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
      return { ...state, addAccountOpen: action.open, paletteOpen: false };
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
    case "archive":
      return { ...state, archived: uniq([...state.archived, ...action.threadIds]) };
    case "restore":
      return { ...state, archived: state.archived.filter((id) => !action.threadIds.includes(id)) };
    case "read":
      return { ...state, readExtra: uniq([...state.readExtra, ...action.threadIds]) };
    case "star": {
      const next = { ...state.starOverrides };
      for (const id of action.threadIds) next[id] = action.starred;
      return { ...state, starOverrides: next };
    }
    case "unstar": {
      // Drops the guess, either because the store now agrees or because the
      // command failed. Either way the row goes back to whatever it says.
      const next = { ...state.starOverrides };
      for (const id of action.threadIds) delete next[id];
      return { ...state, starOverrides: next };
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

function uniq<T>(values: T[]): T[] {
  return [...new Set(values)];
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
  setAddAccount: (open: boolean) => void;
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
  syncNow: () => void;
  /** After an account is added or removed, everything is stale. */
  reload: () => void;
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
   * A backfill can emit `threads-changed` hundreds of times a minute. Coalesce:
   * one refetch per window, however many events arrived in it.
   *
   * **The open conversation is refetched too.** It was not, and the gap is what
   * "I clicked discard but the draft still shows" looked like: this event was
   * wired to the list only, so anything that changed the thread on screen —
   * a sync pass removing a draft, another window, the agent — repainted the
   * list beside a reading pane still showing the old messages. The list has
   * always refetched here; the pane was the half nobody told.
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
        setLabels(l);
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

  const visibleThreads = useMemo(() => {
    const archived = new Set(ui.archived);
    const overrides = ui.starOverrides;
    const rows = allThreads.filter((t) => !archived.has(t.id));
    // Cheap when nothing is pending, which is almost always.
    if (Object.keys(overrides).length === 0) return rows;
    return rows.map((t) =>
      t.id in overrides && overrides[t.id] !== t.starred
        ? { ...t, starred: overrides[t.id]! }
        : t,
    );
  }, [allThreads, ui.archived, ui.starOverrides]);

  const visibleEvents = useMemo(() => {
    const hidden = new Set(ui.hiddenCalendars);
    return events.filter((e) => !hidden.has(e.calendarId));
  }, [events, ui.hiddenCalendars]);

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

  const selectedEvent = useMemo(
    () => events.find((e) => e.id === ui.eventId) ?? null,
    [events, ui.eventId],
  );

  const accountById = useCallback(
    (id: AccountId) => accounts.find((a) => a.id === id),
    [accounts],
  );
  const calendarById = useCallback(
    (id: CalendarId) => calendars.find((c) => c.id === id),
    [calendars],
  );
  const isUnread = useCallback(
    (thread: Thread) => thread.unread && !ui.readExtra.includes(thread.id),
    [ui.readExtra],
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
   */
  const run = useCallback(
    async (
      command: Command,
      options: { quiet?: boolean; reselectFailed?: boolean; label?: string } = {},
    ): Promise<CommandResult | null> => {
      try {
        const result = await getDataSource().execute(command);
        // A calendar command's whole effect is rows in the event window, and
        // nothing else refetches that window. Without this, `z` on a deleted
        // event puts it back on Google and in SQLite and leaves the grid empty —
        // undo that reports success and visibly does nothing.
        if (!isMailCommand(command)) setEventsKey((k) => k + 1);
        if (command.kind === "star") {
          // The refetch that follows carries the truth; keeping the guess past
          // that point would pin a stale star if Gmail disagreed.
          dispatchUi({ type: "unstar", threadIds: command.threadIds });
        }
        const failed = failedIds(result);
        if (failed.length > 0) {
          dispatchUi({ type: "restore", threadIds: failed });
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
          commitUndo(pushUndo(undoRef.current, command, result, message, Date.now()));
        }
        return result;
      } catch (caught) {
        // The command never ran, so nothing changed anywhere: undo the whole
        // optimistic edit, not part of it.
        if (isMailCommand(command)) {
          dispatchUi({ type: "restore", threadIds: command.threadIds });
          if (command.kind === "star") {
            dispatchUi({ type: "unstar", threadIds: command.threadIds });
          }
        }
        dispatchUi({
          type: "status",
          status: { message: toMailboxError(caught).message, tone: "error" },
        });
        return null;
      }
    },
    [commitUndo],
  );

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
      execute: (command) => run(command, { quiet: true }),
      restore: (threadIds) => dispatchUi({ type: "restore", threadIds }),
      hide: (threadIds) => dispatchUi({ type: "archive", threadIds }),
      // A traversal's own message carries no inverse — the entry it moved is
      // the record of it — so it says which button to hold out next instead.
      say: (message, offer) =>
        dispatchUi({ type: "status", status: { message, tone: "info", offer } }),
    }),
    [commitUndo, run],
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
     * `hides` says whether the rows leave the list, which decides whether the
     * cursor has to move and whether to hide them optimistically.
     */
    const bulk = (command: MailCommand, hides: boolean, label?: string) => {
      const ids = command.threadIds;
      if (ids.length === 0) return;
      if (hides) {
        const nextFocus = nextAfterRemoval(listIds, ids, ui.threadId);
        dispatch({ type: "archive", threadIds: ids });
        if (nextFocus !== ui.threadId) dispatch({ type: "thread", threadId: nextFocus });
      }
      dispatch({ type: "selection", selection: clearSelection(ui.selection) });
      void run(command, { reselectFailed: true, label });
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

      archiveSelected: () => bulk({ kind: "archive", threadIds: commandTargetIds }, true),
      trashSelected: () => bulk({ kind: "trash", threadIds: commandTargetIds }, true),
      snoozeSelected: (until) =>
        bulk(
          { kind: "snooze", threadIds: commandTargetIds, until },
          true,
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
        dispatch({ type: "star", threadIds: commandTargetIds, starred: !allStarred });
        bulk({ kind: "star", threadIds: commandTargetIds, starred: !allStarred }, false);
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
      undo: () => void runUndo(undoHost),
      redo: () => void runRedo(undoHost),

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
      setAddAccount: (open) => dispatch({ type: "addAccount", open }),
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
      syncNow: () => {
        void getDataSource()
          .syncNow()
          .catch((caught) =>
            dispatch({
              type: "status",
              status: { message: toMailboxError(caught).message, tone: "error" },
            }),
          );
      },
      reload: () => {
        setReloadKey((k) => k + 1);
        streamRef.current.refresh();
      },
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
    listIds,
    commandTargetIds,
    viewFavorite,
    threadFavorite,
    isFavorite,
    run,
    undoHost,
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
    events,
    detail,
    detailLoading,
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
