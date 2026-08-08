import { Search } from "lucide-react";
import { useEffect, useRef } from "react";
import { Kbd } from "@/components/ui/kbd";
import { BareInput } from "@/components/ui/input";
import { fullDate } from "@/lib/time";

export interface EventFinderProps {
  query: string;
  onQuery: (value: string) => void;
  /** How many events the query matches, and which one is highlighted. */
  count: number;
  index: number;
  /** True when the only matches were found outside the week on screen. */
  widened: boolean;
  /** Start of the highlighted match, for the "not this week" line. */
  matchStart: number | null;
  onEnter: () => void;
  onCycle: (delta: 1 | -1) => void;
  onCancel: () => void;
}

/**
 * Find an event by typing its name.
 *
 * **A bar, not a dialog.** Every other search surface in this app is an
 * overlay, and that is right for mail, where the results replace the list. Here
 * the results *are* the week already on screen: the matches keep their places
 * and everything else dims, so the answer to "which Tuesday was that on?" is
 * visible in the same glance as the match itself. An overlay would cover the
 * one thing the user is trying to read.
 *
 * The field is a real input with its own `onKeyDown` rather than four more
 * keymap bindings. Enter, Tab and Escape all mean something different while a
 * caret is in a field, and the keymap's `allowInInput` escape hatch exists for
 * bindings that must fire *through* a field — not for the field's own keys.
 */
export function EventFinder({
  query,
  onQuery,
  count,
  index,
  widened,
  matchStart,
  onEnter,
  onCycle,
  onCancel,
}: EventFinderProps) {
  const field = useRef<HTMLInputElement>(null);

  useEffect(() => {
    field.current?.focus();
  }, []);

  const typed = query.trim().length > 0;

  return (
    <div className="flex shrink-0 items-center gap-2 border-b border-border bg-surface-raised px-3 py-1.5">
      <Search size={12} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
      <BareInput
        ref={field}
        value={query}
        placeholder="Type part of an event's name"
        aria-label="Find an event"
        className="min-w-0 flex-1 text-list"
        onChange={(event) => onQuery(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            onEnter();
            return;
          }
          if (event.key === "Tab") {
            event.preventDefault();
            onCycle(event.shiftKey ? -1 : 1);
            return;
          }
          if (event.key === "Escape") {
            event.preventDefault();
            onCancel();
          }
        }}
      />

      {/* Say what happened, always. A query that matches nothing used to be
          indistinguishable from a query that had not been typed yet. */}
      {typed && count === 0 && (
        <span className="shrink-0 whitespace-nowrap text-micro text-danger">
          Nothing matches “{query.trim()}”
        </span>
      )}
      {count > 0 && (
        <span className="shrink-0 whitespace-nowrap font-mono text-micro tabular-nums text-muted-foreground">
          {index + 1} of {count}
        </span>
      )}
      {/* Widening is silent until it succeeds, and then it says so — landing on
          a match three weeks away without a word would read as the calendar
          having jumped on its own. */}
      {widened && matchStart !== null && (
        <span className="shrink-0 whitespace-nowrap text-micro text-warning">
          not this view — {fullDate(matchStart)}
        </span>
      )}

      <span className="shrink-0 whitespace-nowrap text-micro text-faint-foreground">
        <Kbd keys="enter" /> open · <Kbd keys="tab" /> next · <Kbd keys="escape" /> cancel
      </span>
    </div>
  );
}
