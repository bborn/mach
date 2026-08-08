import { useEffect, useId } from "react";
import { setPopupOpen } from "@/lib/popups";

/**
 * The one floating surface in the app.
 *
 * Select menus, popovers and the date grid are all the same object seen from
 * different angles: a raised panel a few pixels off its anchor. Sharing the
 * string is what stops them drifting into three slightly different greys, and
 * it deliberately reuses the modal's shadow — the app has exactly one, and it
 * only ever says "this floats".
 */
export const POPUP_SURFACE = [
  "z-50 overflow-hidden rounded-[var(--radius)] border border-border-strong bg-surface",
  "shadow-[0_16px_48px_-12px_rgba(0,0,0,0.35)] outline-none",
  // Base UI drives these attributes across the open/close transition. 100ms of
  // opacity and a 2% scale is the whole animation budget: enough to read as a
  // panel arriving, not enough to be noticed twice.
  "origin-(--transform-origin) transition-[opacity,scale] duration-100 ease-out",
  "data-[starting-style]:scale-[0.98] data-[starting-style]:opacity-0",
  "data-[ending-style]:scale-[0.98] data-[ending-style]:opacity-0",
].join(" ");

/**
 * The attribute that tells the modal focus trap "this is mine".
 *
 * Base UI portals its popups to `document.body`, which is outside the dialog
 * panel — so without a marker `Overlay`'s trap sees focus land in a select menu,
 * calls it an escape, and yanks focus back, making the menu unusable. Put this
 * on the positioner of anything that portals.
 *
 * @see components/ui/dialog.tsx
 */
export const POPUP_MARKER = { "data-mach-popup": "" } as const;

/**
 * Tell the keymap that a popup is on screen for as long as it is.
 *
 * Returns the id so a caller can pass it around if it ever needs to; almost
 * nothing does.
 *
 * @see lib/popups.ts for why the keymap needs to know.
 */
export function usePopupRegistration(open: boolean | undefined): string {
  const id = useId();
  useEffect(() => {
    setPopupOpen(id, open === true);
  }, [id, open]);
  // Unmounting while open — a route change, a dialog closing under it — must
  // not leave the counter stuck above zero and Escape permanently declined.
  useEffect(() => () => setPopupOpen(id, false), [id]);
  return id;
}
