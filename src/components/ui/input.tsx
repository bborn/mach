import { forwardRef, type InputHTMLAttributes } from "react";
import { cn } from "@/lib/utils";

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(
  function Input({ className, ...props }, ref) {
    return (
      <input
        ref={ref}
        className={cn(
          "h-7 w-full rounded-[var(--radius)] border border-border bg-background px-2",
          "text-body text-foreground placeholder:text-faint-foreground",
          "focus:border-accent focus:outline-none",
          className,
        )}
        {...props}
      />
    );
  },
);

/**
 * The palette's field: no chrome at all, because the dialog frame already is
 * the chrome. Separate from `Input` so the two never drift.
 */
export const BareInput = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(
  function BareInput({ className, ...props }, ref) {
    return (
      <input
        ref={ref}
        autoComplete="off"
        spellCheck={false}
        className={cn(
          "w-full bg-transparent text-reading text-foreground",
          "placeholder:text-faint-foreground focus:outline-none",
          className,
        )}
        {...props}
      />
    );
  },
);
