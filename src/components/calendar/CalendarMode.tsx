import { AlertTriangle, ChevronLeft, ChevronRight, Mail, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AccountId, CalendarEvent, CalendarId, EventId, Rsvp } from "@/types";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach, viewRange, type CalendarView } from "@/hooks/useMach";
import { usePreferences } from "@/components/prefs/PreferencesProvider";
import { useContacts } from "@/hooks/useContacts";
import {
  describeResult,
  getDataSource,
  type Command,
  type EventDraft,
  type EventScope,
  type Notify,
} from "@/lib/data";
import type { KeyBinding } from "@/lib/keymap";
import { assignHues, FALLBACK_FILLS, fallbackFill, type CalendarColor } from "@/lib/calendar-palette";
import { assignCalendarColors } from "@/lib/colors";
import { mergeDuplicates, type MergedEvent } from "@/lib/calendar-merge";
import {
  arrowCursor,
  inReadingOrder,
  matchEvents,
  stepCursor,
  type Arrow,
  type CursorMove,
} from "@/lib/calendar-cursor";
import { parseEventText, type ParsedEvent } from "@/lib/calendar-nlp";
import { nudge, type DragOutcome } from "@/lib/calendar-drag";
import {
  canEditEvent,
  copyDraft,
  duplicateDraft,
  formDraft,
  formPatch,
  looksRecurring,
  nextSlot,
  pasteDraft,
  requiresSeriesScope,
  rulesFor,
  type EventForm,
} from "@/lib/calendar-edit";
import { googleCalendarUrl } from "@/lib/calendar-links";
import { usePeriodWheel } from "./use-period-wheel";
import {
  DAY,
  MINUTE,
  addDays,
  fullDate,
  monthShort,
  monthYear,
  startOfDay,
} from "@/lib/time";
import { DEFAULT_EVENT_MINUTES } from "@/lib/calendar-geometry";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { MonthGrid } from "./MonthGrid";
import { TimeGrid, type EventDraft as GridDraft, type EventMove } from "./TimeGrid";
import { CalendarSidebar, calendarLabel, type CalendarSettings } from "./CalendarSidebar";
import { CalendarContextMenu, type CalendarVerbs } from "./CalendarContextMenu";
import { EventModal, type ModalTarget } from "./EventModal";
import { EventFinder } from "./EventFinder";
import { QuickCreate } from "./QuickCreate";
import { useIsDark } from "./use-is-dark";

const VIEWS: { id: CalendarView; label: string; keys: string; alias: string }[] = [
  { id: "day", label: "Day", keys: "1", alias: "d" },
  { id: "week", label: "Week", keys: "2", alias: "w" },
  { id: "month", label: "Month", keys: "3", alias: "m" },
];

const SETTINGS_KEY = "mach.calendar.settings";

/** ⇧1…⇧5 as the characters a US keyboard actually emits. */
const SHIFTED_DIGITS = ["!", "@", "#", "$", "%"];

/**
 * Where a dragged, resized or nudged event has been *put*, before the store
 * agrees that it is there.
 *
 * There is no expiry on it, and none is needed: the command layer writes the
 * local row to exactly these values before it calls Google, so a successful
 * write always produces a store that agrees, and agreement is what retires the
 * guess. A refused write deletes it outright. The only way one could outlive
 * its usefulness is a later sync moving the event to a third time, which is a
 * race with a background pass rather than a state this can be in on its own.
 */
interface PendingMove {
  start: number;
  end: number;
  allDay: boolean;
}

const DEFAULT_SETTINGS: CalendarSettings = {
  // §7: merging is the right default. It hides that a meeting exists on two
  // calendars, which the detail modal then has to say out loud — it does.
  mergeDuplicates: true,
  // §11(3): declined events are noise. Hidden, with a toggle.
  showDeclined: false,
  showWeekends: true,
};

export function CalendarMode() {
  const {
    ui,
    dispatch,
    actions,
    accounts,
    calendars,
    events: allEvents,
    visibleEvents,
    selectedEvent,
    calendarById,
  } = useMach();
  const dark = useIsDark();
  const prefs = usePreferences();
  const contacts = useContacts();

  const [settings, setSettings] = useState<CalendarSettings>(() => loadSettings());
  const [soloAccount, setSoloAccount] = useState<AccountId | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [createSeed, setCreateSeed] = useState<string | undefined>(undefined);
  const [dateOpen, setDateOpen] = useState(false);
  const [todayNonce, setTodayNonce] = useState(0);
  const [revealNonce, setRevealNonce] = useState(0);
  /** The event the modal is showing, or the slot it is creating into. */
  const [modal, setModal] = useState<ModalTarget | null>(null);
  const [busy, setBusy] = useState(false);
  /**
   * The last write Google refused, and the command that was refused.
   *
   * Kept out of `ui.status` on purpose: that message is transient by design
   * (six seconds, so the undo offer does not linger), and a failure is the one
   * thing that must not vanish before it has been read. This one stays until it
   * is dismissed or retried.
   */
  const [failure, setFailure] = useState<{ message: string; command: Command } | null>(null);
  /** Which end of the newly-paged period an arrow key wants to land on. */
  const [pendingEdge, setPendingEdge] = useState<"first" | "last" | null>(null);
  /**
   * The type-to-select bar. `restore` is what Escape puts back, so cancelling a
   * search leaves the cursor exactly where it was rather than on whichever
   * match happened to be highlighted when the user changed their mind.
   */
  const [finder, setFinder] = useState<{
    query: string;
    index: number;
    restore: EventId | null;
    /**
     * The week to come back to. A search that widened past the visible range
     * moved the view to show its match; cancelling has to undo that too, or
     * Escape leaves the user three weeks from where they started with nothing
     * on screen explaining how they got there.
     */
    restoreAnchor: number;
  } | null>(null);
  /** ⌘C parks an event here; ⌘V drops a copy of it on the anchored day. */
  const [clipboard, setClipboard] = useState<CalendarEvent | null>(null);
  /**
   * Where a dragged, resized or nudged event has been *put*, before the store
   * agrees.
   *
   * The same idea as `starOverrides` in `useMach`, and for the same reason: the
   * UI never waits on Google. A drop used to leave the block sitting at its old
   * time for the length of an IPC round trip, a Google call and a refetch —
   * a quarter of a second on a good day, and a visible snap-back on a bad one,
   * which reads as "the drag did not work" rather than as "it is saving".
   *
   * Kept here rather than in `useMach` deliberately: it is a fact about the
   * calendar surface, it is discarded the moment the surface unmounts, and
   * nothing else in the app needs to know an event is mid-flight.
   */
  const [pendingMoves, setPendingMoves] = useState<Record<EventId, PendingMove>>({});

  const modalOpen = modal !== null;
  const finderOpen = finder !== null;
  const active =
    ui.mode === "calendar" && !ui.paletteOpen && !createOpen && !modalOpen && !finderOpen;

  /**
   * Two-finger navigation between periods. An addition to `n`/`p`/`j`/`k`/`t`
   * and never a replacement: it is gated on the same `active` as the key
   * bindings are, so the two surfaces agree about when the calendar is
   * listening, and nothing on the calendar is reachable by gesture alone.
   *
   * Week and day give horizontal to the period and keep vertical for the hour
   * grid, which is real scrollable content and the reason the grid exists.
   * Month has nothing to scroll, so vertical moves a month there as well —
   * which is the gesture people reach for first when a grid fills the window.
   */
  const grid = useRef<HTMLDivElement>(null);
  usePeriodWheel({
    ref: grid,
    vertical: ui.calendarView === "month",
    enabled: active,
    // The same call `n`, `p` and the arrow buttons make. `shiftPeriod` reads the
    // anchor out of the render it was built in, which would collapse two
    // gestures into one if React had not committed between them; it always has,
    // because a wheel stream puts a task boundary between every pair of events
    // and the batch flushes on the microtask before it. Four quick flicks move
    // four weeks with nothing forcing the issue.
    onStep: (delta) => actions.shiftPeriod(delta),
  });

  useEffect(() => {
    window.localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
  }, [settings]);

  const { start, days: dayCount } = viewRange(ui.calendarView, ui.anchor, prefs.weekStartsOn);
  const allDays = useMemo(
    () => Array.from({ length: dayCount }, (_, i) => addDays(start, i)),
    [start.getTime(), dayCount],
  );
  const days = useMemo(() => {
    if (settings.showWeekends || ui.calendarView !== "week") return allDays;
    return allDays.filter((day) => day.getDay() !== 0 && day.getDay() !== 6);
  }, [allDays, settings.showWeekends, ui.calendarView]);

  const rangeStart = startOfDay(days[0]).getTime();
  const rangeEnd = startOfDay(days[days.length - 1]).getTime() + DAY;

  // Google's `backgroundColor`, verbatim, so a calendar is the colour its owner
  // actually chose — falling back to the id hash for the ones with no colour,
  // which keeps them stable between sessions however sync orders things.
  const colors = useMemo(
    () => assignCalendarColors(calendars, FALLBACK_FILLS, assignHues),
    [calendars],
  );
  const colorFor = useCallback(
    (id: CalendarId): CalendarColor => colors.get(id) ?? fallbackFill(0),
    [colors],
  );

  /**
   * The events as the user has just left them.
   *
   * A guess is applied only while it still *differs* from the store, which is
   * the same rule `starOverrides` uses: the moment sync catches up, the
   * override is a no-op and the row renders from the truth. The effect below
   * then drops it, so the map cannot accumulate.
   */
  const settledEvents = useMemo(() => {
    if (Object.keys(pendingMoves).length === 0) return visibleEvents;
    return visibleEvents.map((event) => {
      const move = pendingMoves[event.id];
      if (!move) return event;
      if (move.start === event.start && move.end === event.end && move.allDay === event.allDay) {
        return event;
      }
      return { ...event, start: move.start, end: move.end, allDay: move.allDay };
    });
  }, [visibleEvents, pendingMoves]);

  // Drop guesses the store has caught up with, and guesses about events that
  // are no longer there at all. Without this the map grows for the life of the
  // session and every render pays for it.
  useEffect(() => {
    const ids = Object.keys(pendingMoves);
    if (ids.length === 0) return;
    const byId = new Map(visibleEvents.map((e) => [e.id, e] as const));
    const stale = ids.filter((key) => {
      const id = Number(key);
      const event = byId.get(id);
      if (!event) return true;
      const move = pendingMoves[id];
      return move.start === event.start && move.end === event.end && move.allDay === event.allDay;
    });
    if (stale.length === 0) return;
    setPendingMoves((current) => {
      const next = { ...current };
      for (const key of stale) delete next[Number(key)];
      return next;
    });
  }, [visibleEvents, pendingMoves]);

  const inRange = useMemo(() => {
    const soloed = soloAccount;
    return settledEvents.filter((event) => {
      if (event.start >= rangeEnd || event.end <= rangeStart) return false;
      if (soloed !== null && event.accountId !== soloed) return false;
      if (!settings.showDeclined && event.rsvp === "declined") return false;
      return true;
    });
  }, [settledEvents, rangeStart, rangeEnd, soloAccount, settings.showDeclined]);

  const merged = useMemo(
    () =>
      mergeDuplicates(inRange, {
        order: calendars.map((c) => c.id),
        enabled: settings.mergeDuplicates,
      }),
    [inRange, calendars, settings.mergeDuplicates],
  );

  const mergedById = useMemo(() => {
    const map = new Map<EventId, MergedEvent>();
    for (const item of merged) map.set(item.event.id, item);
    return map;
  }, [merged]);

  /** Tab order: the blocks as they read down the week. */
  const ordered = useMemo(() => {
    const byId = new Map(merged.map((item) => [item.event.id, item] as const));
    return inReadingOrder(merged.map((item) => item.event)).map((event) => byId.get(event.id)!);
  }, [merged]);

  /**
   * The same blocks as plain events — what every cursor and match decision is
   * made against, so the keyboard can only ever land on something drawn.
   */
  const inView = useMemo(() => ordered.map((item) => item.event), [ordered]);

  /**
   * Apply whatever the cursor resolved to.
   *
   * A `page` outcome cannot select anything yet — the events of the period it
   * is moving to are in `allEvents`, but `ordered` is derived from the anchor
   * and only recomputes on the next render. So it shifts the range and parks
   * which edge to land on; the effect below picks it up once the new window
   * exists. That is what stops the arrow keys from dead-ending at Sunday.
   */
  const applyCursor = useCallback(
    (move: CursorMove) => {
      if (move.kind === "none") {
        actions.setStatus("Nothing to select", "info");
        return;
      }
      if (move.kind === "page") {
        actions.shiftPeriod(move.delta);
        setPendingEdge(move.edge);
        return;
      }
      dispatch({ type: "event", eventId: move.id });
      setRevealNonce((n) => n + 1);
    },
    [actions, dispatch],
  );

  const step = useCallback(
    (delta: 1 | -1) => applyCursor(stepCursor(inView, ui.eventId, delta)),
    [applyCursor, inView, ui.eventId],
  );

  const arrow = useCallback(
    (direction: Arrow) => applyCursor(arrowCursor(inView, ui.eventId, direction)),
    [applyCursor, inView, ui.eventId],
  );

  // Land on the edge of the period an arrow key paged into. Runs after the
  // anchor change has re-derived the window, which is the whole reason the
  // intent had to be parked rather than acted on immediately.
  useEffect(() => {
    if (pendingEdge === null) return;
    if (ordered.length === 0) {
      // An empty week is not a reason to stop: keep going the way the key was
      // pointing rather than stranding the cursor on nothing.
      setPendingEdge(null);
      return;
    }
    const target = pendingEdge === "first" ? ordered[0] : ordered[ordered.length - 1];
    dispatch({ type: "event", eventId: target.event.id });
    setRevealNonce((n) => n + 1);
    setPendingEdge(null);
  }, [pendingEdge, ordered, dispatch]);

  const goToday = useCallback(() => {
    actions.goToday();
    setTodayNonce((n) => n + 1);
  }, [actions]);

  /* ---------------------------------------------------------------------- */
  /* The write path                                                          */
  /* ---------------------------------------------------------------------- */

  /**
   * Dispatch a calendar command and reconcile.
   *
   * The command layer writes to SQLite before it talks to Google, so by the
   * time this resolves the local store is already right — but `useMach` loads
   * the calendar window once per anchor and has no idea a row changed. Asking
   * it to refetch is the seam: `actions.reloadEvents()` refetches just that
   * window. It used to be `actions.reload()`, which also refetched accounts,
   * labels, calendars, the sync snapshot and the entire thread list — a lot of
   * work, and a visible stutter, to redraw one block fifteen minutes lower.
   *
   * `undo` is threaded into the status bar exactly the way the mail commands
   * do it, so `z` reverses a drag as readily as it reverses an archive.
   */
  const run = useCallback(
    async (command: Command) => {
      setBusy(true);
      setFailure(null);
      try {
        const result = await getDataSource().execute(command);
        // One dispatch, carrying the inverse: `z` reads `ui.status.undo`, so a
        // plain `setStatus` here would say the right thing and then quietly
        // make the last action un-undoable.
        dispatch({
          type: "status",
          status: {
            message: describeResult(result),
            undo: result.ok ? result.undo : undefined,
            tone: result.ok ? "info" : "error",
          },
        });
        // The status bar is not enough on its own, and this is the whole reason
        // a stale binary read as "I tried to create an event and nothing
        // happened". That rail is 24px tall, it truncates, and it clears itself
        // after six seconds — a failure there is indistinguishable from a
        // success you looked away from. A failed write now also parks a banner
        // that stays until it is dismissed or retried, and keeps the modal open
        // on top of the values that did not save.
        if (!result.ok) setFailure({ message: describeResult(result), command });
        actions.reloadEvents();
        return result;
      } catch (error) {
        const message = error instanceof Error ? error.message : "Not saved";
        dispatch({ type: "status", status: { message, tone: "error" } });
        setFailure({ message, command });
        return null;
      } finally {
        setBusy(false);
      }
    },
    [actions, dispatch],
  );

  /** Whether Google would accept a write to this event at all. */
  const accountEmails = useMemo(() => accounts.map((a) => a.email), [accounts]);
  const editable = useCallback(
    // The calendar's own access role is part of the same decision: a `reader`
    // subscription refuses every write whoever organized the event.
    (event: CalendarEvent) =>
      canEditEvent(event, accountEmails, calendarById(event.calendarId)?.accessRole),
    [accountEmails, calendarById],
  );

  /**
   * Refuse a write to an event that is not ours, and say why.
   *
   * Google would refuse it too, a round trip later and in less useful words.
   * Drag, resize and the arrow-key nudges all funnel through here because none
   * of them go anywhere near the modal, which is where the lock is visible.
   */
  const guardEdit = useCallback(
    (event: CalendarEvent) => {
      if (editable(event)) return true;
      const who = event.organizer?.email;
      actions.setStatus(
        who
          ? `Only ${who} can change “${event.title}”`
          : `Only the organizer can change “${event.title}”`,
        "error",
      );
      return false;
    },
    [editable, actions],
  );

  /** The calendar a new event lands on: the one in view, else the first. */
  const defaultCalendarId = useMemo(() => {
    const visible = calendars.filter((c) => !ui.hiddenCalendars.includes(c.id));
    return (visible[0] ?? calendars[0])?.id ?? null;
  }, [calendars, ui.hiddenCalendars]);

  const accountForCalendar = useCallback(
    (id: CalendarId) => calendarById(id)?.accountId ?? accounts[0]?.id ?? null,
    [calendarById, accounts],
  );

  const create = useCallback(
    async (draft: EventDraft, calendarId: CalendarId | null) => {
      const target = calendarId ?? defaultCalendarId;
      const accountId = target === null ? null : accountForCalendar(target);
      if (target === null || accountId === null) {
        actions.setStatus("No calendar to create on", "error");
        return null;
      }
      return run({ kind: "createEvent", accountId, calendarId: target, draft });
    },
    [run, defaultCalendarId, accountForCalendar, actions],
  );

  /**
   * A drag, a resize or a keyboard nudge — the same command either way, and the
   * same optimistic shape.
   *
   * The block is drawn at its new time *before* the command is dispatched, so
   * the drop is instant however slow Google is. On failure the guess is dropped
   * and the block snaps back to where it was — visibly, and with the banner
   * saying why. A silent snap-back is the worst of both worlds: it looks like
   * the drag missed, and it teaches the user that dragging is unreliable rather
   * than that this particular write was refused.
   */
  const applyMove = useCallback(
    (eventId: EventId, outcome: DragOutcome, allDay: boolean) => {
      const event = allEvents.find((e) => e.id === eventId);
      if (event && !guardEdit(event)) return;

      setPendingMoves((current) => ({
        ...current,
        [eventId]: { start: outcome.start, end: outcome.end, allDay },
      }));

      void run({
        kind: "updateEvent",
        eventId,
        patch: { startTs: outcome.start, endTs: outcome.end, isAllDay: allDay },
        scope: "this",
      }).then((result) => {
        if (result?.ok) return;
        // Google refused it, or never answered. `run` has already put the
        // reason on screen; this is the half that puts the block back.
        setPendingMoves((current) => {
          if (!(eventId in current)) return current;
          const next = { ...current };
          delete next[eventId];
          return next;
        });
      });
    },
    [run, allEvents, guardEdit],
  );

  /**
   * A block was dropped.
   *
   * With alt held it is a copy: the original stays where it was and a new event
   * is created at the time the ghost landed on, on the same calendar. That is a
   * create, not a move, so it takes the create path whole — including the
   * refusal that comes back when the calendar is one you can only read.
   */
  const onGridMove = useCallback(
    (move: EventMove) => {
      if (move.copy) {
        const source = allEvents.find((e) => e.id === move.eventId);
        if (!source) return;
        void create(copyDraft(source, { start: move.start, end: move.end }), source.calendarId);
        return;
      }
      applyMove(move.eventId, { start: move.start, end: move.end }, false);
    },
    [applyMove, allEvents, create],
  );

  /**
   * Nudge the focused event from the keyboard.
   *
   * All-day events move in UTC days because that is how the store pins them;
   * everything else moves in local calendar days, so a 9am meeting stays at 9am
   * across a DST boundary.
   */
  const nudgeSelected = useCallback(
    (
      action:
        | { kind: "move"; axis: "time"; steps: number }
        | { kind: "move"; axis: "day"; days: number }
        | { kind: "resize"; edge: "start" | "end"; steps: number },
    ) => {
      // The *drawn* event, not the stored one. They differ for exactly as long
      // as an optimistic move is in flight, and nudging from the stored copy
      // made the second of two quick presses a no-op: it recomputed "fifteen
      // minutes after 1pm" from a row that still said 1pm, and arrived back at
      // the time the first press had already moved it to.
      const event = settledEvents.find((e) => e.id === ui.eventId) ?? selectedEvent;
      if (!event) {
        actions.setStatus("Pick an event first", "info");
        return;
      }
      if (event.allDay) {
        if (action.kind !== "move" || action.axis !== "day") {
          actions.setStatus("All-day event — no time to move", "info");
          return;
        }
        const shift = action.days * DAY;
        applyMove(event.id, { start: event.start + shift, end: event.end + shift }, true);
        return;
      }
      const outcome = nudge(
        { start: event.start, end: event.end, dayStart: startOfDay(event.start).getTime() },
        action,
      );
      applyMove(event.id, outcome, false);
    },
    [selectedEvent, settledEvents, ui.eventId, applyMove, actions],
  );

  /* ---------------------------------------------------------------------- */
  /* Type to select                                                          */
  /* ---------------------------------------------------------------------- */

  /**
   * What the typed query matches, and whether it had to look past the view.
   *
   * The visible week is searched first and wins outright when it has anything,
   * because "the meeting I can see" is what someone typing at a calendar
   * usually means. Only when *nothing* on screen matches does it widen to the
   * whole loaded window — roughly four months around the anchor, already in
   * memory, so widening costs a filter rather than a fetch.
   *
   * Widening rather than reporting "no matches" is a deliberate call: the
   * alternative is a user who types "dentist", sees nothing, and has to guess
   * which week to page to before they can type it again. The bar says out loud
   * when a match came from elsewhere, and the grid follows the highlight there,
   * so nothing moves without an explanation.
   */
  const finderMatches = useMemo(() => {
    const query = finder?.query ?? "";
    if (!query.trim()) return { rows: [] as CalendarEvent[], widened: false };
    const here = matchEvents(inView, query);
    if (here.length > 0) return { rows: here, widened: false };
    const anywhere = matchEvents(visibleEvents, query);
    return { rows: anywhere, widened: anywhere.length > 0 };
  }, [finder?.query, inView, visibleEvents]);

  const finderMatch = finder ? (finderMatches.rows[finder.index] ?? null) : null;

  /** Everything the query did *not* match, so the grid can dim it. */
  const dimIds = useMemo(() => {
    if (!finder || finderMatches.rows.length === 0) return undefined;
    const matched = new Set(finderMatches.rows.map((e) => e.id));
    return new Set(inView.filter((e) => !matched.has(e.id)).map((e) => e.id));
  }, [finder, finderMatches, inView]);

  const openFinder = useCallback(() => {
    setFinder({ query: "", index: 0, restore: ui.eventId, restoreAnchor: ui.anchor });
  }, [ui.eventId, ui.anchor]);

  const closeFinder = useCallback(
    (restore: boolean) => {
      setFinder((current) => {
        if (current && restore) {
          dispatch({ type: "event", eventId: current.restore });
          dispatch({ type: "anchor", anchor: current.restoreAnchor });
        }
        return null;
      });
    },
    [dispatch],
  );

  // Follow the highlighted match: select it, and bring the view to it when the
  // match came from outside the week on screen. Doing this as the user types is
  // what makes the matches "highlight in place" rather than being a list they
  // then have to act on.
  useEffect(() => {
    if (!finder || !finderMatch) return;
    if (finderMatch.id !== ui.eventId) {
      dispatch({ type: "event", eventId: finderMatch.id });
    }
    if (finderMatch.start < rangeStart || finderMatch.start >= rangeEnd) {
      dispatch({ type: "anchor", anchor: finderMatch.start });
    }
    setRevealNonce((n) => n + 1);
    // Keyed on the match, not on the whole finder: re-running on every
    // keystroke that does not change the answer would fight the grid's scroll.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [finderMatch?.id]);

  const openEvent = useCallback(
    (id: EventId) => {
      const event = allEvents.find((e) => e.id === id);
      if (!event) return;
      dispatch({ type: "event", eventId: id });
      setModal({ mode: "view", event });
    },
    [allEvents, dispatch],
  );

  // Keep the open modal pointed at the freshest copy of its event: a reload
  // after a save replaces the object, and a stale one would show old values.
  useEffect(() => {
    if (modal?.mode !== "view") return;
    const fresh = allEvents.find((e) => e.id === modal.event.id);
    if (!fresh) {
      setModal(null);
    } else if (fresh !== modal.event) {
      setModal({ mode: "view", event: fresh });
    }
  }, [allEvents, modal]);

  const closeModal = useCallback(() => {
    setModal(null);
    // Closing the panel is an acknowledgement of whatever it was showing,
    // including the failure — leaving the banner behind afterwards would nag
    // about something the user has already walked away from.
    setFailure(null);
    // Focus goes back to the block it came from, which is where the keyboard
    // cursor already is — the grid scrolls it into view.
    setRevealNonce((n) => n + 1);
  }, []);

  const openCreate = useCallback(
    (slot?: { start: number; end: number; allDay?: boolean; title?: string }) => {
      const start = slot?.start ?? defaultSlot(selectedEvent, ui.anchor);
      setModal({
        mode: "create",
        start,
        end: slot?.end ?? start + DEFAULT_EVENT_MINUTES * MINUTE,
        allDay: slot?.allDay,
        title: slot?.title,
      });
    },
    [selectedEvent, ui.anchor],
  );

  /**
   * Save the modal's form.
   *
   * The modal used to close on the way *in* to the command, which is how a
   * refused save came to look exactly like a successful one: the panel went
   * away, the grid did not change, and a red line appeared in a rail nobody was
   * looking at. It now closes only once the write has actually landed — on a
   * failure it stays up, still holding everything that was typed, with the
   * reason above the fields.
   */
  const saveForm = useCallback(
    (form: EventForm, scope: EventScope) => {
      const event = modal?.mode === "view" ? modal.event : null;
      if (!event) {
        const draft = formDraft(form);
        if ("error" in draft) {
          actions.setStatus(draft.error, "error");
          return;
        }
        void create(draft, form.calendarId).then((result) => {
          if (result?.ok) setModal(null);
        });
        return;
      }
      if (!guardEdit(event)) return;

      const patch = formPatch(event, form);
      const moved = form.calendarId !== event.calendarId;

      const move = () => {
        const accountId = accountForCalendar(form.calendarId);
        if (accountId === null) return;
        void run({
          kind: "moveEvent",
          eventId: event.id,
          accountId,
          calendarId: form.calendarId,
        }).then((result) => {
          if (result?.ok) setModal(null);
        });
      };

      if (!patch) {
        if (moved) move();
        else setModal(null);
        return;
      }
      // Changing how an event repeats is a property of the series master, and
      // `events.patch` on an expanded occurrence refuses a `recurrence` key
      // outright. The command layer says so in words; catching it here means
      // the user is never asked a question ("this one, or all of them?") whose
      // first answer cannot work.
      const effective: EventScope = requiresSeriesScope(patch) ? "all" : scope;

      // A move is insert-into-destination then delete-from-source, so it re-reads
      // the row the patch just wrote. If the patch did *not* land — Google refused
      // it, the account needs reauthorizing — the row was rolled back, and moving
      // on top of that would copy the pre-edit event to the other calendar and
      // report success for a save that never happened.
      void run({ kind: "updateEvent", eventId: event.id, patch, scope: effective }).then(
        (result) => {
          if (!result?.ok) return;
          if (moved) move();
          else setModal(null);
        },
      );
    },
    [modal, run, create, accountForCalendar, actions, guardEdit],
  );

  const deleteEvent = useCallback(
    /*
     * `notify` is the guests' half of a deletion, and it is not a detail: a
     * meeting that vanishes from your calendar and stays on everybody else's is
     * the failure this whole change exists to end. Absent means tell them —
     * the same default Google Calendar's own delete has.
     */
    (id: EventId, scope: EventScope, notify?: Notify) => {
      const event = allEvents.find((e) => e.id === id);
      if (event && !guardEdit(event)) return;
      void run({ kind: "deleteEvent", eventId: id, scope, notify }).then((result) => {
        if (!result?.ok) return;
        setModal(null);
        dispatch({ type: "event", eventId: null });
      });
    },
    [run, dispatch, allEvents, guardEdit],
  );

  const duplicate = useCallback(
    (event: CalendarEvent) => {
      setModal(null);
      void create(duplicateDraft(event), event.calendarId);
    },
    [create],
  );

  const rsvp = useCallback(
    (id: EventId, response: Rsvp) => {
      void run({ kind: "rsvp", eventId: id, response });
    },
    [run],
  );

  const onGridDraft = useCallback(
    (draft: GridDraft, intent: "save" | "expand") => {
      if (intent === "expand") {
        openCreate({ start: draft.start, end: draft.end, title: draft.title });
        return;
      }
      create(
        {
          title: draft.title,
          startTs: draft.start,
          endTs: draft.end,
          isAllDay: false,
          attendees: [],
          recurrence: [],
        },
        null,
      );
    },
    [create, openCreate],
  );

  const onQuickCreate = useCallback(
    (parsed: ParsedEvent) => {
      setCreateOpen(false);
      setCreateSeed(undefined);
      if (parsed.start === null) {
        actions.setStatus("No date in that — try “Standup tomorrow 2pm”", "error");
        return;
      }
      dispatch({ type: "anchor", anchor: parsed.start });
      const end = parsed.end ?? parsed.start + DEFAULT_EVENT_MINUTES * MINUTE;
      create(
        {
          title: parsed.title,
          startTs: parsed.start,
          endTs: end,
          isAllDay: parsed.allDay,
          location: parsed.location,
          attendees: parsed.invitees.map((email) => ({ name: email, email })),
          // The parser reports the phrase ("every Tuesday"); the rule comes
          // from the same choice list the modal offers, anchored on the start.
          recurrence: parsed.recurrence ? rulesFor("weekly", parsed.start) : [],
          reminderMinutes:
            parsed.alertMinutes === undefined ? undefined : [parsed.alertMinutes],
        },
        parsed.calendarId ?? null,
      );
    },
    [actions, dispatch, create],
  );

  const toggleCalendarAt = useCallback(
    (index: number) => {
      const calendar = calendars[index];
      if (calendar) dispatch({ type: "toggleCalendar", calendarId: calendar.id });
    },
    [calendars, dispatch],
  );

  const soloAccountAt = useCallback(
    (index: number) => {
      const account = accounts[index];
      if (!account) return;
      setSoloAccount((current) => (current === account.id ? null : account.id));
    },
    [accounts],
  );

  const openExternal = useCallback(
    (url: string) => {
      void getDataSource()
        .openExternal(url)
        .catch(() => actions.setStatus("Could not open that link", "error"));
    },
    [actions],
  );

  /* ---------------------------------------------------------------------- */
  /* The verbs, once                                                         */
  /* ---------------------------------------------------------------------- */

  /*
   * Everything below is a thing the calendar does to an event, named once and
   * called from both the keyboard and the right-click menu. They used to be
   * written inline in the binding list, which is fine while the keyboard is the
   * only way to reach them and becomes a second implementation the moment it is
   * not — see the note at the top of `CalendarContextMenu`.
   */

  /** ⌫ — delete it, unless it repeats, in which case ask which occurrences. */
  const requestDelete = useCallback(
    (event: CalendarEvent) => {
      if (looksRecurring(event, inRange)) setModal({ mode: "view", event });
      else deleteEvent(event.id, "this");
    },
    [deleteEvent, inRange],
  );

  const copyEvent = useCallback(
    (event: CalendarEvent) => {
      setClipboard(event);
      actions.setStatus(`Copied “${event.title}”`, "info");
    },
    [actions],
  );

  const openInGoogle = useCallback(
    (event: CalendarEvent) =>
      openExternal(
        googleCalendarUrl(event, accounts.find((a) => a.id === event.accountId)?.email),
      ),
    [accounts, openExternal],
  );

  /**
   * The event as it is *drawn*, which is what a click or the cursor is on.
   *
   * The same fallback `nudgeSelected` uses: an optimistic move is in the
   * settled copy and not yet in the store, and a menu that read the store would
   * be about the block's old time.
   */
  const eventById = useCallback(
    (id: EventId) => settledEvents.find((e) => e.id === id) ?? allEvents.find((e) => e.id === id),
    [settledEvents, allEvents],
  );

  /**
   * Whether this event's calendar would accept a *new* event on it.
   *
   * A separate question from `canEditEvent`, which is about this event: a
   * stranger's invitation on a calendar you own cannot be edited and can
   * perfectly well be duplicated, while nothing at all can be created on a
   * calendar you only subscribe to.
   */
  const creatableOn = useCallback(
    (event: CalendarEvent) => {
      const role = calendarById(event.calendarId)?.accessRole;
      return role !== "reader" && role !== "freeBusyReader";
    },
    [calendarById],
  );

  const verbs = useMemo<CalendarVerbs>(
    () => ({
      open: (event) => openEvent(event.id),
      remove: requestDelete,
      duplicate,
      copy: copyEvent,
      openInGoogle,
      rsvp: (event, response) => rsvp(event.id, response),
      createAt: (slot) => openCreate(slot),
    }),
    [copyEvent, duplicate, openCreate, openEvent, openInGoogle, requestDelete, rsvp],
  );

  /* ---------------------------------------------------------------------- */
  /* Keyboard                                                                */
  /* ---------------------------------------------------------------------- */

  const withEvent = useCallback(
    (what: (event: CalendarEvent) => void) => () => {
      if (!selectedEvent) {
        actions.setStatus("Pick an event first", "info");
        return;
      }
      what(selectedEvent);
    },
    [selectedEvent, actions],
  );

  const bindings = useMemo<KeyBinding[]>(
    () => [
      // Views — Google's digits, Notion's letters, both live.
      ...VIEWS.flatMap((view) => [
        {
          keys: view.keys,
          group: "Calendar",
          description: `${view.label} view`,
          when: () => active,
          handler: () => actions.setCalendarView(view.id),
        },
        {
          keys: view.alias,
          when: () => active,
          handler: () => actions.setCalendarView(view.id),
        },
      ]),

      // Range navigation — letters only.
      //
      // These are Google Calendar's own: `n`/`j` forward, `p`/`k` back, `t` for
      // today. They used to have the arrow keys too, and that was the problem:
      // the gesture every hand reaches for first moved the *week* instead of
      // the cursor, and the events themselves were reachable only by Tab. The
      // letters are unchanged, so nothing anyone had learned was taken away;
      // the arrows moved to the events, below.
      {
        keys: "j",
        group: "Calendar",
        description: "Next period",
        when: () => active,
        handler: () => actions.shiftPeriod(1),
      },
      { keys: "n", when: () => active, handler: () => actions.shiftPeriod(1) },
      {
        keys: "k",
        group: "Calendar",
        description: "Previous period",
        when: () => active,
        handler: () => actions.shiftPeriod(-1),
      },
      { keys: "p", when: () => active, handler: () => actions.shiftPeriod(-1) },
      {
        keys: "t",
        group: "Calendar",
        description: "Today",
        when: () => active,
        handler: goToday,
      },
      {
        keys: "g d",
        group: "Calendar",
        description: "Go to date",
        when: () => active,
        handler: () => setDateOpen(true),
      },

      // Moving between events. Tab is the platform's "next thing"; the arrows
      // are the reflex. Up and down are the same step as Tab — down a column
      // and on into the next occupied day — while left and right cross to the
      // nearest event on another day at about the same time, which is how a
      // week of standups reads as a row rather than as seven separate columns.
      {
        keys: "tab",
        group: "Calendar",
        description: "Next event",
        when: () => active,
        handler: () => step(1),
      },
      {
        keys: "shift+tab",
        group: "Calendar",
        description: "Previous event",
        when: () => active,
        handler: () => step(-1),
      },
      {
        keys: "down",
        group: "Calendar",
        description: "Next event ↓ ↑, nearest event on another day ← →",
        when: () => active,
        handler: () => arrow("down"),
      },
      { keys: "up", when: () => active, handler: () => arrow("up") },
      { keys: "right", when: () => active, handler: () => arrow("right") },
      { keys: "left", when: () => active, handler: () => arrow("left") },

      // Type to select. `/` is Gmail's search key and the palette claims it
      // globally at priority 200 — scoped `when: () => !open`, which is true
      // here, so this has to outrank it rather than merely coexist. In every
      // other mode the palette still gets the key.
      {
        keys: "/",
        priority: 220,
        group: "Calendar",
        description: "Find an event by name",
        when: () => active,
        handler: () => openFinder(),
      },

      // Creating.
      {
        keys: "c",
        group: "Event",
        description: "Create — type it in words",
        when: () => active,
        handler: () => {
          setCreateSeed(undefined);
          setCreateOpen(true);
        },
      },
      {
        keys: "shift+c",
        group: "Event",
        description: "Create — full editor",
        when: () => active,
        handler: () => openCreate(),
      },

      // Opening and editing the focused event.
      {
        keys: "e",
        group: "Event",
        description: "Open the event",
        when: () => active,
        handler: () => (ui.eventId === null ? step(1) : openEvent(ui.eventId)),
      },
      {
        keys: "enter",
        when: () => active,
        handler: () => (ui.eventId === null ? step(1) : openEvent(ui.eventId)),
      },
      {
        keys: "backspace",
        group: "Event",
        description: "Delete the event",
        when: () => active,
        handler: withEvent(requestDelete),
      },
      {
        keys: "delete",
        when: () => active,
        handler: withEvent(requestDelete),
      },

      // Moving and resizing without a mouse. Shift slides the whole event,
      // Alt moves one edge — the same two gestures the pointer offers.
      {
        keys: "shift+down",
        group: "Event",
        description: "Move 15 minutes later",
        when: () => active,
        handler: () => nudgeSelected({ kind: "move", axis: "time", steps: 1 }),
      },
      {
        keys: "shift+up",
        group: "Event",
        description: "Move 15 minutes earlier",
        when: () => active,
        handler: () => nudgeSelected({ kind: "move", axis: "time", steps: -1 }),
      },
      {
        keys: "shift+right",
        group: "Event",
        description: "Move to the next day",
        when: () => active,
        handler: () => nudgeSelected({ kind: "move", axis: "day", days: 1 }),
      },
      {
        keys: "shift+left",
        group: "Event",
        description: "Move to the previous day",
        when: () => active,
        handler: () => nudgeSelected({ kind: "move", axis: "day", days: -1 }),
      },
      {
        keys: "alt+down",
        group: "Event",
        description: "Make it 15 minutes longer",
        when: () => active,
        handler: () => nudgeSelected({ kind: "resize", edge: "end", steps: 1 }),
      },
      {
        keys: "alt+up",
        group: "Event",
        description: "Make it 15 minutes shorter",
        when: () => active,
        handler: () => nudgeSelected({ kind: "resize", edge: "end", steps: -1 }),
      },
      {
        keys: "shift+alt+up",
        group: "Event",
        description: "Start 15 minutes earlier",
        when: () => active,
        handler: () => nudgeSelected({ kind: "resize", edge: "start", steps: -1 }),
      },
      {
        keys: "shift+alt+down",
        group: "Event",
        description: "Start 15 minutes later",
        when: () => active,
        handler: () => nudgeSelected({ kind: "resize", edge: "start", steps: 1 }),
      },

      // Copy, paste, duplicate, and out to Google.
      // Gated on `active` alone, not on there being a selection or a clipboard.
      // `?` snapshots the *live* bindings, so a binding that gates itself off
      // until you have already used it can never be discovered from the sheet —
      // which is the one thing the sheet is for. The handlers say what is
      // missing instead, the way every other event binding here does.
      {
        keys: "mod+c",
        group: "Event",
        description: "Copy the event",
        when: () => active,
        handler: withEvent(copyEvent),
      },
      {
        keys: "mod+v",
        group: "Event",
        description: "Paste onto the day in view",
        when: () => active,
        handler: () => {
          if (!clipboard) {
            actions.setStatus("Nothing copied", "info");
            return;
          }
          create(pasteDraft(clipboard, ui.anchor), clipboard.calendarId);
        },
      },
      {
        keys: "shift+d",
        group: "Event",
        description: "Duplicate the event",
        when: () => active,
        handler: withEvent(duplicate),
      },
      {
        keys: "o",
        group: "Event",
        description: "Open in Google Calendar",
        when: () => active,
        handler: withEvent(openInGoogle),
      },

      {
        keys: "z",
        group: "Event",
        description: "Undo",
        when: () => active,
        handler: () => actions.undo(),
      },
      {
        keys: "escape",
        priority: 10,
        when: () => active && (ui.eventId !== null || dateOpen),
        handler: () => {
          setDateOpen(false);
          dispatch({ type: "event", eventId: null });
        },
      },
      {
        keys: "escape",
        priority: 110,
        allowInInput: true,
        when: () => createOpen,
        handler: () => setCreateOpen(false),
      },
      {
        // Above the palette's own `allowInInput` Escape at 100. The finder's
        // input has an Escape handler of its own, and it never sees the key:
        // the keymap listens in the capture phase, so whatever it decides
        // happens first. This is that decision.
        keys: "escape",
        priority: 230,
        allowInInput: true,
        when: () => finderOpen,
        handler: () => closeFinder(true),
      },

      // Multi-account (§7). The brief asks for ⌘1–9 here, but ⌘1/⌘2 already mean
      // "switch mode" app-wide and on macOS ⌘<digit> means "switch view" in every
      // app on the platform. Visibility moves to a `v <digit>` sequence instead, in the
      // spirit of `g d`.
      ...Array.from({ length: 9 }, (_, i) => ({
        keys: `v ${i + 1}`,
        group: i === 0 ? "Calendars" : undefined,
        description: i === 0 ? "Show or hide calendar 1–9" : undefined,
        when: () => active,
        handler: () => toggleCalendarAt(i),
      })),
      // ⇧⌘1–5 solos an account. The chord has to be written as the *shifted
      // character* — the dispatcher records shift only for alphabetic keys, so
      // "shift+mod+1" would never match anything a keyboard can produce, while
      // ⇧⌘1 arrives as ⌘! on a US layout. `s <digit>` is the layout-independent
      // twin, and the one the shortcut sheet advertises.
      ...SHIFTED_DIGITS.map((glyph, i) => ({
        keys: `mod+${glyph}`,
        priority: 20,
        when: () => active,
        handler: () => soloAccountAt(i),
      })),
      ...Array.from({ length: 5 }, (_, i) => ({
        keys: `s ${i + 1}`,
        group: i === 0 ? "Calendars" : undefined,
        description: i === 0 ? "Show only account 1–5" : undefined,
        when: () => active,
        handler: () => soloAccountAt(i),
      })),
    ],
    [
      active,
      actions,
      clipboard,
      copyEvent,
      create,
      dateOpen,
      dispatch,
      duplicate,
      goToday,
      modalOpen,
      nudgeSelected,
      openCreate,
      openEvent,
      openInGoogle,
      requestDelete,
      createOpen,
      arrow,
      closeFinder,
      finderOpen,
      openFinder,
      soloAccountAt,
      step,
      toggleCalendarAt,
      ui.eventId,
      ui.mode,
      ui.paletteOpen,
      ui.anchor,
      withEvent,
    ],
  );

  useKeyBindings(bindings);

  const title =
    ui.calendarView === "day"
      ? fullDate(days[0].getTime())
      : ui.calendarView === "month"
        ? monthYear(ui.anchor)
        : `${monthShort(days[0])} ${days[0].getDate()} – ${monthShort(days[days.length - 1])} ${days[days.length - 1].getDate()}, ${days[days.length - 1].getFullYear()}`;

  const modalEvent = modal?.mode === "view" ? modal.event : null;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex h-8 shrink-0 items-center gap-2 border-b border-border px-2">
        {/* The way back to mail. The rail states the app's two surfaces, but
            the rail is inert here — so calendar mode carries its own return,
            and the trip is never keyboard-only. */}
        <Button size="sm" title="Mail — ⌘1" onClick={() => actions.setMode("mail")}>
          <Mail size={13} strokeWidth={1.75} />
          Mail
        </Button>
        <Button size="icon" title="Previous (k)" onClick={() => actions.shiftPeriod(-1)}>
          <ChevronLeft size={14} strokeWidth={1.75} />
        </Button>
        <Button size="icon" title="Next (j)" onClick={() => actions.shiftPeriod(1)}>
          <ChevronRight size={14} strokeWidth={1.75} />
        </Button>
        <Button size="sm" title="Today (t)" onClick={goToday}>
          Today
        </Button>

        {dateOpen ? (
          <GoToDate
            onPick={(ts) => {
              dispatch({ type: "anchor", anchor: ts });
              setDateOpen(false);
            }}
            onCancel={() => setDateOpen(false)}
          />
        ) : (
          <button
            type="button"
            onClick={() => setDateOpen(true)}
            title="Go to date (g then d)"
            // The period the view is showing is what the whole header is about,
            // and it used to be 13px medium — the same size as the buttons
            // either side of it. `text-reading` semibold puts two steps of the
            // ramp between it and them.
            className="ml-1 min-w-0 truncate text-reading font-semibold text-foreground hover:text-accent"
          >
            {title}
          </button>
        )}

        <div className="ml-auto flex shrink-0 items-center gap-2">
          <Button
            size="sm"
            title="Create event (c)"
            onClick={() => {
              setCreateSeed(undefined);
              setCreateOpen(true);
            }}
          >
            New
            <Kbd keys="c" className="border-none bg-transparent px-0" />
          </Button>
          <div className="flex items-center gap-px rounded-[var(--radius)] border border-border p-px">
            {VIEWS.map((view) => (
              <button
                key={view.id}
                type="button"
                onClick={() => actions.setCalendarView(view.id)}
                className={cn(
                  "flex h-5 items-center gap-1 rounded-[3px] px-2 text-micro transition-colors",
                  ui.calendarView === view.id
                    ? "bg-surface-raised text-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {view.label}
                <Kbd keys={view.keys} className="border-none bg-transparent px-0" />
              </button>
            ))}
          </div>
        </div>
      </header>

      {/* A write Google refused, said out loud and left on screen.
          Everything on this surface writes without a dialog — a drag, a resize,
          an arrow-key nudge, ⌘⌫ — so most failures have no modal to appear in,
          and the status rail clears itself after six seconds. This does not
          clear itself. It offers the same command again, because "try it again"
          is the correct response to most of what fails here: a 5xx, a rate
          limit, a network that came back. */}
      {failure && !modalOpen && (
        <div
          role="alert"
          className="flex shrink-0 items-center gap-2 border-b border-danger/40 bg-danger/10 px-3 py-1"
        >
          <AlertTriangle size={12} strokeWidth={1.75} className="shrink-0 text-danger" />
          <span className="min-w-0 flex-1 truncate text-micro text-danger">{failure.message}</span>
          <Button size="sm" variant="subtle" disabled={busy} onClick={() => void run(failure.command)}>
            Try again
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setFailure(null)} aria-label="Dismiss">
            <X size={12} strokeWidth={1.75} />
          </Button>
        </div>
      )}

      {finder && (
        <EventFinder
          query={finder.query}
          onQuery={(query) => setFinder((current) => (current ? { ...current, query, index: 0 } : current))}
          count={finderMatches.rows.length}
          index={finder.index}
          widened={finderMatches.widened}
          matchStart={finderMatch?.start ?? null}
          onEnter={() => {
            if (!finderMatch) {
              actions.setStatus(
                finder.query.trim() ? "Nothing matches" : "Type part of a name",
                "info",
              );
              return;
            }
            setFinder(null);
            openEvent(finderMatch.id);
          }}
          onCycle={(delta) =>
            setFinder((current) => {
              const total = finderMatches.rows.length;
              if (!current || total === 0) return current;
              // Wraps, because a list of three matches you are stepping through
              // should not need a Shift-Tab to get back to the first one.
              return { ...current, index: (current.index + delta + total) % total };
            })
          }
          onCancel={() => closeFinder(true)}
        />
      )}

      <div className="flex min-h-0 flex-1">
        <CalendarSidebar
          accounts={accounts}
          calendars={calendars}
          hidden={ui.hiddenCalendars}
          colorFor={colorFor}
          dark={dark}
          soloAccount={soloAccount}
          onToggle={(id) => dispatch({ type: "toggleCalendar", calendarId: id })}
          onSolo={setSoloAccount}
          settings={settings}
          onSettings={setSettings}
        />

        {/* The right-click menu wraps the grid and nothing else: the sidebar's
            calendars are checkboxes with their own affordances, and a menu about
            "the selected event" over a list of calendars would be about nothing
            the pointer is on. */}
        <CalendarContextMenu
          active={active}
          eventById={eventById}
          canEdit={editable}
          canCreateOn={creatableOn}
          verbs={verbs}
        >
          <div ref={grid} className="flex min-h-0 min-w-0 flex-1 flex-col">
            {ui.calendarView === "month" ? (
              <MonthGrid
                days={days}
                anchorMonth={ui.anchor}
                events={merged}
                colorFor={colorFor}
                dark={dark}
                selectedId={ui.eventId}
                dimIds={dimIds}
                onSelect={openEvent}
              />
            ) : (
              <TimeGrid
                days={days}
                events={merged}
                colorFor={colorFor}
                dark={dark}
                selectedId={ui.eventId}
                dimIds={dimIds}
                onSelect={(id) => dispatch({ type: "event", eventId: id })}
                onOpen={openEvent}
                onDraft={onGridDraft}
                onMove={onGridMove}
                todayNonce={todayNonce}
                revealNonce={revealNonce}
              />
            )}
          </div>
        </CalendarContextMenu>
      </div>

      <EventModal
        target={modal}
        calendars={calendars}
        accounts={accounts}
        colorFor={colorFor}
        dark={dark}
        merged={modalEvent ? (mergedById.get(modalEvent.id) ?? null) : null}
        defaultCalendarId={defaultCalendarId}
        recurring={modalEvent ? looksRecurring(modalEvent, allEvents) : false}
        canEdit={modalEvent ? editable(modalEvent) : true}
        error={failure?.message ?? null}
        busy={busy}
        contacts={contacts}
        onClose={closeModal}
        onSave={saveForm}
        onDelete={(scope, notify) => modalEvent && deleteEvent(modalEvent.id, scope, notify)}
        onDuplicate={() => modalEvent && duplicate(modalEvent)}
        onRsvp={(response) => modalEvent && rsvp(modalEvent.id, response)}
        onOpenExternal={openExternal}
      />

      <QuickCreate
        open={createOpen}
        seed={createSeed}
        calendars={calendars.map((calendar) => ({ id: calendar.id, name: calendarLabel(calendar) }))}
        onClose={() => setCreateOpen(false)}
        onCreate={onQuickCreate}
      />
    </div>
  );
}

function loadSettings(): CalendarSettings {
  try {
    const raw = window.localStorage.getItem(SETTINGS_KEY);
    return raw ? { ...DEFAULT_SETTINGS, ...(JSON.parse(raw) as Partial<CalendarSettings>) } : DEFAULT_SETTINGS;
  } catch {
    return DEFAULT_SETTINGS;
  }
}

/**
 * Where a keyboard-created event lands.
 *
 * The slot after whatever is focused, so `Tab Tab ⇧C` books the gap you were
 * just looking at. With nothing focused it is the next clean half hour — today
 * if today is in view, otherwise nine in the morning on the anchored day, which
 * is the hour someone browsing next week is thinking about.
 */
function defaultSlot(selected: CalendarEvent | null, anchor: number): number {
  if (selected && !selected.allDay) return selected.end;
  const now = Date.now();
  if (startOfDay(anchor).getTime() === startOfDay(now).getTime()) return nextSlot(now);
  return startOfDay(anchor).getTime() + 9 * 60 * MINUTE;
}

/** `g d` — one field, parsed the same way the composer parses a date. */
function GoToDate({ onPick, onCancel }: { onPick: (ts: number) => void; onCancel: () => void }) {
  const [text, setText] = useState("");
  const field = useRef<HTMLInputElement>(null);
  const parsed = text.trim() ? parseEventText(text).start : null;

  useEffect(() => {
    field.current?.focus();
  }, []);

  return (
    <div className="ml-1 flex min-w-0 items-center gap-2">
      <Input
        ref={field}
        value={text}
        placeholder="Go to… (next friday, 12 Sep)"
        className="h-6 w-56 text-list"
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && parsed !== null) onPick(parsed);
          if (event.key === "Escape") onCancel();
        }}
        onBlur={onCancel}
      />
      <span className="truncate text-micro text-faint-foreground">
        {parsed !== null ? fullDate(parsed) : "type a date"}
      </span>
    </div>
  );
}
