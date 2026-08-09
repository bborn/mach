import { useEffect, useRef, type ReactNode, type RefObject } from "react";
import { cn } from "@/lib/utils";

interface OverlayProps {
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  /** Distance from the top of the viewport. Palettes sit high, not centred. */
  align?: "top" | "center";
  /**
   * Fill the window instead of floating a panel in it.
   *
   * For the one surface that is not a question but a place — preferences. A
   * settings window that has to scroll to show its sixth control is a settings
   * window nobody reads, and the way out of that is width, not a taller modal.
   * Everything else about the overlay is unchanged, which is the point of it
   * being a flag rather than a second component: the focus trap, the popup
   * exception and the restore are the parts that are hard to get right.
   */
  fullScreen?: boolean;
  /**
   * Where focus should land when this opens. Falls back to the first field.
   *
   * A surface with its own navigation wants focus at the top of that
   * navigation, not in the middle of whatever section happens to be showing —
   * and the trap's own effect runs after its children's, so a child cannot
   * simply focus something itself.
   */
  initialFocus?: RefObject<HTMLElement | null>;
  labelledBy?: string;
  className?: string;
}

/**
 * A modal surface with a real focus trap and focus restoration.
 *
 * Escape is *not* handled here — it belongs to the keymap registry so that
 * precedence between the palette, an open thread and the shell is decided in
 * one place instead of by whoever bubbles first.
 */
export function Overlay({
  open,
  onClose,
  children,
  align = "top",
  fullScreen = false,
  initialFocus,
  labelledBy,
  className,
}: OverlayProps) {
  const panel = useRef<HTMLDivElement>(null);
  const restoreTo = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!open) return;
    restoreTo.current = document.activeElement as HTMLElement | null;
    const focusable =
      initialFocus?.current ??
      panel.current?.querySelector<HTMLElement>(
        "input, textarea, [tabindex]:not([tabindex='-1'])",
      );
    focusable?.focus();

    /*
     * Trap: focus cannot leave the panel while it is open.
     *
     * With one exception, and it is not a loophole. A select menu, a popover, a
     * date grid — anything built on Base UI — renders through a portal on
     * `document.body`, because a popup clipped by its dialog's `overflow` is
     * not a popup. Those elements are outside `panel` in the DOM and inside it
     * in every sense that matters, so without this check the trap sees focus
     * land in an open menu, calls it an escape, and drags focus back to the
     * first field — which makes every select in every dialog unusable by
     * keyboard *and* by mouse.
     *
     * `components/ui/popup.ts` puts the marker on; nothing else may.
     */
    const onFocusIn = (event: FocusEvent) => {
      const node = event.target as HTMLElement | null;
      if (!panel.current || panel.current.contains(node)) return;
      if (node?.closest?.("[data-mach-popup]")) return;
      event.stopPropagation();
      focusable?.focus();
    };
    document.addEventListener("focusin", onFocusIn, true);
    return () => {
      document.removeEventListener("focusin", onFocusIn, true);
      restoreTo.current?.focus?.();
    };
  }, [open]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex justify-center bg-background/70"
      style={{ paddingTop: align === "top" ? "14vh" : 0, alignItems: align === "center" ? "center" : undefined }}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelledBy}
        className={cn(
          "flex flex-col overflow-hidden bg-surface",
          fullScreen
            ? "h-full w-full"
            : [
                "max-h-[68vh] w-full max-w-[38rem]",
                "rounded-[var(--radius)] border border-border-strong",
                // The one shadow in the app: it says "this floats", nothing more.
                "shadow-[0_16px_48px_-12px_rgba(0,0,0,0.35)]",
              ].join(" "),
          className,
        )}
      >
        {children}
      </div>
    </div>
  );
}
