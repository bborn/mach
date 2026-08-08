import { useCallback, useLayoutEffect, useRef, type ComponentProps, type Ref } from "react";
import { cn } from "@/lib/utils";

export interface TextareaProps extends ComponentProps<"textarea"> {
  ref?: Ref<HTMLTextAreaElement>;
  /**
   * Grow with the content instead of scrolling, up to `maxRows`.
   *
   * Done by measuring rather than with `field-sizing: content`, which is the
   * one-line CSS answer and is not implemented in the WebKit this app actually
   * renders in.
   */
  autoSize?: boolean;
  maxRows?: number;
}

/**
 * The app's multi-line field.
 *
 * The visible difference from a bare `<textarea>` is the missing resize grip in
 * the bottom-right corner. That grip is the single loudest "this is a web form"
 * signal in a desktop app: it is a hairline of diagonal lines drawn by the
 * platform, at a size and colour no design system chooses, and it appears in
 * every screenshot. Height is the component's business, not the user's.
 */
export function Textarea({
  className,
  ref,
  autoSize,
  maxRows = 10,
  rows = 3,
  onChange,
  value,
  ...props
}: TextareaProps) {
  const own = useRef<HTMLTextAreaElement | null>(null);

  const attach = useCallback(
    (node: HTMLTextAreaElement | null) => {
      own.current = node;
      if (typeof ref === "function") ref(node);
      else if (ref) ref.current = node;
    },
    [ref],
  );

  const measure = useCallback(() => {
    const node = own.current;
    if (!node || !autoSize) return;
    const styles = window.getComputedStyle(node);
    const line = Number.parseFloat(styles.lineHeight) || 18;
    const chrome =
      Number.parseFloat(styles.paddingTop) +
      Number.parseFloat(styles.paddingBottom) +
      Number.parseFloat(styles.borderTopWidth) +
      Number.parseFloat(styles.borderBottomWidth);
    node.style.height = "auto";
    const wanted = Math.min(node.scrollHeight, Math.round(line * maxRows + chrome));
    node.style.height = `${Math.max(wanted, Math.round(line * rows + chrome))}px`;
    node.style.overflowY = node.scrollHeight > wanted ? "auto" : "hidden";
  }, [autoSize, maxRows, rows]);

  useLayoutEffect(measure, [measure, value]);

  return (
    <textarea
      ref={attach}
      data-slot="textarea"
      rows={rows}
      value={value}
      onChange={(event) => {
        onChange?.(event);
        measure();
      }}
      className={cn(
        "w-full resize-none rounded-[var(--radius)] border border-border bg-background px-2 py-1",
        "text-body leading-snug text-foreground placeholder:text-faint-foreground",
        "transition-colors hover:border-border-strong focus:border-accent focus:outline-none",
        "disabled:pointer-events-none disabled:opacity-40",
        "aria-invalid:border-danger",
        className,
      )}
      {...props}
    />
  );
}
