import { useMemo, useState } from "react";
import type { CalendarId, EventId } from "@/types";
import type { MergedEvent } from "@/lib/calendar-merge";
import { toneFor, type HueIndex } from "@/lib/calendar-palette";
import { HOUR, addDays, isToday, monthShort, startOfDay, weekdayShort } from "@/lib/time";
import { cn } from "@/lib/utils";
import { EventChip } from "./EventBlock";

const MAX_CHIPS = 3;

interface MonthGridProps {
  days: Date[];
  anchorMonth: number;
  events: MergedEvent[];
  hueFor: (id: CalendarId) => HueIndex;
  dark: boolean;
  selectedId: EventId | null;
  onSelect: (id: EventId) => void;
}

export function MonthGrid({
  days,
  anchorMonth,
  events,
  hueFor,
  dark,
  selectedId,
  onSelect,
}: MonthGridProps) {
  const [expanded, setExpanded] = useState<number | null>(null);
  const now = Date.now();

  /**
   * Every day each event covers — not only the day it starts on.
   *
   * Bucketing by start alone made a week in Lisbon a single chip on the Monday
   * and nothing for the rest of it, and made a trip that *began* before this
   * month vanish from the month entirely: its start day is not a key in the
   * grid, so the row was silently dropped. Both are the calendar lying about
   * what is on it, which is worse than any amount of clutter.
   *
   * Day bounds come from `startOfDay` on each column rather than from adding
   * 86,400,000 — a DST day is 23 or 25 hours long and arithmetic on the
   * millisecond value drifts across one.
   */
  const byDay = useMemo(() => {
    const bounds = days.map((day) => ({
      start: startOfDay(day).getTime(),
      end: startOfDay(addDays(day, 1)).getTime(),
    }));
    const map = new Map<number, MergedEvent[]>(bounds.map((b) => [b.start, []]));
    for (const item of events) {
      const { start } = item.event;
      // Overlap, not containment. `start + 1` keeps a zero-length event on its
      // own day instead of on none.
      const end = Math.max(item.event.end, start + 1);
      for (const bound of bounds) {
        if (start < bound.end && end > bound.start) map.get(bound.start)!.push(item);
      }
    }
    for (const list of map.values()) {
      list.sort((a, b) => a.event.start - b.event.start || a.event.id - b.event.id);
    }
    return map;
  }, [days, events]);

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

      <div className="grid min-h-0 flex-1 grid-cols-7 grid-rows-6">
        {days.map((day) => {
          const key = startOfDay(day).getTime();
          const items = byDay.get(key) ?? [];
          const outside = day.getMonth() !== new Date(anchorMonth).getMonth();
          const first = day.getDate() === 1;
          const open = expanded === key;
          const shown = open ? items : items.slice(0, MAX_CHIPS);

          return (
            <div
              key={key}
              className={cn(
                "flex min-h-0 min-w-0 flex-col gap-0.5 border-b border-r border-border p-1",
                open ? "overflow-y-auto" : "overflow-hidden",
                outside && "bg-surface",
              )}
            >
              <div className="flex shrink-0 items-baseline gap-1">
                {first && (
                  <span className="text-micro uppercase text-faint-foreground">
                    {monthShort(day)}
                  </span>
                )}
                <span
                  className={cn(
                    "font-mono text-micro tabular-nums",
                    isToday(day)
                      ? "rounded-full bg-accent px-1 text-accent-foreground"
                      : outside
                        ? "text-faint-foreground"
                        : "text-muted-foreground",
                  )}
                >
                  {day.getDate()}
                </span>
                {items.length > MAX_CHIPS && (
                  <button
                    type="button"
                    onClick={() => setExpanded(open ? null : key)}
                    className="ml-auto font-mono text-micro text-faint-foreground hover:text-foreground"
                  >
                    {open ? "less" : `+${items.length - MAX_CHIPS}`}
                  </button>
                )}
              </div>

              {shown.map((item) => (
                <EventChip
                  key={item.event.id}
                  event={item.event}
                  hue={hueFor(item.event.calendarId)}
                  dark={dark}
                  tone={toneFor(item.event.rsvp)}
                  past={item.event.end < now}
                  selected={item.event.id === selectedId}
                  copies={item.copies.length}
                  showTime={item.event.end - item.event.start < 24 * HOUR}
                  style={{ height: 18, lineHeight: "18px", fontSize: 11, padding: "0 5px" }}
                  onSelect={() => onSelect(item.event.id)}
                />
              ))}
            </div>
          );
        })}
      </div>
    </div>
  );
}
