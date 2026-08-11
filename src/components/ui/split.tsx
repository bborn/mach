import { useCallback, useEffect, useRef, useState } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { cn } from "@/lib/utils";

/** How far one arrow key moves a divider. */
export const RESIZE_STEP = 16;

interface ResizerProps {
  /** Current size of the pane this handle sizes, in pixels. */
  size: number;
  onResize: (size: number) => void;
  /**
   * The end of a gesture: the pointer released, an arrow pressed, Home or End.
   *
   * Separate from `onResize` because a drag fires that a few hundred times and
   * only the last of them is worth writing to the store. A handle whose
   * consumer is happy to persist every intermediate value simply omits this and
   * does its remembering in `onResize`.
   */
  onCommit?: (size: number) => void;
  /**
   * What a double-click on the divider does. Omitted, it does nothing — which
   * is the right answer for a divider with no default worth going back to.
   */
  onReset?: () => void;
  /** The range the handle may produce. Also what it reports to the reader. */
  min: number;
  max: number;
  /** What this divides, said in a few words. Read aloud; never rendered. */
  label: string;
  /**
   * Which way it moves, and therefore which pane it sizes.
   *
   * `x` is the column divider: the pane is to its **left**, and it grows as the
   * pointer moves right. `y` is the drawer divider: the pane is **below** it,
   * and it grows as the pointer moves up. Those are the only two arrangements
   * in the app, so they are the whole vocabulary rather than a general theory
   * of splitters.
   */
  axis?: "x" | "y";
  className?: string;
}

/**
 * A one-pixel divider that happens to be draggable. The hit area is eight
 * pixels wide and invisible; the line itself never thickens, because a
 * thickening divider is the kind of motion that makes an app feel like a toy.
 *
 * # It answers the keyboard as well as the pointer
 *
 * A divider only a mouse can move fails the app's first rule, so the handle is
 * a focus stop and the arrow keys move it — ← → for a column, ↑ ↓ for the
 * drawer — with Home and End for the ends of the range. The bindings go
 * through the keymap registry rather than an `onKeyDown` because the registry
 * listens in the capture phase: a local handler would never see an arrow key
 * that the mail list had already claimed. They are live only while the handle
 * has focus, and carry no description, so they cost the `?` sheet nothing.
 *
 * Focus is not always a keystroke away, though — both modes spend ⇥ on their
 * own rail-and-list loop — so a divider that matters also wants a binding of
 * its own from its consumer. `AgentDock`, `ComposerDock` and `AccountRail` each
 * register one.
 *
 * # Double-click goes back to the default
 *
 * Only where the consumer passes `onReset`. The handle knows the range it may
 * produce; it does not know which value in that range is the one the app was
 * designed around.
 */
export function Resizer({
  size,
  onResize,
  onCommit,
  onReset,
  min,
  max,
  label,
  axis = "x",
  className,
}: ResizerProps) {
  const [dragging, setDragging] = useState(false);
  const [focused, setFocused] = useState(false);
  const origin = useRef({ point: 0, size: 0 });
  /*
   * The last size this handle produced, so `pointerup` has something to commit.
   *
   * A ref rather than reading `size` back: the consumer may hold the value in
   * state that has not re-rendered this component yet when the pointer comes
   * up, and committing a stale number would write the second-to-last frame of
   * the drag.
   */
  const latest = useRef(size);

  // Every route in — drag, arrow, Home, End — comes through here, so the range
  // is enforced once. The consumer clamps too, against limits this cannot know.
  const resize = useCallback(
    (next: number) => {
      const clamped = Math.min(max, Math.max(min, Math.round(next)));
      latest.current = clamped;
      onResize(clamped);
      return clamped;
    },
    [onResize, min, max],
  );

  /** Move it and call the gesture over — what a keystroke is. */
  const step = useCallback((next: number) => void onCommit?.(resize(next)), [resize, onCommit]);

  const onPointerDown = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      origin.current = { point: axis === "x" ? event.clientX : event.clientY, size };
      latest.current = size;
      setDragging(true);
      event.currentTarget.setPointerCapture(event.pointerId);
    },
    [axis, size],
  );

  useEffect(() => {
    if (!dragging) return;
    const onMove = (event: PointerEvent) => {
      const moved = (axis === "x" ? event.clientX : event.clientY) - origin.current.point;
      // Down is positive on the y axis and the pane is below the handle, so
      // the drawer grows as the pointer goes up.
      resize(origin.current.size + (axis === "x" ? moved : -moved));
    };
    const onUp = () => {
      setDragging(false);
      onCommit?.(latest.current);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
  }, [dragging, resize, axis, onCommit]);

  useKeyBindings([
    {
      keys: axis === "x" ? "right" : "up",
      priority: 250,
      when: () => focused,
      handler: () => step(size + RESIZE_STEP),
    },
    {
      keys: axis === "x" ? "left" : "down",
      priority: 250,
      when: () => focused,
      handler: () => step(size - RESIZE_STEP),
    },
    { keys: "home", priority: 250, when: () => focused, handler: () => step(min) },
    { keys: "end", priority: 250, when: () => focused, handler: () => step(max) },
  ]);

  return (
    <div
      role="separator"
      aria-orientation={axis === "x" ? "vertical" : "horizontal"}
      aria-label={label}
      aria-valuenow={Math.round(size)}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onDoubleClick={onReset}
      onFocus={() => setFocused(true)}
      onBlur={() => setFocused(false)}
      className={cn(
        "relative z-10 shrink-0 outline-none",
        axis === "x"
          ? "-ml-1 w-2 cursor-col-resize after:absolute after:inset-y-0 after:left-1 after:w-px"
          : "-mt-1 h-2 cursor-row-resize after:absolute after:inset-x-0 after:top-1 after:h-px",
        "after:bg-transparent",
        // Lit while it is being moved — by the pointer, or by the keyboard,
        // which is also the focus ring it would otherwise need.
        (dragging || focused) && "after:bg-accent",
        className,
      )}
    />
  );
}

interface PaneProps {
  children: React.ReactNode;
  className?: string;
  width?: number;
}

/** A fixed-width column in the three-pane layout. */
export function Pane({ children, className, width }: PaneProps) {
  return (
    <div
      className={cn("flex min-h-0 shrink-0 flex-col border-r border-border", className)}
      style={width === undefined ? undefined : { width }}
    >
      {children}
    </div>
  );
}

/** The pane that takes whatever is left. */
export function FlexPane({ children, className }: { children: React.ReactNode; className?: string }) {
  return <div className={cn("flex min-h-0 min-w-0 flex-1 flex-col", className)}>{children}</div>;
}
