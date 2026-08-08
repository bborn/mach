import { Checkbox as CheckboxPrimitive } from "@base-ui/react/checkbox";
import { Check } from "lucide-react";
import { cn } from "@/lib/utils";

/**
 * A 12px checkbox — the same square the calendar sidebar has always drawn.
 *
 * Stock shadcn is 16px with a 3px focus ring; at Mach's density that box is
 * bigger than the label beside it. The tick is drawn rather than glyphed so it
 * stays crisp at this size, and the whole thing is a `<button role="checkbox">`
 * under the hood, which is how it escapes the native control's platform look
 * without giving up its keyboard or screen-reader behaviour.
 */
export function Checkbox({ className, ...props }: CheckboxPrimitive.Root.Props) {
  return (
    <CheckboxPrimitive.Root
      data-slot="checkbox"
      className={cn(
        "peer flex size-3 shrink-0 items-center justify-center rounded-[3px] border transition-colors",
        "border-border-strong hover:border-faint-foreground",
        "data-checked:border-accent data-checked:bg-accent data-checked:text-accent-foreground",
        "data-disabled:pointer-events-none data-disabled:opacity-40",
        className,
      )}
      {...props}
    >
      <CheckboxPrimitive.Indicator
        data-slot="checkbox-indicator"
        className="flex items-center justify-center text-current"
      >
        <Check size={9} strokeWidth={3} />
      </CheckboxPrimitive.Indicator>
    </CheckboxPrimitive.Root>
  );
}
