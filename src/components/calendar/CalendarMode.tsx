import { ChevronLeft, ChevronRight } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AccountId, CalendarEvent, CalendarId, EventId, Rsvp } from "@/types";
import { useKeyBindings, useKeymap } from "@/hooks/useKeymap";
import { useMach, viewRange, type CalendarView } from "@/hooks/useMach";
import {
  describeResult,
  getDataSource,
  type Command,
  type EventDraft,
  type EventScope,
} from "@/lib/data";
import type { KeyBinding } from "@/lib/keymap";
import { assignHues, type HueIndex } from "@/lib/calendar-palette";
import { mergeDuplicates, type MergedEvent } from "@/lib/calendar-merge";
import { parseEventText, type ParsedEvent } from "@/lib/calendar-nlp";
import { nudge, type DragOutcome } from "@/lib/calendar-drag";
import {
  duplicateDraft,
  formDraft,
  formPatch,
  looksRecurring,
  nextSlot,
  pasteDraft,
  rulesFor,
  type EventForm,
} from "@/lib/calendar-edit";
import { googleCalendarUrl } from "@/lib/calendar-links";
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
import { EventModal, type ModalTarget } from "./EventModal";
import { QuickCreate } from "./QuickCreate";
import { ShortcutSheet } from "./ShortcutSheet";
import { useIsDark } from "./use-is-dark";

const VIEWS: { id: CalendarView; label: string; keys: string; alias: string }[] = [
  { id: "day", label: "Day", keys: "1", alias: "d" },
  { id: "week", label: "Week", keys: "2", alias: "w" },
  { id: "month", label: "Month", keys: "3", alias: "m" },
];

const SETTINGS_KEY = "mach.calendar.settings";

/** ⇧1…⇧5 as the characters a US keyboard actually emits. */
const SHIFTED_DIGITS = ["!", "@", "#", "$", "%"];

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
  const keymap = useKeymap();

  const [settings, setSettings] = useState<CalendarSettings>(() => loadSettings());
  const [soloAccount, setSoloAccount] = useState<AccountId | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [createSeed, setCreateSeed] = useState<string | undefined>(undefined);
  const [shortcuts, setShortcuts] = useState<KeyBinding[] | null>(null);
  const [dateOpen, setDateOpen] = useState(false);
  const [todayNonce, setTodayNonce] = useState(0);
  const [revealNonce, setRevealNonce] = useState(0);
  /** The event the modal is showing, or the slot it is creating into. */
  const [modal, setModal] = useState<ModalTarget | null>(null);
  const [busy, setBusy] = useState(false);
  /** ⌘C parks an event here; ⌘V drops a copy of it on the anchored day. */
  const [clipboard, setClipboard] = useState<CalendarEvent | null>(null);

  const shortcutsOpen = shortcuts !== null;
  const modalOpen = modal !== null;
  const active =
    ui.mode === "calendar" && !ui.paletteOpen && !createOpen && !shortcutsOpen && !modalOpen;

  useEffect(() => {
    window.localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
  }, [settings]);

  const { start, days: dayCount } = viewRange(ui.calendarView, ui.anchor);
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

  // Hue by calendar id hash, so a calendar keeps its colour between sessions
  // however sync happens to order things.
  const hues = useMemo(() => assignHues(calendars.map((c) => c.id)), [calendars]);
  const hueFor = useCallback(
    (id: CalendarId): HueIndex => hues.get(id) ?? 0,
    [hues],
  );

  const inRange = useMemo(() => {
    const soloed = soloAccount;
    return visibleEvents.filter((event) => {
      if (event.start >= rangeEnd || event.end <= rangeStart) return false;
      if (soloed !== null && event.accountId !== soloed) return false;
      if (!settings.showDeclined && event.rsvp === "declined") return false;
      return true;
    });
  }, [visibleEvents, rangeStart, rangeEnd, soloAccount, settings.showDeclined]);

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
  const ordered = useMemo(
    () => [...merged].sort((a, b) => a.event.start - b.event.start || a.event.id - b.event.id),
    [merged],
  );

  const step = useCallback(
    (delta: number) => {
      if (ordered.length === 0) return;
      const index = ordered.findIndex((m) => m.event.id === ui.eventId);
      const next = index === -1 ? (delta > 0 ? 0 : ordered.length - 1) : index + delta;
      const clamped = Math.min(Math.max(next, 0), ordered.length - 1);
      dispatch({ type: "event", eventId: ordered[clamped].event.id });
      setRevealNonce((n) => n + 1);
    },
    [ordered, ui.eventId, dispatch],
  );

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
   * it to reload is the seam: `actions.reload()` refetches the window, which is
   * the only lever this component has into state another unit owns.
   *
   * `undo` is threaded into the status bar exactly the way the mail commands
   * do it, so `z` reverses a drag as readily as it reverses an archive.
   */
  const run = useCallback(
    async (command: Command) => {
      setBusy(true);
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
        actions.reload();
        return result;
      } catch (error) {
        dispatch({
          type: "status",
          status: {
            message: error instanceof Error ? error.message : "That did not save",
            tone: "error",
          },
        });
        return null;
      } finally {
        setBusy(false);
      }
    },
    [actions, dispatch],
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
    (draft: EventDraft, calendarId: CalendarId | null) => {
      const target = calendarId ?? defaultCalendarId;
      const accountId = target === null ? null : accountForCalendar(target);
      if (target === null || accountId === null) {
        actions.setStatus("There is no calendar to create on yet", "error");
        return;
      }
      void run({ kind: "createEvent", accountId, calendarId: target, draft });
    },
    [run, defaultCalendarId, accountForCalendar, actions],
  );

  /** A drag or a keyboard nudge — the same command either way. */
  const applyMove = useCallback(
    (eventId: EventId, outcome: DragOutcome, allDay: boolean) => {
      void run({
        kind: "updateEvent",
        eventId,
        patch: { startTs: outcome.start, endTs: outcome.end, isAllDay: allDay },
        scope: "this",
      });
    },
    [run],
  );

  const onGridMove = useCallback(
    (move: EventMove) => {
      applyMove(move.eventId, { start: move.start, end: move.end }, false);
    },
    [applyMove],
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
      const event = selectedEvent;
      if (!event) {
        actions.setStatus("Pick an event first — Tab steps through them", "info");
        return;
      }
      if (event.allDay) {
        if (action.kind !== "move" || action.axis !== "day") {
          actions.setStatus("An all-day event has no time to move", "info");
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
    [selectedEvent, applyMove, actions],
  );

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

  const saveForm = useCallback(
    (form: EventForm, scope: EventScope) => {
      const event = modal?.mode === "view" ? modal.event : null;
      if (!event) {
        const draft = formDraft(form);
        if ("error" in draft) {
          actions.setStatus(draft.error, "error");
          return;
        }
        setModal(null);
        create(draft, form.calendarId);
        return;
      }

      const patch = formPatch(event, form);
      const moved = form.calendarId !== event.calendarId;
      setModal(null);
      if (patch) {
        void run({ kind: "updateEvent", eventId: event.id, patch, scope }).then(() => {
          if (moved) {
            const accountId = accountForCalendar(form.calendarId);
            if (accountId !== null) {
              void run({
                kind: "moveEvent",
                eventId: event.id,
                accountId,
                calendarId: form.calendarId,
              });
            }
          }
        });
      } else if (moved) {
        const accountId = accountForCalendar(form.calendarId);
        if (accountId !== null) {
          void run({
            kind: "moveEvent",
            eventId: event.id,
            accountId,
            calendarId: form.calendarId,
          });
        }
      }
    },
    [modal, run, create, accountForCalendar, actions],
  );

  const deleteEvent = useCallback(
    (id: EventId, scope: EventScope) => {
      setModal(null);
      dispatch({ type: "event", eventId: null });
      void run({ kind: "deleteEvent", eventId: id, scope });
    },
    [run, dispatch],
  );

  const duplicate = useCallback(
    (event: CalendarEvent) => {
      setModal(null);
      create(duplicateDraft(event), event.calendarId);
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
  /* Keyboard                                                                */
  /* ---------------------------------------------------------------------- */

  const withEvent = useCallback(
    (what: (event: CalendarEvent) => void) => () => {
      if (!selectedEvent) {
        actions.setStatus("Pick an event first — Tab steps through them", "info");
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

      // Range navigation. §4 resolves the Google/Notion conflict in Google's
      // favour: n/p move the *range*, and event-to-event moves to Tab.
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
      { keys: "right", when: () => active, handler: () => actions.shiftPeriod(1) },
      { keys: "left", when: () => active, handler: () => actions.shiftPeriod(-1) },
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

      // Moving between events. Tab is the platform's "next thing"; the bare
      // arrows are the reflex, and both land on the same step.
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
      { keys: "down", when: () => active, handler: () => step(1) },
      { keys: "up", when: () => active, handler: () => step(-1) },

      // Creating.
      {
        keys: "c",
        group: "Calendar",
        description: "Create — type it in words",
        when: () => active,
        handler: () => {
          setCreateSeed(undefined);
          setCreateOpen(true);
        },
      },
      {
        keys: "shift+c",
        group: "Calendar",
        description: "Create — full editor",
        when: () => active,
        handler: () => openCreate(),
      },

      // Opening and editing the focused event.
      {
        keys: "e",
        group: "Calendar",
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
        group: "Calendar",
        description: "Delete the event",
        when: () => active,
        handler: withEvent((event) =>
          looksRecurring(event, inRange)
            ? setModal({ mode: "view", event })
            : deleteEvent(event.id, "this"),
        ),
      },
      {
        keys: "delete",
        when: () => active,
        handler: withEvent((event) =>
          looksRecurring(event, inRange)
            ? setModal({ mode: "view", event })
            : deleteEvent(event.id, "this"),
        ),
      },

      // Moving and resizing without a mouse. Shift slides the whole event,
      // Alt moves one edge — the same two gestures the pointer offers.
      {
        keys: "shift+down",
        group: "Calendar",
        description: "Move 15 minutes later",
        when: () => active,
        handler: () => nudgeSelected({ kind: "move", axis: "time", steps: 1 }),
      },
      {
        keys: "shift+up",
        group: "Calendar",
        description: "Move 15 minutes earlier",
        when: () => active,
        handler: () => nudgeSelected({ kind: "move", axis: "time", steps: -1 }),
      },
      {
        keys: "shift+right",
        group: "Calendar",
        description: "Move to the next day",
        when: () => active,
        handler: () => nudgeSelected({ kind: "move", axis: "day", days: 1 }),
      },
      {
        keys: "shift+left",
        group: "Calendar",
        description: "Move to the previous day",
        when: () => active,
        handler: () => nudgeSelected({ kind: "move", axis: "day", days: -1 }),
      },
      {
        keys: "alt+down",
        group: "Calendar",
        description: "Make it 15 minutes longer",
        when: () => active,
        handler: () => nudgeSelected({ kind: "resize", edge: "end", steps: 1 }),
      },
      {
        keys: "alt+up",
        group: "Calendar",
        description: "Make it 15 minutes shorter",
        when: () => active,
        handler: () => nudgeSelected({ kind: "resize", edge: "end", steps: -1 }),
      },
      {
        keys: "shift+alt+up",
        group: "Calendar",
        description: "Start 15 minutes earlier",
        when: () => active,
        handler: () => nudgeSelected({ kind: "resize", edge: "start", steps: -1 }),
      },
      {
        keys: "shift+alt+down",
        group: "Calendar",
        description: "Start 15 minutes later",
        when: () => active,
        handler: () => nudgeSelected({ kind: "resize", edge: "start", steps: 1 }),
      },

      // Copy, paste, duplicate, and out to Google.
      {
        keys: "mod+c",
        group: "Calendar",
        description: "Copy the event",
        when: () => active && ui.eventId !== null,
        handler: withEvent((event) => {
          setClipboard(event);
          actions.setStatus(`Copied “${event.title}” — ⌘V drops it on the day in view`, "info");
        }),
      },
      {
        keys: "mod+v",
        group: "Calendar",
        description: "Paste onto the day in view",
        when: () => active && clipboard !== null,
        handler: () => {
          if (!clipboard) return;
          create(pasteDraft(clipboard, ui.anchor), clipboard.calendarId);
        },
      },
      {
        keys: "shift+d",
        group: "Calendar",
        description: "Duplicate the event",
        when: () => active,
        handler: withEvent(duplicate),
      },
      {
        keys: "o",
        group: "Calendar",
        description: "Open in Google Calendar",
        when: () => active,
        handler: withEvent((event) =>
          openExternal(
            googleCalendarUrl(event, accounts.find((a) => a.id === event.accountId)?.email),
          ),
        ),
      },

      {
        keys: "z",
        group: "Calendar",
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
        keys: "?",
        group: "Calendar",
        description: "Keyboard shortcuts",
        when: () => ui.mode === "calendar" && !ui.paletteOpen && !createOpen && !modalOpen,
        handler: () => setShortcuts((open) => (open ? null : snapshotBindings())),
      },
      {
        keys: "escape",
        priority: 110,
        allowInInput: true,
        when: () => shortcutsOpen || createOpen,
        handler: () => {
          setShortcuts(null);
          setCreateOpen(false);
        },
      },

      // Multi-account (§7). The brief asks for ⌘1–9 here, but ⌘1/⌘2 already mean
      // "switch mode" app-wide and on macOS ⌘<digit> means "switch view" in every
      // app on the platform. Visibility moves to a `v <digit>` sequence instead, in the
      // spirit of `g d`.
      ...Array.from({ length: 9 }, (_, i) => ({
        keys: `v ${i + 1}`,
        group: i === 0 ? "Calendar" : undefined,
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
        group: i === 0 ? "Calendar" : undefined,
        description: i === 0 ? "Show only account 1–5" : undefined,
        when: () => active,
        handler: () => soloAccountAt(i),
      })),
    ],
    [
      active,
      actions,
      accounts,
      clipboard,
      create,
      dateOpen,
      deleteEvent,
      dispatch,
      duplicate,
      goToday,
      inRange,
      modalOpen,
      nudgeSelected,
      openCreate,
      openEvent,
      openExternal,
      shortcutsOpen,
      createOpen,
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

  /**
   * Everything the sheet should list, whether or not it is live right now.
   *
   * `keymap.active()` alone answers "almost nothing": most calendar bindings
   * are gated on there being a selected event or on no dialog being open, so
   * the moment `?` opens the sheet, half of them go dark. Merging the local
   * list back in is what makes the sheet a map of the mode rather than a
   * snapshot of one instant.
   */
  const snapshotBindings = useCallback((): KeyBinding[] => {
    const seen = new Set<string>();
    const out: KeyBinding[] = [];
    for (const binding of [...bindings, ...keymap.active()]) {
      if (!binding.description) continue;
      const key = `${binding.group ?? ""}|${binding.keys}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push(binding);
    }
    return out;
  }, [bindings, keymap]);

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
            className="ml-1 min-w-0 truncate text-list font-medium text-foreground hover:text-accent"
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
                  "flex h-5 items-center gap-1.5 rounded-[3px] px-2 text-micro transition-colors",
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

      <div className="flex min-h-0 flex-1">
        <CalendarSidebar
          accounts={accounts}
          calendars={calendars}
          hidden={ui.hiddenCalendars}
          hueFor={hueFor}
          dark={dark}
          soloAccount={soloAccount}
          onToggle={(id) => dispatch({ type: "toggleCalendar", calendarId: id })}
          onSolo={setSoloAccount}
          settings={settings}
          onSettings={setSettings}
        />

        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          {ui.calendarView === "month" ? (
            <MonthGrid
              days={days}
              anchorMonth={ui.anchor}
              events={merged}
              hueFor={hueFor}
              dark={dark}
              selectedId={ui.eventId}
              onSelect={openEvent}
            />
          ) : (
            <TimeGrid
              days={days}
              events={merged}
              hueFor={hueFor}
              dark={dark}
              selectedId={ui.eventId}
              onSelect={(id) => dispatch({ type: "event", eventId: id })}
              onOpen={openEvent}
              onDraft={onGridDraft}
              onMove={onGridMove}
              todayNonce={todayNonce}
              revealNonce={revealNonce}
            />
          )}
        </div>
      </div>

      <EventModal
        target={modal}
        calendars={calendars}
        accounts={accounts}
        hueFor={hueFor}
        dark={dark}
        merged={modalEvent ? (mergedById.get(modalEvent.id) ?? null) : null}
        defaultCalendarId={defaultCalendarId}
        recurring={modalEvent ? looksRecurring(modalEvent, allEvents) : false}
        busy={busy}
        onClose={closeModal}
        onSave={saveForm}
        onDelete={(scope) => modalEvent && deleteEvent(modalEvent.id, scope)}
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
      <ShortcutSheet
        open={shortcutsOpen}
        bindings={shortcuts ?? []}
        onClose={() => setShortcuts(null)}
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
