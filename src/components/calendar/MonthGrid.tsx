import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import type { CalendarId, EventId } from "@/types";
import type { MergedEvent } from "@/lib/calendar-merge";
import {
  ALL_DAY_MAX_ROWS,
  BLOCK_RADIUS,
  clipToDays,
  eventGridRange,
  packRows,
  type RowItem,
} from "@/lib/calendar-geometry";
import {
  cellIndexAt,
  isCopyDrag,
  isDrag,
  moveByDays,
  type EventMove,
} from "@/lib/calendar-drag";
import { paintFor, toneFor, type CalendarColor } from "@/lib/calendar-palette";
import { addDays, HOUR, isToday, monthShort, startOfDay, weekdayShort } from "@/lib/time";
import { cn } from "@/lib/utils";
import { EventChip } from "./EventBlock";

/** Tighter than the week all-day strip: a month cell has six weeks to fit. */
const CHIP_HEIGHT = 18;
const ROW_PITCH = 20;
/** Date number plus the padding above the bars, so a spanning bar never covers it. */
const DATE_ROW = 22;

interface MonthGridProps {
  days: Date[];
  anchorMonth: number;
  events: MergedEvent[];
  colorFor: (id: CalendarId) => CalendarColor;
  dark: boolean;
  selectedId: EventId | null;
  /**
   * Events the type-to-select bar ruled out. They stay in place and fade, so
   * the shape of the month is still readable while the matches stand out of it.
   */
  dimIds?: ReadonlySet<EventId>;
  onSelect: (id: EventId) => void;
  onMove: (move: EventMove) => void;
}

interface MonthDrag {
  eventId: EventId;
  merged: MergedEvent;
  originStart: number;
  originEnd: number;
  allDay: boolean;
  originCell: number;
  span: number;
  grabOffset: number;
  pointerId: number;
  startX: number;
  startY: number;
  moved: boolean;
  copy: boolean;
  outcome: { start: number; end: number };
}

interface WeekBar extends RowItem {
  merged: MergedEvent;
  row: number;
}

export function MonthGrid({
  days,
  anchorMonth,
  events,
  colorFor,
  dark,
  selectedId,
  dimIds,
  onSelect,
  onMove,
}: MonthGridProps) {
  const [expandedWeek, setExpandedWeek] = useState<number | null>(null);
  const now = Date.now();
  const weeks = useMemo(() => chunkWeeks(days), [days]);
  const packed = useMemo(() => weeks.map((week) => packWeek(week, events)), [weeks, events]);
  const grid = useRef<HTMLDivElement>(null);
  const session = useRef<MonthDrag | null>(null);
  const swallowClick = useRef(false);
  const [dragging, setDragging] = useState<MonthDrag | null>(null);
  const [copying, setCopying] = useState(false);

  const syncCopy = (altKey: boolean) => {
    const live = session.current;
    if (!live) return;
    const next = isCopyDrag("move", altKey);
    if (next === live.copy) return;
    live.copy = next;
    setCopying(next);
  };

  const endDrag = useCallback(
    (commit: boolean) => {
      const live = session.current;
      session.current = null;
      setDragging(null);
      setCopying(false);
      if (!live) return;
      swallowClick.current = live.moved;
      if (
        commit &&
        live.moved &&
        (live.outcome.start !== live.originStart || live.outcome.end !== live.originEnd)
      ) {
        onMove({
          eventId: live.eventId,
          start: live.outcome.start,
          end: live.outcome.end,
          copy: live.copy,
          allDay: live.allDay,
        });
      }
    },
    [onMove],
  );

  useEffect(() => {
    if (!dragging) return;
    const onPointerMove = (event: PointerEvent) => {
      const live = session.current;
      if (!live || event.pointerId !== live.pointerId) return;
      syncCopy(event.altKey);
      const dx = event.clientX - live.startX;
      const dy = event.clientY - live.startY;
      if (!live.moved && !isDrag(dx, dy)) return;
      live.moved = true;
      const rect = grid.current?.getBoundingClientRect();
      if (!rect) return;
      const pointerCell = cellIndexAt(event.clientX, event.clientY, rect, 7, weeks.length);
      const startCell = Math.min(
        Math.max(pointerCell - live.grabOffset, 0),
        Math.max(days.length - live.span, 0),
      );
      live.outcome = moveByDays(
        { start: live.originStart, end: live.originEnd, allDay: live.allDay },
        startCell - live.originCell,
      );
      setDragging({ ...live });
    };
    const onPointerUp = (event: PointerEvent) => {
      if (session.current && event.pointerId !== session.current.pointerId) return;
      syncCopy(event.altKey);
      endDrag(true);
    };
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
    const root = document.documentElement;
    const previous = root.style.cursor;
    root.style.cursor = copying ? "copy" : "grabbing";
    return () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("pointercancel", onPointerUp);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("keyup", onKeyUp, true);
      root.style.cursor = previous;
    };
  }, [dragging, copying, days.length, weeks.length, endDrag]);

  const grab = (event: ReactPointerEvent, merged: MergedEvent) => {
    if (event.button !== 0) return;
    const rect = grid.current?.getBoundingClientRect();
    if (!rect) return;
    const target = merged.event;
    const range = eventGridRange(target);
    const originCell = Math.max(0, cellOf(days, range.start));
    const span = dayCount(range);
    const pointerCell = cellIndexAt(event.clientX, event.clientY, rect, 7, weeks.length);
    const next: MonthDrag = {
      eventId: target.id,
      merged,
      originStart: target.start,
      originEnd: target.end,
      allDay: target.allDay,
      originCell,
      span,
      grabOffset: Math.min(Math.max(pointerCell - originCell, 0), Math.max(span - 1, 0)),
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      moved: false,
      copy: isCopyDrag("move", event.altKey),
      outcome: { start: target.start, end: target.end },
    };
    session.current = next;
    setDragging(next);
    setCopying(next.copy);
    event.preventDefault();
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 border-b border-border">
        {days.slice(0, 7).map((day) => (
          <div
            key={day.getTime()}
            className="flex-1 border-r border-border px-2 py-1 text-micro font-medium uppercase tracking-[0.06em] text-faint-foreground last:border-r-0"
          >
            {weekdayShort(day)}
          </div>
        ))}
      </div>

      <div
        ref={grid}
        className="relative grid min-h-0 flex-1 grid-rows-6"
        onClickCapture={(event) => {
          if (!swallowClick.current) return;
          swallowClick.current = false;
          event.preventDefault();
          event.stopPropagation();
        }}
      >
        {weeks.map((week, weekIndex) => {
          const bars = packed[weekIndex];
          const rowCount = bars.reduce((max, bar) => Math.max(max, bar.row + 1), 0);
          const open = expandedWeek === weekIndex;
          const shownRows = open ? rowCount : Math.min(rowCount, ALL_DAY_MAX_ROWS);
          const hiddenOnDay = hiddenCounts(week, bars, shownRows);

          const contentHeight = DATE_ROW + shownRows * ROW_PITCH;

          return (
            <div
              key={startOfDay(week[0]).getTime()}
              className={cn(
                "relative min-h-0 min-w-0 border-b border-border",
                open ? "overflow-y-auto" : "overflow-hidden",
              )}
            >
              {/* Height is the bars when they overflow, and the week cell when
                  they do not — so "+N" can actually scroll the extra rows out,
                  and a quiet week still fills the month. */}
              <div className="relative min-h-full" style={{ height: contentHeight }}>
                <div className="absolute inset-0 grid grid-cols-7">
                  {week.map((day, dayIndex) => {
                    const key = startOfDay(day).getTime();
                    const outside = day.getMonth() !== new Date(anchorMonth).getMonth();
                    const first = day.getDate() === 1;
                    const hidden = hiddenOnDay[dayIndex];

                    return (
                      <div
                        key={key}
                        data-day-cell={key}
                        className={cn(
                          "min-h-0 min-w-0 border-r border-border p-1 last:border-r-0",
                          outside && "bg-surface",
                        )}
                      >
                        <div className="flex h-[18px] shrink-0 items-baseline gap-1">
                          {first && (
                            <span className="text-micro uppercase text-faint-foreground">
                              {monthShort(day)}
                            </span>
                          )}
                          <span
                            className={cn(
                              "font-mono text-list font-medium tabular-nums leading-none",
                              isToday(day)
                                ? "rounded-full bg-accent px-1 text-accent-foreground"
                                : outside
                                  ? "text-faint-foreground"
                                  : "text-foreground",
                            )}
                          >
                            {day.getDate()}
                          </span>
                          {hidden > 0 && (
                            <button
                              type="button"
                              onClick={() => setExpandedWeek(open ? null : weekIndex)}
                              className="ml-auto font-mono text-micro text-faint-foreground hover:text-foreground"
                            >
                              {open ? "less" : `+${hidden}`}
                            </button>
                          )}
                        </div>
                      </div>
                    );
                  })}
                </div>

                {/* Overlay, not per-cell chips: a multi-day event is one bar across
                    the days it covers, packed into a shared row so Monday's standup
                    and Tuesday's trip can share a line the way they do in Google. */}
                <div
                  className="pointer-events-none absolute right-0 left-0"
                  style={{ top: DATE_ROW, height: Math.max(shownRows, 1) * ROW_PITCH }}
                >
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
                          tone={toneFor(event.rsvp, event.transparency)}
                          past={event.end < now}
                          selected={event.id === selectedId}
                          dimmed={
                            (dimIds?.has(event.id) ?? false) ||
                            Boolean(dragging && !copying && dragging.eventId === event.id)
                          }
                          copies={bar.merged.copies.length}
                          showTime={
                            !event.allDay &&
                            event.end - event.start < 24 * HOUR &&
                            bar.span === 1
                          }
                          onSelect={() => onSelect(event.id)}
                          onGrab={(pointer) => grab(pointer, bar.merged)}
                          style={{
                            pointerEvents: "auto",
                            position: "absolute",
                            top: bar.row * ROW_PITCH,
                            height: CHIP_HEIGHT,
                            left: `calc(${(bar.startIndex * 100) / week.length}% + 2px)`,
                            width: `calc(${(bar.span * 100) / week.length}% - 4px)`,
                            fontSize: 11,
                            lineHeight: `${CHIP_HEIGHT}px`,
                            padding: "0 4px",
                          }}
                        />
                      );
                    })}
                </div>
              </div>
            </div>
          );
        })}
        {dragging?.moved && (
          <MonthGhost
            drag={dragging}
            days={days}
            weeks={weeks.length}
            dark={dark}
            copying={copying}
            color={colorFor(dragging.merged.event.calendarId)}
          />
        )}
      </div>
    </div>
  );
}

function chunkWeeks(days: Date[]): Date[][] {
  const weeks: Date[][] = [];
  for (let i = 0; i < days.length; i += 7) weeks.push(days.slice(i, i + 7));
  return weeks;
}

function packWeek(week: Date[], events: readonly MergedEvent[]): WeekBar[] {
  const dayStarts = week.map((day) => startOfDay(day).getTime());
  const segments: (RowItem & { merged: MergedEvent })[] = [];
  for (const merged of events) {
    const clipped = clipToDays(eventGridRange(merged.event), dayStarts);
    if (clipped) segments.push({ merged, ...clipped });
  }
  return packRows(segments);
}

function cellOf(days: Date[], ts: number): number {
  const key = startOfDay(ts).getTime();
  return days.findIndex((day) => startOfDay(day).getTime() === key);
}

function dayCount(range: { start: number; end: number }): number {
  let n = 0;
  for (let t = range.start; t < range.end && n < 366; t = addDays(t, 1).getTime()) n++;
  return Math.max(n, 1);
}

function MonthGhost({
  drag,
  days,
  weeks,
  dark,
  copying,
  color,
}: {
  drag: MonthDrag;
  days: Date[];
  weeks: number;
  dark: boolean;
  copying: boolean;
  color: CalendarColor;
}) {
  const event = drag.merged.event;
  const paint = paintFor(color, toneFor(event.rsvp, event.transparency), { dark });
  const range = eventGridRange({
    start: drag.outcome.start,
    end: drag.outcome.end,
    allDay: drag.allDay,
  });
  const startCell = Math.max(0, cellOf(days, range.start));
  const row = Math.floor(startCell / 7);
  const col = startCell % 7;
  const span = Math.min(drag.span, 7 - col);

  return (
    <div
      data-calendar-drag
      className="pointer-events-none absolute overflow-hidden px-1"
      style={{
        top: `calc(${(row / Math.max(weeks, 1)) * 100}% + ${DATE_ROW}px)`,
        height: CHIP_HEIGHT,
        left: `calc(${(col * 100) / 7}% + 2px)`,
        width: `calc(${(span * 100) / 7}% - 4px)`,
        borderRadius: BLOCK_RADIUS,
        background: paint.background,
        color: paint.color,
        fontSize: 11,
        lineHeight: `${CHIP_HEIGHT}px`,
        fontWeight: 600,
        zIndex: 20,
        boxShadow: "0 4px 16px -4px color-mix(in oklab, var(--foreground) 45%, transparent)",
      }}
    >
      {copying && (
        <span
          aria-hidden
          className="absolute right-[3px] top-[1px] flex h-[14px] w-[14px] items-center justify-center rounded-full font-semibold"
          style={{
            fontSize: 11,
            lineHeight: "14px",
            background: paint.color,
            color: paint.background,
          }}
        >
          +
        </span>
      )}
      <span className="block truncate">{event.title}</span>
    </div>
  );
}

function hiddenCounts(week: Date[], bars: readonly WeekBar[], shownRows: number): number[] {
  return week.map((_, dayIndex) =>
    bars.filter(
      (bar) =>
        bar.row >= shownRows &&
        dayIndex >= bar.startIndex &&
        dayIndex < bar.startIndex + bar.span,
    ).length,
  );
}
