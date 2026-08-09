import { useCallback, useEffect, useRef, useState } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { cn } from "@/lib/utils";

/** How far one arrow key moves a divider. */
export const RESIZE_STEP = 16;

interface ResizerProps {
  /** Current size of the pane this handle sizes, in pixels. */
  size: number;
  onResize: (size: number) => void;
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
 */
export function Resizer({ size, onResize, min, max, label, axis = "x", className }: ResizerProps) {
  const [dragging, setDragging] = useState(false);
  const [focused, setFocused] = useState(false);
  const origin = useRef({ point: 0, size: 0 });

  // Every route in — drag, arrow, Home, End — comes through here, so the range
  // is enforced once. The consumer clamps too, against limits this cannot know.
  const resize = useCallback(
    (next: number) => onResize(Math.min(max, Math.max(min, Math.round(next)))),
    [onResize, min, max],
  );

  const onPointerDown = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      origin.current = { point: axis === "x" ? event.clientX : event.clientY, size };
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
    const onUp = () => setDragging(false);
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
  }, [dragging, resize, axis]);

  useKeyBindings([
    {
      keys: axis === "x" ? "right" : "up",
      priority: 250,
      when: () => focused,
      handler: () => resize(size + RESIZE_STEP),
    },
    {
      keys: axis === "x" ? "left" : "down",
      priority: 250,
      when: () => focused,
      handler: () => resize(size - RESIZE_STEP),
    },
    { keys: "home", priority: 250, when: () => focused, handler: () => resize(min) },
    { keys: "end", priority: 250, when: () => focused, handler: () => resize(max) },
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
