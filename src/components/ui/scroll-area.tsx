import { forwardRef, type HTMLAttributes } from "react";
import { cn } from "@/lib/utils";

export interface ScrollAreaProps extends HTMLAttributes<HTMLDivElement> {
  /**
   * Keep the classic scrollbar track even when content is short.
   *
   * `scrollbar-gutter: stable` is already on every pane. WebKit drops it once
   * `::-webkit-scrollbar` is styled, so the reading pane still jumped 10px
   * sideways when a tall message made the bar appear. The thread list is
   * usually already overflowing; this is for the pane that is not.
   */
  lockGutter?: boolean;
}

/**
 * A scroll container that behaves like an app pane rather than a document:
 * it owns its overscroll, reserves gutter space so content never shifts when
 * the scrollbar appears, and never scrolls horizontally by accident.
 */
export const ScrollArea = forwardRef<HTMLDivElement, ScrollAreaProps>(
  function ScrollArea({ className, style, lockGutter, ...props }, ref) {
    return (
      <div
        ref={ref}
        className={cn("min-h-0 flex-1 overflow-y-auto overflow-x-hidden", className)}
        style={{
          overscrollBehavior: "contain",
          scrollbarGutter: "stable",
          ...(lockGutter ? { overflowY: "scroll" } : null),
          ...style,
        }}
        {...props}
      />
    );
  },
);
