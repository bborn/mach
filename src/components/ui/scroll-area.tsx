import { forwardRef, type HTMLAttributes } from "react";
import { cn } from "@/lib/utils";

/**
 * A scroll container that behaves like an app pane rather than a document:
 * it owns its overscroll, reserves gutter space so content never shifts when
 * the scrollbar appears, and never scrolls horizontally by accident.
 */
export const ScrollArea = forwardRef<HTMLDivElement, HTMLAttributes<HTMLDivElement>>(
  function ScrollArea({ className, style, ...props }, ref) {
    return (
      <div
        ref={ref}
        className={cn("min-h-0 flex-1 overflow-y-auto overflow-x-hidden", className)}
        style={{ overscrollBehavior: "contain", scrollbarGutter: "stable", ...style }}
        {...props}
      />
    );
  },
);
