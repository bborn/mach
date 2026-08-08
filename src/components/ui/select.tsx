import { Select as SelectPrimitive } from "@base-ui/react/select";
import { Check, ChevronDown, ChevronUp } from "lucide-react";
import { useState } from "react";
import { cn } from "@/lib/utils";
import { POPUP_MARKER, POPUP_SURFACE, usePopupRegistration } from "./popup";

/**
 * A select, from the shadcn registry on Base UI, wearing Mach's tokens.
 *
 * Nothing here is stock. The stock trigger is 32px tall, `text-sm`, and rings
 * three pixels of `--ring` on focus; this one is the same 28px box as `Input`
 * with the same one-pixel accent border, because a form whose fields are three
 * different heights reads as three different apps.
 *
 * Two things worth knowing before using it:
 *
 *  - **Pass `items` to `Select`.** Base UI renders `SelectValue` from that map,
 *    so the trigger can show a label before the popup has ever been mounted.
 *    `SelectItem` children are then free to be richer than the trigger text —
 *    a colour swatch and an account address, say.
 *  - **It is not modal.** Base UI's default locks page scroll and blocks
 *    outside pointers. Every select in this app already lives inside a modal
 *    that does both, and nesting the two fights over focus on close.
 */

export function Select({
  items,
  onOpenChange,
  ...props
}: SelectPrimitive.Root.Props<string>) {
  // Base UI is uncontrolled here, so the open state has to be observed rather
  // than owned — the keymap needs to know a menu is up (see lib/popups.ts).
  const [open, setOpen] = useState(false);
  usePopupRegistration(open);

  return (
    <SelectPrimitive.Root
      items={items}
      modal={false}
      onOpenChange={(next, details) => {
        setOpen(next);
        onOpenChange?.(next, details);
      }}
      {...props}
    />
  );
}

export function SelectGroup({ className, ...props }: SelectPrimitive.Group.Props) {
  return (
    <SelectPrimitive.Group
      data-slot="select-group"
      className={cn("scroll-my-1 p-1", className)}
      {...props}
    />
  );
}

export function SelectValue({ className, ...props }: SelectPrimitive.Value.Props) {
  return (
    <SelectPrimitive.Value
      data-slot="select-value"
      className={cn(
        "min-w-0 flex-1 truncate text-left data-[placeholder]:text-faint-foreground",
        className,
      )}
      {...props}
    />
  );
}

export function SelectTrigger({
  className,
  children,
  ...props
}: SelectPrimitive.Trigger.Props) {
  return (
    <SelectPrimitive.Trigger
      data-slot="select-trigger"
      className={cn(
        "flex h-7 w-full items-center gap-1.5 rounded-[var(--radius)] border border-border bg-background",
        "px-2 text-body text-foreground select-none transition-colors",
        "hover:border-border-strong data-[popup-open]:border-accent",
        "disabled:pointer-events-none disabled:opacity-40",
        className,
      )}
      {...props}
    >
      {children}
      <SelectPrimitive.Icon
        render={
          <ChevronDown
            size={13}
            strokeWidth={1.75}
            className="pointer-events-none shrink-0 text-faint-foreground"
          />
        }
      />
    </SelectPrimitive.Trigger>
  );
}

export function SelectContent({
  className,
  children,
  sideOffset = 4,
  ...props
}: SelectPrimitive.Popup.Props &
  Pick<SelectPrimitive.Positioner.Props, "align" | "alignOffset" | "side" | "sideOffset">) {
  return (
    <SelectPrimitive.Portal>
      <SelectPrimitive.Positioner
        sideOffset={sideOffset}
        alignItemWithTrigger={false}
        className="isolate z-50"
        {...POPUP_MARKER}
      >
        <SelectPrimitive.Popup
          data-slot="select-content"
          className={cn(
            POPUP_SURFACE,
            "max-h-(--available-height) min-w-(--anchor-width) py-1",
            className,
          )}
          {...props}
        >
          <SelectScrollArrow direction="up" />
          <SelectPrimitive.List className="max-h-(--available-height) overflow-x-hidden overflow-y-auto">
            {children}
          </SelectPrimitive.List>
          <SelectScrollArrow direction="down" />
        </SelectPrimitive.Popup>
      </SelectPrimitive.Positioner>
    </SelectPrimitive.Portal>
  );
}

export function SelectLabel({ className, ...props }: SelectPrimitive.GroupLabel.Props) {
  return (
    <SelectPrimitive.GroupLabel
      data-slot="select-label"
      className={cn("px-2 py-1 text-micro text-faint-foreground", className)}
      {...props}
    />
  );
}

export function SelectItem({ className, children, ...props }: SelectPrimitive.Item.Props) {
  return (
    <SelectPrimitive.Item
      data-slot="select-item"
      className={cn(
        "relative flex min-h-7 cursor-default items-center gap-1.5 px-2 py-1 pr-7",
        "text-body text-foreground outline-none select-none",
        // `data-highlighted` is keyboard *and* pointer focus in Base UI, which
        // is why this is the row-hover step rather than the accent: a menu that
        // paints a saturated bar under the pointer feels like a website.
        "data-highlighted:bg-row-hover data-disabled:pointer-events-none data-disabled:opacity-40",
        className,
      )}
      {...props}
    >
      <SelectPrimitive.ItemText className="min-w-0 flex-1 truncate">
        {children}
      </SelectPrimitive.ItemText>
      <SelectPrimitive.ItemIndicator
        render={<span className="pointer-events-none absolute right-2 flex items-center" />}
      >
        <Check size={12} strokeWidth={2} className="text-accent" />
      </SelectPrimitive.ItemIndicator>
    </SelectPrimitive.Item>
  );
}

export function SelectSeparator({ className, ...props }: SelectPrimitive.Separator.Props) {
  return (
    <SelectPrimitive.Separator
      data-slot="select-separator"
      className={cn("pointer-events-none my-1 h-px bg-border", className)}
      {...props}
    />
  );
}

function SelectScrollArrow({ direction }: { direction: "up" | "down" }) {
  const Arrow = direction === "up" ? SelectPrimitive.ScrollUpArrow : SelectPrimitive.ScrollDownArrow;
  const Icon = direction === "up" ? ChevronUp : ChevronDown;
  return (
    <Arrow
      data-slot={`select-scroll-${direction}`}
      className="z-10 flex w-full cursor-default items-center justify-center bg-surface py-0.5 text-faint-foreground"
    >
      <Icon size={12} strokeWidth={1.75} />
    </Arrow>
  );
}
