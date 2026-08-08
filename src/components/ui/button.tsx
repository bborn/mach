import { forwardRef, type ButtonHTMLAttributes } from "react";
import { cn } from "@/lib/utils";

type Variant = "default" | "ghost" | "subtle" | "danger";
type Size = "sm" | "md" | "icon";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
}

/**
 * Flat by design. No shadow, no gradient, one radius step. The only filled
 * button in the app is the accent one, and it appears at most once per screen.
 */
const VARIANTS: Record<Variant, string> = {
  default:
    "bg-accent text-accent-foreground hover:brightness-110 active:brightness-95 border border-transparent",
  ghost:
    "text-muted-foreground hover:bg-row-hover hover:text-foreground border border-transparent",
  subtle:
    "bg-surface-raised text-foreground border border-border hover:border-border-strong",
  danger: "text-danger hover:bg-row-hover border border-transparent",
};

const SIZES: Record<Size, string> = {
  sm: "h-6 px-2 text-micro gap-1",
  md: "h-7 px-2.5 text-list gap-1.5",
  icon: "h-7 w-7 justify-center",
};

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { className, variant = "ghost", size = "md", type = "button", ...props },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      className={cn(
        "inline-flex select-none items-center rounded-[var(--radius)] transition-colors",
        "disabled:pointer-events-none disabled:opacity-40",
        VARIANTS[variant],
        SIZES[size],
        className,
      )}
      {...props}
    />
  );
});
