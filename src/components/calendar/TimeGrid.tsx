import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from "react";
import type { CalendarId, EventId } from "@/types";
import { columnGeometry, layoutEvents } from "@/lib/event-layout";
import {
  ALL_DAY_CHIP_HEIGHT,
  ALL_DAY_MAX_ROWS,
  ALL_DAY_ROW_PITCH,
  BLOCK_RADIUS,
  BLOCK_RIGHT_GUTTER,
  DEFAULT_EVENT_MINUTES,
  EXPAND_CLUSTER_MIN,
  EXPAND_DELAY_MS,
  HOUR_HEIGHT,
  SNAP_MINUTES,
  TIME_GUTTER,
  TIME_GUTTER_INSET,
  Z_EVENT,
  Z_EVENT_HOVER,
  Z_EVENT_SELECTED,
  Z_NOW,
  blockHeight,
  clusterPlan,
  nowScrollTop,
  offsetForTime,
  packRows,
  snapTime,
  snapTimeDown,
  timeForOffset,
  visibleColumns,
} from "@/lib/calendar-geometry";
import {
  createResult,
  dayIndexAt,
  dragLabel,
  isCopyDrag,
  isDrag,
  moveResult,
  resizeResult,
  type DragOrigin,
  type DragOutcome,
  type ResizeEdge,
} from "@/lib/calendar-drag";
import type { MergedEvent } from "@/lib/calendar-merge";
import { fallbackFill, paintFor, toneFor, type CalendarColor } from "@/lib/calendar-palette";
import { DAY, MINUTE, isToday, shortTime, startOfDay, weekdayShort } from "@/lib/time";
import { cn } from "@/lib/utils";
import { usePreferences } from "@/components/prefs/PreferencesProvider";
import { EventBlock, EventChip, selectionShadow } from "./EventBlock";

/** A provisional event, dragged or clicked into being but not yet saved. */
export interface EventDraft {
  start: number;
  end: number;
  title: string;
}

/** What a finished drag or resize asks the caller to save. */
export interface EventMove {
  eventId: EventId;
  start: number;
  end: number;
  /** Alt was held: leave the original where it is and create a copy here. */
  copy: boolean;
}

interface TimeGridProps {
  days: Date[];
  events: MergedEvent[];
  colorFor: (calendarId: CalendarId) => CalendarColor;
  dark: boolean;
  /** Where the keyboard cursor is. Distinct from "open in the modal". */
  selectedId: EventId | null;
  /**
   * Events the type-to-select bar ruled out. They fade rather than disappear:
   * the week's shape is the context that makes a match mean something, and a
   * grid that empties itself as you type is a grid you cannot navigate by.
   */
  dimIds?: ReadonlySet<EventId>;
  onSelect: (id: EventId) => void;
  /** Click, or Enter on the focused block — show the whole event. */
  onOpen: (id: EventId) => void;
  /** Enter in the inline title field; `expand` means Tab — hand it to the composer. */
  onDraft: (draft: EventDraft, intent: "save" | "expand") => void;
  /** A block was dragged or resized to a new time. */
  onMove: (move: EventMove) => void;
  /** Bumped by the `t` key so the grid re-anchors on now, animated. */
  todayNonce: number;
  /** Bumped when something outside wants the focused block scrolled into view. */
  revealNonce?: number;
}

const HEADER_HEIGHT = 30;

/** Gutter + one track per day. Header, all-day and the timed grid share this
 *  so their vertical rules cannot round to different pixels. */
function weekTracks(count: number): string {
  return `${TIME_GUTTER}px repeat(${count}, minmax(0, 1fr))`;
}

/** Where a drag started, plus everything needed to paint its ghost. */
interface DragSession {
  kind: "move" | "resize";
  edge: ResizeEdge;
  eventId: EventId;
  origin: DragOrigin;
  color: CalendarColor;
  tone: ReturnType<typeof toneFor>;
  title: string;
  pointerId: number;
  startX: number;
  startY: number;
  originDayIndex: number;
  /** Left edge and width of the day-columns area, measured once at grab. */
  contentLeft: number;
  contentWidth: number;
  moved: boolean;
  /** Alt is held right now. Recomputed as the drag runs, not fixed at grab. */
  copy: boolean;
  outcome: DragOutcome;
}

/** Pixels from the top of the grid for an instant, whichever day it lands on. */
function gridTop(ts: number): number {
  return offsetForTime(ts, startOfDay(ts).getTime());
}

/**
 * The week (and day) grid.
 *
 * One scroll container, so the sticky day header and the pinned all-day strip
 * can never drift out of alignment with the columns beneath them. All the
 * numbers come from `calendar-geometry`, which is the brief's measurements of
 * Google Calendar and nothing else.
 *
 * # How dragging avoids re-rendering the week
 *
 * A `pointermove` handler that calls `setState` re-renders every column, every
 * block and the whole overlap layout — sixty times a second, for a gesture that
 * only ever moves one rectangle. So it does not.
 *
 * A drag makes exactly **two** React renders: one when it starts (the source
 * block dims and a ghost mounts) and one when it ends. In between, the ghost is
 * moved by writing `transform` straight onto its DOM node inside a
 * `requestAnimationFrame`, and its time label by writing `textContent`. Nothing
 * above it in the tree knows the pointer is moving. The grid only re-lays-out
 * once, on release, when the command comes back.
 *
 * Alt is the exception, and it earns one render each time it goes down or up:
 * it changes what the drag *means* — move, or copy — and the source block and
 * the ghost both have to say so.
 */
export function TimeGrid({
  days,
  events,
  colorFor,
  dark,
  selectedId,
  dimIds,
  onSelect,
  onOpen,
  onDraft,
  onMove,
  todayNonce,
  revealNonce = 0,
}: TimeGridProps) {
  const scroller = useRef<HTMLDivElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const ghost = useRef<HTMLDivElement>(null);
  const ghostLabel = useRef<HTMLSpanElement>(null);
  const session = useRef<DragSession | null>(null);
  const frame = useRef<number | null>(null);
  /** Set on release so the click that follows a real drag does not open it. */
  const swallowClick = useRef(false);
  const blocks = useRef(new Map<EventId, HTMLElement>());

  const [now, setNow] = useState(() => Date.now());
  const [allDayExpanded, setAllDayExpanded] = useState(false);
  const [draft, setDraft] = useState<{ dayStart: number; start: number; end: number } | null>(null);
  const [editing, setEditing] = useState(false);
  /** Non-null while a drag is live — the only state a drag ever sets. */
  const [dragging, setDragging] = useState<DragSession | null>(null);
  /*
   * Alt held during a move, as state rather than as part of the session.
   *
   * It has to re-render — the source block stops being dimmed, the ghost grows
   * a `+`, the cursor stays on the original — and it is the one thing about a
   * live drag that can change without the pointer moving. Keeping it out of
   * `dragging` matters: the window listeners are keyed on that object, and
   * rebuilding them every time a thumb touches the option key would tear down
   * and reattach the drag mid-gesture.
   */
  const [copying, setCopying] = useState(false);

  /*
   * The working day, as a preference.
   *
   * Two things read it, and they are the two things "working hours" has to
   * mean in a day grid: where it opens, and which part of the column is lit.
   * Read here rather than threaded down from `CalendarMode` because it is
   * purely presentational — no layout, no geometry, nothing a parent has to
   * agree with — and threading it through `TimeGridProps` and `DayColumnProps`
   * would put a preference in two prop lists to change a background.
   */
  const { workingHours } = usePreferences();
  // Half an hour of air above the start, which is what the measured 6.5 was
  // relative to a seven o'clock day.
  const scrollFloor = Math.max(workingHours.start - 0.5, 0);

  /**
   * The last scroll position this component asked for.
   *
   * How a scroll of ours is told apart from a scroll of the user's, which is
   * the difference between a position we may replace and one we may not.
   */
  const asked = useRef<number | null>(null);
  /** The user has put the grid somewhere. It is theirs now. */
  const moved = useRef(false);

  /**
   * Put now a quarter of the way down, if the grid is in a state to be asked.
   *
   * Returns whether it actually happened. **`scrollTop` is a request, not an
   * assignment**: an element with no layout — no height, nothing to overflow —
   * takes the write, keeps zero, and reports zero afterwards. Zero is midnight,
   * and midnight is a screen and a half of empty night with the day's events
   * pushed below the fold. So the caller is told, and tries again later.
   */
  const anchor = useCallback(
    (behavior: ScrollBehavior): boolean => {
      const node = scroller.current;
      if (!node) return false;
      // Not "is it visible": the calendar is mounted behind the mail pane for
      // the life of the window and is anchored perfectly well from there. It is
      // "is there anything to scroll", which is the precondition the write has.
      if (node.clientHeight <= 0 || node.scrollHeight <= node.clientHeight) return false;
      const top = nowScrollTop(
        Date.now(),
        startOfDay(Date.now()).getTime(),
        node.clientHeight,
        scrollFloor,
      );
      asked.current = top;
      if (behavior === "smooth") node.scrollTo({ top, behavior });
      else node.scrollTop = top;
      return true;
    },
    [scrollFloor],
  );

  /*
   * Where the grid opens.
   *
   * This used to be a `[]` effect — one write, at mount, believed. Three
   * things are wrong with that, and the first one shipped:
   *
   *   - **A mount is not a layout.** The calendar mounts with the window,
   *     whichever mode is on screen, and a window that has not been laid out
   *     yet gives the scroller `clientHeight: 0` and `scrollHeight: 0`. The
   *     write is dropped, the grid stays at midnight, and nothing ever asks
   *     again. So the anchor waits for the first layout the grid is given.
   *   - **The floor is a guess until the store answers.** `workingHours`
   *     arrives asynchronously, so the first render's floor is the default and
   *     may be an hour and a half off. A floor that changes re-anchors.
   *   - **A grid the user has scrolled is not ours to move.** Once they have
   *     put it somewhere, a preference landing late leaves it alone.
   *
   * Day and week share this component and this scroller, so switching between
   * them keeps the position, which is what Google Calendar does. Month is a
   * different component: coming back from it is a mount, and mounts anchor.
   */
  useLayoutEffect(() => {
    if (moved.current) return;
    if (anchor("auto")) return;
    const node = scroller.current;
    if (!node) return;
    // No layout yet. `ResizeObserver` fires as soon as the box is real —
    // including the fire it makes on observe, which covers the case where the
    // layout arrived between the effect and here.
    const observer = new ResizeObserver(() => {
      if (moved.current || anchor("auto")) observer.disconnect();
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [anchor]);

  /*
   * `t` re-anchors, and animates so the keypress visibly registered.
   *
   * An explicit ask, so it overrides a scroll the user made — and having
   * answered it the grid is where they asked, not where anything guessed, so a
   * later change of floor leaves it alone. Keyed on the nonce itself rather
   * than on the effect running: `anchor` changes identity with the working day,
   * and that must not replay the last keypress.
   */
  const answered = useRef(0);
  useEffect(() => {
    if (todayNonce === 0 || todayNonce === answered.current) return;
    answered.current = todayNonce;
    if (anchor("smooth")) moved.current = true;
  }, [todayNonce, anchor]);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, []);

  // Keyboard focus has to be *visible*, which means the block has to be on
  // screen. `block: "nearest"` scrolls the minimum that achieves that, so
  // stepping through a day does not jump the viewport around.
  useEffect(() => {
    if (selectedId === null) return;
    const node = blocks.current.get(selectedId);
    node?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [selectedId, revealNonce]);

  const timed = useMemo(() => events.filter((m) => !m.event.allDay), [events]);
  const allDay = useMemo(() => events.filter((m) => m.event.allDay), [events]);

  const rangeStart = startOfDay(days[0]).getTime();
  const rangeEnd = startOfDay(days[days.length - 1]).getTime() + DAY;

  // All-day bars span their days rather than repeating once per day.
  const bars = useMemo(() => {
    const rows = allDay
      .filter((m) => m.event.start < rangeEnd && m.event.end > rangeStart)
      .map((m) => {
        const startIndex = Math.max(
          0,
          Math.round((startOfDay(m.event.start).getTime() - rangeStart) / DAY),
        );
        const endIndex = Math.min(
          days.length,
          Math.max(startIndex + 1, Math.round((m.event.end - rangeStart) / DAY)),
        );
        return { merged: m, startIndex, span: endIndex - startIndex };
      });
    return packRows(rows);
  }, [allDay, rangeStart, rangeEnd, days.length]);

  const rowCount = bars.reduce((max, bar) => Math.max(max, bar.row + 1), 0);
  const shownRows = allDayExpanded ? rowCount : Math.min(rowCount, ALL_DAY_MAX_ROWS);
  const hiddenBars = bars.filter((bar) => bar.row >= shownRows).length;

  const commitDraft = useCallback(
    (intent: "save" | "expand", title: string) => {
      if (draft) onDraft({ start: draft.start, end: draft.end, title }, intent);
      setDraft(null);
      setEditing(false);
    },
    [draft, onDraft],
  );

  const registerBlock = useCallback((id: EventId, node: HTMLElement | null) => {
    if (node) blocks.current.set(id, node);
    else blocks.current.delete(id);
  }, []);

  /* ---------------------------------------------------------------------- */
  /* Dragging                                                                */
  /* ---------------------------------------------------------------------- */

  /** Paint the ghost. Called from rAF only — never from React. */
  const paintGhost = useCallback(() => {
    frame.current = null;
    const live = session.current;
    const node = ghost.current;
    if (!live || !node) return;

    const dayShift =
      (columnIndexFor(live.outcome.start, days) - live.originDayIndex) *
      (live.contentWidth / days.length);
    const dy = gridTop(live.outcome.start) - gridTop(live.origin.start);

    node.style.transform = `translate3d(${dayShift}px, ${dy}px, 0)`;
    if (live.kind === "resize") {
      node.style.height = `${blockHeight(live.outcome.end - live.outcome.start)}px`;
    }
    if (ghostLabel.current) {
      ghostLabel.current.textContent = dragLabel(live.outcome, {
        showDate: columnIndexFor(live.outcome.start, days) !== live.originDayIndex,
      });
    }
  }, [days]);

  const schedule = useCallback(() => {
    if (frame.current !== null) return;
    frame.current = window.requestAnimationFrame(paintGhost);
  }, [paintGhost]);

  const endDrag = useCallback(
    (commit: boolean) => {
      const live = session.current;
      session.current = null;
      if (frame.current !== null) {
        window.cancelAnimationFrame(frame.current);
        frame.current = null;
      }
      setDragging(null);
      setCopying(false);
      if (!live) return;
      swallowClick.current = live.moved;
      if (
        commit &&
        live.moved &&
        (live.outcome.start !== live.origin.start || live.outcome.end !== live.origin.end)
      ) {
        onMove({
          eventId: live.eventId,
          start: live.outcome.start,
          end: live.outcome.end,
          copy: live.copy,
        });
      }
    },
    [onMove],
  );

  // The listeners live on `window` rather than on the block, so a drag survives
  // the pointer leaving the grid entirely — and they are attached only while a
  // drag is live, so there is nothing running when the calendar is idle.
  useEffect(() => {
    if (!dragging) return;

    /** Take alt from whatever event carries it. Cheap, and idempotent. */
    const syncCopy = (altKey: boolean) => {
      const live = session.current;
      if (!live) return;
      const next = isCopyDrag(live.kind, altKey);
      if (next === live.copy) return;
      live.copy = next;
      setCopying(next);
    };

    const onPointerMove = (event: PointerEvent) => {
      const live = session.current;
      if (!live || event.pointerId !== live.pointerId) return;
      syncCopy(event.altKey);
      const dx = event.clientX - live.startX;
      const dy = event.clientY - live.startY;
      if (!live.moved && !isDrag(dx, dy)) return;
      live.moved = true;

      if (live.kind === "resize") {
        live.outcome = resizeResult(live.origin, live.edge, dy);
      } else {
        const column = dayIndexAt(
          event.clientX,
          live.contentLeft,
          live.contentWidth,
          days.length,
        );
        live.outcome = moveResult(live.origin, dy, dayDelta(days, live.originDayIndex, column));
      }
      schedule();
    };

    const onPointerUp = (event: PointerEvent) => {
      if (session.current && event.pointerId !== session.current.pointerId) return;
      // The release is the last word on the modifier: letting go of alt a
      // frame before the button is the user changing their mind, and the
      // event they are dropping is the one they can see.
      syncCopy(event.altKey);
      endDrag(true);
    };

    // Escape mid-drag puts everything back, which is the difference between a
    // gesture you can explore and one you have to be sure about before you
    // start.
    const onKeyDown = (event: KeyboardEvent) => {
      syncCopy(event.altKey);
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      endDrag(false);
    };

    const onKeyUp = (event: KeyboardEvent) => syncCopy(event.altKey);

    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", onPointerUp);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("keyup", onKeyUp, true);
    return () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("pointercancel", onPointerUp);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("keyup", onKeyUp, true);
    };
  }, [dragging, days, schedule, endDrag]);

  const grab = useCallback(
    (
      event: ReactPointerEvent,
      merged: MergedEvent,
      kind: "move" | "resize",
      edge: ResizeEdge,
    ) => {
      if (event.button !== 0) return;
      const rect = body.current?.getBoundingClientRect();
      if (!rect) return;
      const target = merged.event;
      const dayStart = startOfDay(target.start).getTime();

      const next: DragSession = {
        kind,
        edge,
        eventId: target.id,
        origin: { start: target.start, end: target.end, dayStart },
        color: colorFor(target.calendarId),
        tone: toneFor(target.rsvp),
        title: target.title,
        pointerId: event.pointerId,
        startX: event.clientX,
        startY: event.clientY,
        originDayIndex: columnIndexFor(target.start, days),
        contentLeft: rect.left + TIME_GUTTER,
        contentWidth: rect.width - TIME_GUTTER,
        moved: false,
        copy: isCopyDrag(kind, event.altKey),
        outcome: { start: target.start, end: target.end },
      };
      session.current = next;
      setDragging(next);
      setCopying(next.copy);
      onSelect(target.id);
      event.preventDefault();
    },
    [days, colorFor, onSelect],
  );

  const ghostPaint = dragging ? paintFor(dragging.color, dragging.tone, { dark }) : null;
  const ghostColumnWidth = dragging ? dragging.contentWidth / days.length : 0;

  return (
    <div
      ref={scroller}
      className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden"
      style={{ overscrollBehavior: "contain", scrollbarGutter: "stable" }}
      // Whose scroll position is this? Ours until something moves it that is
      // not us — a wheel, a drag of the bar, an arrow key stepping onto a block
      // off screen. After that the grid stays where it was put, and a
      // preference arriving late does not yank it back.
      onScroll={(event) => {
        const top = event.currentTarget.scrollTop;
        if (asked.current !== null && Math.abs(top - asked.current) < 1) return;
        moved.current = true;
      }}
      // A click is how a block opens, and a drag ends in one. Swallowing it in
      // the capture phase is the only place that can tell the two apart.
      onClickCapture={(event) => {
        if (!swallowClick.current) return;
        swallowClick.current = false;
        event.preventDefault();
        event.stopPropagation();
      }}
    >
      <div
        className="sticky top-0 z-30 grid border-b border-border bg-background"
        style={{ gridTemplateColumns: weekTracks(days.length) }}
      >
        <div className="border-r border-border" />
        {/*
          The column header carries the biggest type in the grid, and it is the
          only thing that does.

          It used to be an 11px weekday beside a 13px date: two sizes two pixels
          apart, in a view where 11px and 12px carried everything else, so the
          header did not read as a header and the week had no horizontal anchor.
          The date is `text-title` (17px) against the weekday's 11px, skipping a
          step of the ramp between them. The row is the same 30px it was.
        */}
        {days.map((day) => (
          <div
            key={day.getTime()}
            className="flex min-w-0 items-baseline gap-2 border-r border-border px-2 last:border-r-0"
            style={{ height: HEADER_HEIGHT }}
          >
            <span
              className={cn(
                "self-center text-micro font-medium uppercase tracking-[0.06em]",
                isToday(day) ? "text-accent" : "text-faint-foreground",
              )}
            >
              {weekdayShort(day)}
            </span>
            <span
              className={cn(
                "self-center font-mono text-title tabular-nums leading-none",
                isToday(day) ? "font-semibold text-accent" : "text-foreground",
              )}
            >
              {day.getDate()}
            </span>
          </div>
        ))}
      </div>

      {/* The all-day strip is pinned: an all-day event that scrolls away is an
          all-day event you forget about. Overflow expands in place rather than
          into a popover, which would cover the grid you were comparing to. */}
      {rowCount > 0 && (
        <div
          className="sticky z-20 grid border-b border-border bg-background"
          style={{ top: HEADER_HEIGHT, gridTemplateColumns: weekTracks(days.length) }}
        >
          <div
            className="border-r border-border pt-1 text-right text-micro text-faint-foreground"
            style={{ paddingRight: TIME_GUTTER_INSET }}
          >
            all-day
          </div>
          {/* Column rules live on the same tracks as the header. The chips
              still sit in a percentage overlay, which is how a bar can span
              Thursday into Friday without being two cells. */}
          <div
            className="relative min-w-0"
            style={{
              gridColumn: `2 / span ${days.length}`,
              height: shownRows * ALL_DAY_ROW_PITCH + 4,
            }}
          >
            <div
              className="pointer-events-none absolute inset-0 grid"
              style={{ gridTemplateColumns: `repeat(${days.length}, minmax(0, 1fr))` }}
            >
              {days.map((day) => (
                <div
                  key={day.getTime()}
                  className="border-r border-border last:border-r-0"
                />
              ))}
            </div>
            {bars
              .filter((bar) => bar.row < shownRows)
              .map((bar) => {
                const event = bar.merged.event;
                return (
                  <EventChip
                    key={event.id}
                    event={event}
                    color={colorFor(event.calendarId)}
                    dark={dark}
                    tone={toneFor(event.rsvp)}
                    past={event.end < now}
                    selected={event.id === selectedId}
                    dimmed={dimIds?.has(event.id) ?? false}
                    copies={bar.merged.copies.length}
                    onSelect={() => onOpen(event.id)}
                    blockRef={(node) => registerBlock(event.id, node)}
                    style={{
                      position: "absolute",
                      top: bar.row * ALL_DAY_ROW_PITCH + 2,
                      height: ALL_DAY_CHIP_HEIGHT,
                      left: `calc(${(bar.startIndex * 100) / days.length}% + 2px)`,
                      width: `calc(${(bar.span * 100) / days.length}% - 4px)`,
                    }}
                  />
                );
              })}
            {hiddenBars > 0 && !allDayExpanded && (
              <button
                type="button"
                onClick={() => setAllDayExpanded(true)}
                className="absolute right-1 text-micro text-muted-foreground hover:text-foreground"
                style={{ top: (shownRows - 1) * ALL_DAY_ROW_PITCH + 4 }}
              >
                +{hiddenBars} more
              </button>
            )}
            {allDayExpanded && rowCount > ALL_DAY_MAX_ROWS && (
              <button
                type="button"
                onClick={() => setAllDayExpanded(false)}
                className="absolute right-1 text-micro text-muted-foreground hover:text-foreground"
                style={{ top: (shownRows - 1) * ALL_DAY_ROW_PITCH + 4 }}
              >
                less
              </button>
            )}
          </div>
        </div>
      )}

      <div
        ref={body}
        className="relative grid"
        style={{ height: 24 * HOUR_HEIGHT, gridTemplateColumns: weekTracks(days.length) }}
      >
        <div className="relative border-r border-border">
          {/* No label for hour 0: it would collide with the all-day row. */}
          {Array.from({ length: 23 }, (_, i) => i + 1).map((hour) => (
            <div
              key={hour}
              className="absolute -translate-y-1/2 text-right tabular-nums text-muted-foreground"
              // Regular weight. The gutter is read by position rather than word
              // by word, and it repeats 23 times down every screen; at 500 it
              // competed with the event titles it exists to locate.
              style={{
                top: hour * HOUR_HEIGHT,
                right: TIME_GUTTER_INSET,
                fontSize: 11,
                lineHeight: "16px",
                fontWeight: 400,
              }}
            >
              {hour % 12 === 0 ? 12 : hour % 12}
              {hour < 12 ? " AM" : " PM"}
            </div>
          ))}
        </div>

        {days.map((day) => (
          <DayColumn
            key={day.getTime()}
            day={day}
            events={timed}
            colorFor={colorFor}
            dark={dark}
            selectedId={selectedId}
            dimIds={dimIds}
            // A copy leaves the original alone, so the block underneath is not
            // the thing being dragged: it keeps its colour and keeps the
            // cursor, and the ghost is a new event rather than that one moved.
            draggingId={dragging && !copying ? dragging.eventId : null}
            onSelect={onSelect}
            onOpen={onOpen}
            onGrab={grab}
            registerBlock={registerBlock}
            now={now}
            draft={draft && draft.dayStart === startOfDay(day).getTime() ? draft : null}
            editing={editing}
            onDraftChange={(next) => {
              setDraft(next);
              if (next === null) setEditing(false);
            }}
            onDraftSettled={() => setEditing(true)}
            onCommit={commitDraft}
          />
        ))}

        {/* The drag ghost. One absolutely-positioned rectangle, moved by
            transform, sitting above everything and taking no pointer events —
            so moving it costs a composite, not a layout of the week. */}
        {dragging && ghostPaint && (
          <div
            ref={ghost}
            // Marks a live drag for anything that has to stand out of its way.
            // Trackpad period navigation reads this: a swipe while a block is
            // being dragged is the grid being scrolled, not a week being asked
            // for. See `use-period-wheel.ts`.
            data-calendar-drag
            className="pointer-events-none absolute overflow-hidden px-1 py-[2px]"
            style={{
              left:
                TIME_GUTTER + dragging.originDayIndex * ghostColumnWidth + 1,
              width: Math.max(ghostColumnWidth - BLOCK_RIGHT_GUTTER - 2, 24),
              top: gridTop(dragging.origin.start),
              height: blockHeight(dragging.origin.end - dragging.origin.start),
              borderRadius: BLOCK_RADIUS,
              background: ghostPaint.background,
              color: ghostPaint.color,
              // The block you picked up is the block you are still on, so the
              // ghost carries the cursor while the source block behind it is
              // washed out. Without this the selection mark vanishes for the
              // whole length of a drag — which is when it matters most, because
              // an optimistic move that Google later refuses snaps back and you
              // need to know which block just moved.
              boxShadow: [
                ghostPaint.border ? `inset 0 0 0 1px ${ghostPaint.border}` : undefined,
                dragging.eventId === selectedId && !copying
                  ? selectionShadow(ghostPaint.selectionGap)
                  : "0 4px 16px -4px color-mix(in oklab, var(--foreground) 45%, transparent)",
              ]
                .filter((layer): layer is string => layer !== undefined)
                .join(", "),
              zIndex: Z_NOW + 1,
              willChange: "transform",
            }}
          >
            {/* The badge every desktop uses for "this one is a copy". It sits
                over the title rather than beside it because the ghost is one
                column wide and the title already truncates. */}
            {copying && (
              <span
                aria-hidden
                className="absolute right-[3px] top-[2px] flex h-[14px] w-[14px] items-center justify-center rounded-full font-semibold"
                style={{
                  fontSize: 11,
                  lineHeight: "14px",
                  background: ghostPaint.color,
                  color: ghostPaint.background,
                }}
              >
                +
              </span>
            )}
            <span
              className="block truncate font-semibold"
              style={{ fontSize: 12, lineHeight: "15px", paddingRight: copying ? 16 : 0 }}
            >
              {dragging.title}
            </span>
            <span
              ref={ghostLabel}
              className="block truncate tabular-nums"
              style={{ fontSize: 11, lineHeight: "15px", opacity: 0.85 }}
            >
              {dragLabel(dragging.outcome)}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}

/** Which rendered column an instant belongs to. `-1` when it is off-screen. */
function columnIndexFor(ts: number, days: Date[]): number {
  const target = startOfDay(ts).getTime();
  const index = days.findIndex((day) => startOfDay(day).getTime() === target);
  return index === -1 ? 0 : index;
}

/**
 * Calendar days between two rendered columns.
 *
 * Not `to - from`: with weekends hidden, the column to the right of Friday is
 * Monday, and the event has to land three days later, not one.
 */
function dayDelta(days: Date[], from: number, to: number): number {
  const a = days[from];
  const b = days[to];
  if (!a || !b) return 0;
  return Math.round((startOfDay(b).getTime() - startOfDay(a).getTime()) / DAY);
}

interface DayColumnProps {
  day: Date;
  events: MergedEvent[];
  colorFor: (calendarId: CalendarId) => CalendarColor;
  dark: boolean;
  selectedId: EventId | null;
  dimIds?: ReadonlySet<EventId>;
  draggingId: EventId | null;
  onSelect: (id: EventId) => void;
  onOpen: (id: EventId) => void;
  onGrab: (
    event: ReactPointerEvent,
    merged: MergedEvent,
    kind: "move" | "resize",
    edge: ResizeEdge,
  ) => void;
  registerBlock: (id: EventId, node: HTMLElement | null) => void;
  now: number;
  draft: { dayStart: number; start: number; end: number } | null;
  editing: boolean;
  onDraftChange: (draft: { dayStart: number; start: number; end: number } | null) => void;
  onDraftSettled: () => void;
  onCommit: (intent: "save" | "expand", title: string) => void;
}

/**
 * The dimmed hours either side of the working day, for one column.
 *
 * Renders nothing when the working day is the whole day, which is a real
 * setting and not a degenerate one — some people genuinely do not want the
 * grid to have an opinion about when they work.
 */
function WorkingHoursWash() {
  const { workingHours } = usePreferences();
  if (workingHours.start <= 0 && workingHours.end >= 24) return null;

  const wash = "pointer-events-none absolute inset-x-0 bg-surface-raised/55";
  return (
    <>
      {workingHours.start > 0 && (
        <div className={wash} style={{ top: 0, height: workingHours.start * HOUR_HEIGHT }} />
      )}
      {workingHours.end < 24 && (
        <div className={wash} style={{ top: workingHours.end * HOUR_HEIGHT, bottom: 0 }} />
      )}
    </>
  );
}

function DayColumn({
  day,
  events,
  colorFor,
  dark,
  selectedId,
  dimIds,
  draggingId,
  onSelect,
  onOpen,
  onGrab,
  registerBlock,
  now,
  draft,
  editing,
  onDraftChange,
  onDraftSettled,
  onCommit,
}: DayColumnProps) {
  const dayStart = startOfDay(day).getTime();
  const dayEnd = dayStart + DAY;
  const surface = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [hovered, setHovered] = useState<EventId | null>(null);
  const hoverTimer = useRef<number | null>(null);
  const dragAnchor = useRef<number | null>(null);
  /** Where the press landed, and whether it has since become a real drag. */
  const dragFrom = useRef<{ x: number; y: number; moved: boolean } | null>(null);

  // Column width decides how many columns a cluster may actually show (§2).
  useEffect(() => {
    const node = surface.current;
    if (!node) return;
    const observer = new ResizeObserver(([entry]) => setWidth(entry.contentRect.width));
    observer.observe(node);
    setWidth(node.getBoundingClientRect().width);
    return () => observer.disconnect();
  }, []);

  // Clamp to the day before laying out, so a multi-day event does not drag the
  // whole column's geometry off the bottom of the grid.
  const laid = useMemo(() => {
    const clamped = events
      .filter((m) => m.event.start < dayEnd && m.event.end > dayStart)
      .map((m) => ({
        id: m.event.id,
        start: Math.max(m.event.start, dayStart),
        end: Math.min(m.event.end, dayEnd),
        merged: m,
      }));
    return layoutEvents(clamped);
  }, [events, dayStart, dayEnd]);

  /*
   * Paint order is column order.
   *
   * A cascaded cluster overlaps its blocks, so which one is on top has to be
   * decided rather than inherited. Every unselected block sits at
   * `Z_EVENT`, which leaves the DOM to break the tie — and the DOM is in start
   * order, which is *nearly* column order but not reliably: a short event
   * squeezed into a cluster after two longer ones takes column 2 while a later
   * event reuses column 1, and column 1 would then paint over column 2's strip.
   *
   * Sorting the render list by column fixes it in one line and costs nothing
   * elsewhere: keyboard order comes from the keymap, not from the DOM, and
   * clusters never overlap each other in space, so no two clusters can fight.
   */
  const painted = useMemo(() => [...laid].sort((a, b) => a.column - b.column), [laid]);

  const showNow = now >= dayStart && now < dayEnd;

  function timeAt(clientY: number): number {
    const rect = surface.current?.getBoundingClientRect();
    if (!rect) return dayStart;
    const offset = Math.min(Math.max(clientY - rect.top, 0), 24 * HOUR_HEIGHT);
    return timeForOffset(offset, dayStart);
  }

  function onPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    if ((event.target as HTMLElement).closest("[data-event], [data-draft]")) return;
    const anchor = snapTimeDown(timeAt(event.clientY));
    dragAnchor.current = anchor;
    dragFrom.current = { x: event.clientX, y: event.clientY, moved: false };
    onDraftChange({ dayStart, start: anchor, end: anchor + DEFAULT_EVENT_MINUTES * MINUTE });
    // Capture keeps the drag alive when the pointer leaves the column; a
    // synthetic pointer (tests, automation) has no capture to take, and that
    // must not abort the drag.
    try {
      surface.current?.setPointerCapture(event.pointerId);
    } catch {
      /* no capture available for this pointer */
    }
  }

  function onPointerMove(event: ReactPointerEvent<HTMLDivElement>) {
    const anchor = dragAnchor.current;
    const from = dragFrom.current;
    if (anchor === null || from === null) return;
    // Below the drag threshold this is still a click, and a click means the
    // default length — not "whatever quarter hour the cursor is nearest".
    if (!from.moved && !isDrag(event.clientX - from.x, event.clientY - from.y)) return;
    from.moved = true;
    const edge = snapTime(timeAt(event.clientY));
    const start = Math.min(anchor, edge);
    const end = Math.max(anchor + SNAP_MINUTES * MINUTE, edge);
    onDraftChange({ dayStart, start, end });
  }

  function onPointerUp(event: ReactPointerEvent<HTMLDivElement>) {
    const anchor = dragAnchor.current;
    if (anchor === null) return;
    const moved = dragFrom.current?.moved === true;
    dragAnchor.current = null;
    dragFrom.current = null;
    try {
      surface.current?.releasePointerCapture(event.pointerId);
    } catch {
      /* never captured */
    }
    // A press that never travelled has no edge to speak of. Passing the pointer's
    // *snapped* position instead would make a 30-minute click into a 15-minute
    // event whenever it landed in the back half of a quarter hour, which is half
    // the grid — §3 says a plain click is 30 minutes, always.
    const outcome = createResult(anchor, moved ? snapTime(timeAt(event.clientY)) : null);
    onDraftChange({ dayStart, ...outcome });
    onDraftSettled();
  }

  function hoverIn(id: EventId, cluster: number) {
    if (cluster < EXPAND_CLUSTER_MIN) return;
    if (hoverTimer.current !== null) window.clearTimeout(hoverTimer.current);
    hoverTimer.current = window.setTimeout(() => setHovered(id), EXPAND_DELAY_MS);
  }

  function hoverOut() {
    if (hoverTimer.current !== null) window.clearTimeout(hoverTimer.current);
    setHovered(null);
  }

  return (
    <div
      className="relative min-w-0 border-r border-border last:border-r-0"
      style={{
        backgroundImage: `repeating-linear-gradient(to bottom, var(--border) 0 1px, transparent 1px ${HOUR_HEIGHT}px)`,
      }}
    >
      {/*
        The hours outside the working day, dimmed.

        Shading the *outside* rather than lighting the inside is the way round
        that survives both themes: the working day is already the page, and a
        lit band would have to be a second background colour that reads as a
        highlight in light mode and as a hole in dark. A wash over the rest
        reads as "quieter" in both.

        It sits above the hour lines and below everything interactive — no
        pointer events, no z-index, so DOM order alone keeps it under the
        blocks and under the drag surface.
      */}
      <WorkingHoursWash />

      {/* Google leaves a 13px strip clear on the right of every column so there
          is always somewhere to drag-create even on a full day. Copy it. */}
      <div
        ref={surface}
        // The day this column is, for anything working back from a pointer to a
        // time. Drag-to-create measures against this element already; the
        // right-click menu reads the attribute and does the same arithmetic
        // rather than inventing a second way to find the hour under the cursor.
        data-day-start={dayStart}
        className="absolute inset-y-0 left-0"
        style={{ right: BLOCK_RIGHT_GUTTER }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
      >
        {painted.map((item) => {
          const merged = item.event.merged;
          const event = merged.event;
          const columns = visibleColumns(item.columns, width);
          if (item.column >= columns) return null;
          const height = blockHeight(item.event.end - item.event.start);
          const expanded = hovered === event.id;
          const span = Math.max(1, Math.min(item.span, columns - item.column));
          const geometry = columnGeometry({ ...item, columns, span }, width);
          const cluster = clusterPlan(columns, width);
          // A cascaded block runs to the right edge of the cluster and is only
          // *covered* by the blocks after it, so the width its text is laid out
          // in is the whole remainder — not the strip you can currently see.
          // That is what makes selecting one reveal its title with no reflow.
          const rendered =
            cluster.mode === "cascade"
              ? Math.max(width - item.column * cluster.step, 0)
              : (width * span) / columns;
          // A block clipped by the day boundary is a *slice* of the event: its
          // top and bottom are the day's edges, not the event's, so neither
          // dragging nor resizing it could mean what it looks like it means.
          // Those move from the modal instead.
          //
          // A merged block is still draggable. It stands for the copy that is
          // rendered, moving it moves that copy, and the modal says so out
          // loud — which is better than a meeting on two accounts being the
          // one thing on the grid you cannot move.
          const wholeInThisDay = event.start >= dayStart && event.end <= dayEnd;
          return (
            <div key={event.id} data-event>
              <EventBlock
                event={event}
                color={colorFor(event.calendarId)}
                dark={dark}
                tone={toneFor(event.rsvp)}
                past={event.end < now}
                // While you are dragging it, the cursor lives on the ghost —
                // the ghost is the event now, and two accent halos on screen
                // at once (the hole it left, and the thing under the pointer)
                // read as two cursors rather than one move in progress.
                selected={event.id === selectedId && event.id !== draggingId}
                dimmed={event.id === draggingId || (dimIds?.has(event.id) ?? false)}
                copies={merged.copies.length}
                height={height}
                width={expanded ? width : rendered}
                cascaded={cluster.mode === "cascade" && item.column > 0}
                resizable={wholeInThisDay}
                blockRef={(node) => registerBlock(event.id, node)}
                onSelect={() => onOpen(event.id)}
                onGrab={
                  wholeInThisDay
                    ? (pointer, kind, edge) => onGrab(pointer, merged, kind, edge)
                    : undefined
                }
                onPointerEnter={() => hoverIn(event.id, item.columns)}
                onPointerLeave={hoverOut}
                style={{
                  top: offsetForTime(item.event.start, dayStart),
                  height,
                  left: expanded ? 0 : geometry.left,
                  width: expanded ? "100%" : geometry.width,
                  zIndex: expanded
                    ? Z_EVENT_HOVER
                    : event.id === selectedId
                      ? Z_EVENT_SELECTED
                      : Z_EVENT,
                }}
              />
            </div>
          );
        })}

        <OverflowChips laid={laid} width={width} dayStart={dayStart} onSelect={onSelect} />

        {draft && (
          <DraftBlock
            draft={draft}
            dayStart={dayStart}
            editing={editing}
            dark={dark}
            onCommit={onCommit}
            onCancel={() => onDraftChange(null)}
          />
        )}
      </div>

      {showNow && (
        <div
          className="pointer-events-none absolute inset-x-0"
          style={{ top: offsetForTime(now, dayStart), zIndex: Z_NOW }}
        >
          <div className="h-[2px] w-full bg-danger" />
          <span className="absolute -left-[6px] -top-[5px] h-3 w-3 rounded-full bg-danger" />
        </div>
      )}
    </div>
  );
}

/**
 * When a cluster is too dense to show every column at ≥40px, the last visible
 * column carries a `+N` chip rather than a 12px sliver nobody can read.
 */
function OverflowChips({
  laid,
  width,
  dayStart,
  onSelect,
}: {
  laid: ReturnType<typeof layoutEvents<{ id: EventId; start: number; end: number; merged: MergedEvent }>>;
  width: number;
  dayStart: number;
  onSelect: (id: EventId) => void;
}) {
  const hidden = laid.filter((item) => {
    const columns = visibleColumns(item.columns, width);
    return item.column >= columns;
  });
  if (hidden.length === 0) return null;

  const top = Math.min(...hidden.map((item) => offsetForTime(item.event.start, dayStart)));
  return (
    <button
      type="button"
      data-event
      onClick={() => onSelect(hidden[0].event.id)}
      className="absolute right-0 rounded-[4px] bg-surface-raised px-1 text-micro text-muted-foreground hover:text-foreground"
      style={{ top, zIndex: Z_EVENT_HOVER, height: 14, lineHeight: "14px" }}
      title={hidden.map((item) => item.event.merged.event.title).join("\n")}
    >
      +{hidden.length}
    </button>
  );
}

/**
 * A block being drawn belongs to no calendar yet — the destination is chosen on
 * save — so it takes the first slot of the fallback ramp rather than any real
 * calendar's colour, which would be a claim about where it is going.
 */
const DRAFT_COLOR = fallbackFill(0);

/**
 * The provisional block. Notion Calendar's signature move: the title field is
 * *in* the block, not in a modal. Enter saves, Esc discards, Tab hands it to
 * the full composer.
 */
function DraftBlock({
  draft,
  dayStart,
  editing,
  dark,
  onCommit,
  onCancel,
}: {
  draft: { start: number; end: number };
  dayStart: number;
  editing: boolean;
  dark: boolean;
  onCommit: (intent: "save" | "expand", title: string) => void;
  onCancel: () => void;
}) {
  const [title, setTitle] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const height = blockHeight(draft.end - draft.start);
  const paint = paintFor(DRAFT_COLOR, "solid", { dark });

  useEffect(() => {
    if (editing) input.current?.focus();
  }, [editing]);

  const style: CSSProperties = {
    top: offsetForTime(draft.start, dayStart),
    height,
    left: 1,
    right: 1,
    zIndex: Z_EVENT_HOVER + 1,
    borderRadius: BLOCK_RADIUS,
    background: paint.background,
    color: paint.color,
    opacity: editing ? 1 : 0.85,
  };

  return (
    <div data-draft className="absolute overflow-hidden px-1 py-[2px]" style={style}>
      {editing ? (
        <input
          ref={input}
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="New event"
          className="w-full bg-transparent outline-none placeholder:opacity-70"
          style={{ fontSize: 12, lineHeight: "15px", fontWeight: 600, color: paint.color }}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              onCommit("save", title.trim());
            } else if (e.key === "Escape") {
              e.preventDefault();
              onCancel();
            } else if (e.key === "Tab") {
              e.preventDefault();
              onCommit("expand", title.trim());
            }
          }}
          onBlur={() => onCancel()}
        />
      ) : (
        <span style={{ fontSize: 12, lineHeight: "15px", fontWeight: 600 }}>
          {shortTime(draft.start)} – {shortTime(draft.end)}
        </span>
      )}
      {editing && height >= 34 && (
        <span className="block tabular-nums" style={{ fontSize: 11, lineHeight: "15px", opacity: 0.85 }}>
          {shortTime(draft.start)} – {shortTime(draft.end)}
        </span>
      )}
    </div>
  );
}
