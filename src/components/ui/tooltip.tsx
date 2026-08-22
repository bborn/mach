import { type ReactElement } from "react";
import { Tooltip as TooltipPrimitive } from "@base-ui/react/tooltip";
import { type ModKey } from "@/lib/keymap";
import { cn } from "@/lib/utils";
import { Kbd } from "./kbd";
import { POPUP_MARKER } from "./popup";

/**
 * A tooltip: inverted ink, one line, no arrow.
 *
 * Native `title` is still the right overflow for a truncated string that has
 * no key. Anything that *does* have a key uses `ShortcutTooltip`, so the
 * binding shows as a chip rather than as a parenthetical in the OS balloon.
 * The trigger waits 600ms — a hold, not a hover — so a sweep across the
 * title bar does not light every control on the way.
 *
 * Tooltips do not register with `lib/popups`: they never take focus, so Escape
 * has nothing to do with them.
 */

export function TooltipProvider({ delay = 600, ...props }: TooltipPrimitive.Provider.Props) {
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

/**
 * The payload of a shortcut tooltip: the name, then the keys as chips.
 *
 * A sequence (`g i`) becomes two chips with "then" between them, which is
 * how the shortcut sheet already prints it and how Linear prints `G then I`.
 * Two *spellings* of the same action (`/` and `mod+f`) are two chips with
 * no "then" — they are alternatives, not a chord.
 */
export function ShortcutHint({
  label,
  keys,
  mod,
}: {
  label: string;
  keys?: string | readonly string[];
  /** Passed through to {@link Kbd}; only a test has reason to set it. */
  mod?: ModKey;
}) {
  const bindings = keys === undefined ? [] : typeof keys === "string" ? [keys] : [...keys];
  return (
    <span className="inline-flex items-center gap-1.5">
      <span>{label}</span>
      {bindings.map((binding) => (
        <ShortcutKeys key={binding} keys={binding} mod={mod} />
      ))}
    </span>
  );
}

function ShortcutKeys({ keys, mod }: { keys: string; mod?: ModKey }) {
  const tokens = keys.trim().split(/\s+/).filter(Boolean);
  return (
    <span className="inline-flex items-center gap-0.5">
      {tokens.map((token, i) => (
        <span key={`${i}-${token}`} className="inline-flex items-center gap-0.5">
          {i > 0 && <span className="px-0.5 opacity-70">then</span>}
          <Kbd
            keys={token}
            mod={mod}
            className="h-4 min-w-4 border-background/30 bg-background/15 px-1 text-background"
          />
        </span>
      ))}
    </span>
  );
}

/**
 * Hold on a control, see its name and its key.
 *
 * The child is the control — a `<button>` — passed through as the trigger so
 * we do not wrap a button in a button. Native `title` on the same node would
 * fire a second balloon a beat later; do not set one.
 */
export function ShortcutTooltip({
  label,
  keys,
  children,
  side = "bottom",
}: {
  label: string;
  keys?: string | readonly string[];
  children: ReactElement;
  side?: "top" | "bottom" | "left" | "right";
}) {
  return (
    <Tooltip>
      <TooltipTrigger render={children} delay={600} />
      <TooltipContent side={side} sideOffset={6} className="max-w-none whitespace-nowrap">
        <ShortcutHint label={label} keys={keys} />
      </TooltipContent>
    </Tooltip>
  );
}
