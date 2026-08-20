import { useEffect, useLayoutEffect, useRef, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import { useKeyBindings, useKeymap } from "@/hooks/useKeymap";
import { anyPopupOpen } from "@/lib/popups";
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
 * A modal surface with a real focus trap, focus restoration, and the keyboard.
 *
 * Escape closes. The keymap binding is the one that wins while the caret is
 * in a field (capture, `allowInInput`); the React handler is the backup for
 * a key that never reached the window listener. A nested prompt — "this or
 * all events", "email the guests" — outranks this at a higher priority, so
 * the first Escape peels that off and the second closes.
 *
 * A focus trap only decides where the caret is, and the app's bindings never
 * asked: they are window-level, so with preferences open `e` still archived
 * the conversation underneath it. An open overlay claims the keyboard from
 * the registry for as long as it is up. `claimKeyboard` explains what
 * survives a claim.
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
  const keymap = useKeymap();

  /*
   * Layout effect, not effect: the claim has to be in force before anything can
   * be typed at the surface that just opened. React flushes layout effects
   * before it yields to the browser, so there is no frame in which the dialog
   * is on screen and the list behind it still owns `e`.
   */
  useLayoutEffect(() => {
    if (!open) return;
    return keymap.claimKeyboard();
  }, [open, keymap]);

  useKeyBindings([
    {
      keys: "escape",
      priority: 105,
      allowInInput: true,
      when: () => open && !anyPopupOpen(),
      handler: () => onClose(),
    },
  ]);

  const onEscape = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Escape") return;
    if (anyPopupOpen()) return;
    event.preventDefault();
    onClose();
  };

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
      onKeyDown={onEscape}
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
