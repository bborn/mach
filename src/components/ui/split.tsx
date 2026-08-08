import { useCallback, useEffect, useRef, useState } from "react";
import { cn } from "@/lib/utils";

interface ResizerProps {
  /** Current width of the pane to the left of this handle, in pixels. */
  width: number;
  onResize: (width: number) => void;
  className?: string;
}

/**
 * A one-pixel divider that happens to be draggable. The hit area is eight
 * pixels wide and invisible; the line itself never thickens, because a
 * thickening divider is the kind of motion that makes an app feel like a toy.
 */
export function Resizer({ width, onResize, className }: ResizerProps) {
  const [dragging, setDragging] = useState(false);
  const origin = useRef({ x: 0, width: 0 });

  const onPointerDown = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      origin.current = { x: event.clientX, width };
      setDragging(true);
      event.currentTarget.setPointerCapture(event.pointerId);
    },
    [width],
  );

  useEffect(() => {
    if (!dragging) return;
    const onMove = (event: PointerEvent) => {
      onResize(origin.current.width + (event.clientX - origin.current.x));
    };
    const onUp = () => setDragging(false);
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
  }, [dragging, onResize]);

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      onPointerDown={onPointerDown}
      className={cn(
        "relative z-10 -ml-1 w-2 shrink-0 cursor-col-resize",
        "after:absolute after:inset-y-0 after:left-1 after:w-px after:bg-transparent",
        dragging && "after:bg-accent",
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
