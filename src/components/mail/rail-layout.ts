/**
 * How wide the mail rail gets.
 *
 * One question, and the answer has to be the same whether it came from a drag,
 * from ⌘⌥← / ⌘⌥→, from a double-click on the divider, or from the session blob
 * on the last launch. So it lives here as a function of its input and nothing
 * else — `AccountRail` holds the state, this decides what it means, and the
 * tests can ask every question without a DOM. Same arrangement as
 * `composer-layout.ts`, for the same reason.
 *
 * # Why there is no window argument
 *
 * `clampComposerHeight` takes a viewport, because a composer can be dragged
 * taller than the window it is restored into and the store cannot know that.
 * The rail cannot: the window refuses to go below `minWidth: 960`, and the
 * rail's maximum plus the conversation list's minimum is 600 of those pixels,
 * so every width this function can return fits every window that can exist.
 * A viewport argument here would be a parameter with one legal value.
 */

import { RAIL_WIDTH_BOUNDS } from "@/lib/prefs";

/**
 * The width the rail stood at before it could be dragged — `--rail-width`, 13
 * rem — and what a double-click on the divider goes back to.
 */
export const DEFAULT_RAIL_WIDTH = 208;

/**
 * A width that fits the bounds.
 *
 * Anything that is not a number lands on the default, because "the stored value
 * is nonsense" and "there is no stored value" should look the same on screen —
 * the rule `clampComposerHeight` applies to a height.
 */
export function clampRailWidth(width: number): number {
  const { min, max } = RAIL_WIDTH_BOUNDS;
  if (!Number.isFinite(width)) return DEFAULT_RAIL_WIDTH;
  return Math.min(max, Math.max(min, Math.round(width)));
}
