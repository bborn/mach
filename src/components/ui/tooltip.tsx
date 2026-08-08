import { Tooltip as TooltipPrimitive } from "@base-ui/react/tooltip";
import { cn } from "@/lib/utils";
import { POPUP_MARKER } from "./popup";

/**
 * A tooltip: inverted ink, one line, no arrow.
 *
 * The app leans on the `title` attribute in most places and should keep doing
 * so — the OS tooltip costs nothing and never gets the z-index wrong. This is
 * for the handful of controls that are icon-only *and* inside an overlay, where
 * the native one is both slow and clipped.
 *
 * Tooltips do not register with `lib/popups`: they never take focus, so Escape
 * has nothing to do with them.
 */

export function TooltipProvider({ delay = 400, ...props }: TooltipPrimitive.Provider.Props) {
  return <TooltipPrimitive.Provider data-slot="tooltip-provider" delay={delay} {...props} />;
}

export function Tooltip(props: TooltipPrimitive.Root.Props) {
  return <TooltipPrimitive.Root data-slot="tooltip" {...props} />;
}

export function TooltipTrigger(props: TooltipPrimitive.Trigger.Props) {
  return <TooltipPrimitive.Trigger data-slot="tooltip-trigger" {...props} />;
}

export function TooltipContent({
  className,
  side = "top",
  sideOffset = 5,
  align = "center",
  ...props
}: TooltipPrimitive.Popup.Props &
  Pick<TooltipPrimitive.Positioner.Props, "align" | "side" | "sideOffset">) {
  return (
    <TooltipPrimitive.Portal>
      <TooltipPrimitive.Positioner
        side={side}
        sideOffset={sideOffset}
        align={align}
        className="isolate z-50"
        {...POPUP_MARKER}
      >
        <TooltipPrimitive.Popup
          data-slot="tooltip-content"
          className={cn(
            "z-50 max-w-64 rounded-[3px] bg-foreground px-1.5 py-1 text-micro leading-snug text-background",
            "origin-(--transform-origin) transition-[opacity,scale] duration-100 ease-out",
            "data-[starting-style]:scale-[0.98] data-[starting-style]:opacity-0",
            "data-[ending-style]:scale-[0.98] data-[ending-style]:opacity-0",
            className,
          )}
          {...props}
        />
      </TooltipPrimitive.Positioner>
    </TooltipPrimitive.Portal>
  );
}
