/**
 * How tall the agent drawer is allowed to be.
 *
 * Two limits, and they are different kinds of thing. {@link
 * AGENT_DRAWER_HEIGHT_BOUNDS} is what a *stored* height may say — a hand-edited
 * row, or one written by a build with a different maximum. This adds the limit
 * that only exists at render: the window. A drawer restored at 700px into a
 * window that is 600px tall is not a tall drawer, it is a window with no mail
 * in it, and the height it was dragged to on a large display must not be able
 * to produce that on a small one.
 *
 * Both are applied in the same function so there is exactly one answer to "how
 * tall is it", whether the number came from a drag, from the arrow keys, or
 * from the store on the last launch.
 */

import { AGENT_DRAWER_HEIGHT_BOUNDS } from "@/lib/prefs";

/** The height it stood at before it could be dragged: `h-80`. */
export const DEFAULT_AGENT_DRAWER_HEIGHT = 320;

/**
 * What the drawer leaves for everything else: the title bar, the dock's pill
 * strip, the status bar, and enough of the mail list to still be a mail list.
 * A drawer that fills the window is a modal, and the whole design of this unit
 * is that it is not one.
 */
export const RESERVED_WINDOW_CHROME = 180;

/**
 * A height that fits both the bounds and this window.
 *
 * The floor wins ties: in a window too short for even the minimum, a drawer at
 * the minimum with the list squeezed is a better answer than a two-pixel
 * drawer. Anything that is not a number lands on the default, because "the
 * stored value is nonsense" and "there is no stored value" should look the
 * same on screen.
 */
export function clampDrawerHeight(height: number, viewportHeight: number): number {
  const { min, max } = AGENT_DRAWER_HEIGHT_BOUNDS;
  // A viewport of zero is "not measured yet" — a server render, a test — and
  // the honest answer there is the bounds alone rather than a drawer clamped
  // to nothing.
  const ceiling =
    viewportHeight > 0
      ? Math.max(min, Math.min(max, viewportHeight - RESERVED_WINDOW_CHROME))
      : max;
  if (!Number.isFinite(height)) return Math.min(ceiling, DEFAULT_AGENT_DRAWER_HEIGHT);
  return Math.min(ceiling, Math.max(min, Math.round(height)));
}
