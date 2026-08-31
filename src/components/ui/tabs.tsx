import { useEffect, useRef, useState, type ReactNode } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";

export interface TabItem {
  id: string;
  /** What the tab draws. */
  children: ReactNode;
  /** Read aloud, and what a test looks for. Never rendered. */
  label: string;
  /** The chord that also selects this tab, as an `aria-keyshortcuts` value. */
  keyshortcuts?: string;
}

interface TabStripProps {
  items: readonly TabItem[];
  activeId: string | null;
  onSelect: (id: string) => void;
  /** What this row of tabs is a row of. Read aloud; never rendered. */
  label: string;
  className?: string;
  /** Extra classes on every tab, for a consumer with its own measure. */
  tabClassName?: string;
}

/**
 * A row of tabs that is **one** tab stop, not one per tab.
 *
 * # Why this is a primitive and not a `<button>` loop
 *
 * The composer's strip was a row of bare `<button>`s, and it had the three
 * faults a hand-rolled tab row always has: no role, so a screen reader read a
 * row of unrelated buttons; a tab stop per tab, so ⇥ out of a field walked
 * every open draft before it reached the message; and no arrow keys, so the
 * chord was the only way in.
 *
 * So: `role="tablist"`, a roving `tabIndex` — the selected tab is the stop, the
 * rest are -1 — and ← → Home End inside it. The same shape as the date grid in
 * `date-field.tsx`, for the same reason: the strip is one control.
 *
 * `SessionPane` still draws its own; it is the second consumer this is shaped
 * for, and moving it over is a separate change.
 *
 * # Focus moves; ⏎ selects
 *
 * ARIA allows either, and the choice turns on what selecting costs. Here it can
 * navigate to another conversation, so arrowing across four tabs with automatic
 * activation would be four navigations nobody asked for. Focus moves on its
 * own; ⏎ or Space is the act.
 *
 * # The bindings go through the keymap, not `onKeyDown`
 *
 * Same reason `Resizer` does it: the registry listens in the capture phase, so
 * a local handler would never see an arrow the mail list had already claimed —
 * nor ⏎, which opens a thread. They are live only while focus is inside the
 * strip, and carry no description, so the `?` sheet is unchanged.
 */
export function TabStrip({
  items,
  activeId,
  onSelect,
  label,
  className,
  tabClassName,
}: TabStripProps) {
  const [focusedId, setFocusedId] = useState<string | null>(null);
  const nodes = useRef(new Map<string, HTMLButtonElement>());

  /**
   * Put focus on the tab `delta` away from wherever it is now.
   *
   * Written as a plain closure rather than a `useCallback`: `useKeyBindings`
   * re-reads its handlers on every render, so this one always sees the current
   * list and the current focus without the registry churning.
   */
  const move = (delta: number | "first" | "last") => {
    if (items.length === 0) return;
    const from = focusedId ?? activeId;
    const at = Math.max(0, items.findIndex((item) => item.id === from));
    const next =
      delta === "first"
        ? 0
        : delta === "last"
          ? items.length - 1
          : Math.min(items.length - 1, Math.max(0, at + delta));
    nodes.current.get(items[next]!.id)?.focus();
  };

  const inside = focusedId !== null;

  useKeyBindings([
    { keys: "left", priority: 250, when: () => inside, handler: () => move(-1) },
    { keys: "right", priority: 250, when: () => inside, handler: () => move(1) },
    { keys: "home", priority: 250, when: () => inside, handler: () => move("first") },
    { keys: "end", priority: 250, when: () => inside, handler: () => move("last") },
    {
      keys: "enter",
      priority: 250,
      when: () => inside,
      handler: () => focusedId !== null && onSelect(focusedId),
    },
    {
      keys: "space",
      priority: 250,
      when: () => inside,
      handler: () => focusedId !== null && onSelect(focusedId),
    },
  ]);

  /*
   * The selected tab is brought into view when it changes, because the strip
   * scrolls sideways once there are more tabs than fit and the chord can select
   * one that is off the end of it.
   */
  useEffect(() => {
    if (activeId === null) return;
    nodes.current.get(activeId)?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [activeId]);

  /*
   * Which tab ⇥ lands on. The selected one, and the first where nothing is
   * selected — a strip with every tab at -1 is a strip the keyboard cannot
   * reach at all, which is the failure this pattern exists to avoid.
   */
  const stopId = items.some((item) => item.id === activeId) ? activeId : (items[0]?.id ?? null);

  return (
    <div role="tablist" aria-label={label} aria-orientation="horizontal" className={className}>
      {items.map((item) => {
        const selected = item.id === activeId;
        return (
          <button
            key={item.id}
            type="button"
            role="tab"
            aria-selected={selected}
            aria-label={item.label}
            aria-keyshortcuts={item.keyshortcuts}
            // The roving stop. Everything else is reachable with ← →, and with
            // nothing at all if the consumer has a chord.
            tabIndex={item.id === stopId ? 0 : -1}
            ref={(node) => {
              if (node) nodes.current.set(item.id, node);
              else nodes.current.delete(item.id);
            }}
            onClick={() => onSelect(item.id)}
            onFocus={() => setFocusedId(item.id)}
            onBlur={() => setFocusedId((current) => (current === item.id ? null : current))}
            className={tabClassName}
          >
            {item.children}
          </button>
        );
      })}
    </div>
  );
}
