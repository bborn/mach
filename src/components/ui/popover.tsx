import { Popover as PopoverPrimitive } from "@base-ui/react/popover";
import { useState } from "react";
import { cn } from "@/lib/utils";
import { POPUP_MARKER, POPUP_SURFACE, usePopupRegistration } from "./popup";

/**
 * A popover on the app's one floating surface.
 *
 * Deliberately **not** modal: everything that opens one here is already inside
 * a dialog that owns the focus trap, and two traps arguing about where focus
 * goes on close is how a keyboard user ends up back at the top of the document.
 */

export function Popover({ open, onOpenChange, ...props }: PopoverPrimitive.Root.Props) {
  const [uncontrolled, setUncontrolled] = useState(false);
  usePopupRegistration(open ?? uncontrolled);

  return (
    <PopoverPrimitive.Root
      data-slot="popover"
      open={open}
      onOpenChange={(next, details) => {
        setUncontrolled(next);
        onOpenChange?.(next, details);
      }}
      {...props}
    />
  );
}

export function PopoverTrigger(props: PopoverPrimitive.Trigger.Props) {
  return <PopoverPrimitive.Trigger data-slot="popover-trigger" {...props} />;
}

export function PopoverContent({
  className,
  align = "start",
  alignOffset = 0,
  side = "bottom",
  sideOffset = 4,
  anchor,
  ...props
}: PopoverPrimitive.Popup.Props &
  Pick<
    PopoverPrimitive.Positioner.Props,
    "align" | "alignOffset" | "side" | "sideOffset" | "anchor"
  >) {
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Positioner
        align={align}
        alignOffset={alignOffset}
        side={side}
        sideOffset={sideOffset}
        anchor={anchor}
        className="isolate z-50"
        {...POPUP_MARKER}
      >
        <PopoverPrimitive.Popup
          data-slot="popover-content"
          className={cn(POPUP_SURFACE, "text-body text-foreground", className)}
          {...props}
        />
      </PopoverPrimitive.Positioner>
    </PopoverPrimitive.Portal>
  );
}

export function PopoverTitle({ className, ...props }: PopoverPrimitive.Title.Props) {
  return (
    <PopoverPrimitive.Title
      data-slot="popover-title"
      className={cn("text-micro font-medium text-muted-foreground", className)}
      {...props}
    />
  );
}

export function PopoverDescription({ className, ...props }: PopoverPrimitive.Description.Props) {
  return (
    <PopoverPrimitive.Description
      data-slot="popover-description"
      className={cn("text-micro text-faint-foreground", className)}
      {...props}
    />
  );
}
