import type { ComponentProps } from "react";
import { cn } from "@/lib/utils";

/**
 * A form label at the app's smallest step.
 *
 * `text-micro` rather than shadcn's `text-sm`: labels in this app are the
 * quietest thing on the row, not a second voice competing with the value.
 */
export function Label({ className, ...props }: ComponentProps<"label">) {
  return (
    <label
      data-slot="label"
      className={cn(
        "flex items-center gap-1 text-micro leading-none text-faint-foreground select-none",
        "group-data-[disabled=true]/field:opacity-50 peer-disabled:opacity-50",
        className,
      )}
      {...props}
    />
  );
}
