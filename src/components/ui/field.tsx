import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/utils";
import { Label } from "./label";

/**
 * Form rows.
 *
 * This is shadcn's `field` primitive with everything this app does not have a
 * use for removed (fieldsets, legends, responsive containers, the `errors[]`
 * array shape borrowed from react-hook-form) and one orientation added.
 *
 * That orientation, `row`, is the house layout: a fixed 4rem gutter of quiet
 * label on the left and the control filling the rest. It is a grid rather than
 * a flex row because the gutter has to be the same width on every row of the
 * form — with `flex` the labels line up only until one of them is called
 * "Calendar", and a ragged left edge is exactly the thing that makes a dense
 * form look thrown together.
 */

const fieldVariants = cva("group/field min-w-0 data-[invalid=true]:text-danger", {
  variants: {
    orientation: {
      vertical: "flex flex-col gap-1",
      horizontal: "flex flex-row items-center gap-2",
      row: [
        "grid grid-cols-[4rem_minmax(0,1fr)] items-start gap-x-2 gap-y-1",
        // Labels sit on the first line of a 28px control rather than its top
        // edge; without this every label rides two pixels high.
        "[&>[data-slot=field-label]]:pt-[0.5rem]",
        // Anything after the control spans the full width under it, so a hint
        // or an error is not squeezed into the gutter.
        "[&>[data-slot=field-description]]:col-start-2 [&>[data-slot=field-error]]:col-start-2",
      ].join(" "),
    },
  },
  defaultVariants: { orientation: "vertical" },
});

export function Field({
  className,
  orientation = "vertical",
  ...props
}: ComponentProps<"div"> & VariantProps<typeof fieldVariants>) {
  return (
    <div
      role="group"
      data-slot="field"
      data-orientation={orientation}
      className={cn(fieldVariants({ orientation }), className)}
      {...props}
    />
  );
}

export function FieldGroup({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      data-slot="field-group"
      className={cn("flex w-full min-w-0 flex-col gap-2.5", className)}
      {...props}
    />
  );
}

export function FieldLabel({ className, ...props }: ComponentProps<typeof Label>) {
  return <Label data-slot="field-label" className={cn("gap-1", className)} {...props} />;
}

/** A stack of controls inside one field — the two ends of a date range, say. */
export function FieldContent({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      data-slot="field-content"
      className={cn("flex min-w-0 flex-col gap-1.5", className)}
      {...props}
    />
  );
}

export function FieldDescription({ className, ...props }: ComponentProps<"p">) {
  return (
    <p
      data-slot="field-description"
      className={cn("text-micro leading-snug text-faint-foreground", className)}
      {...props}
    />
  );
}

export function FieldError({ className, children, ...props }: ComponentProps<"p">) {
  if (!children) return null;
  return (
    <p
      role="alert"
      data-slot="field-error"
      className={cn("text-micro leading-snug text-danger", className)}
      {...props}
    >
      {children}
    </p>
  );
}
