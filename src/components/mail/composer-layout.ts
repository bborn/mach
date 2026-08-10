/**
 * How much room the composer gets, and where it is drawn.
 *
 * Two questions with one thing in common: the answer has to be the same
 * whether it came from a drag, from the arrow keys, from ⇧⌘O, or from the
 * store on the last launch. So both live here, as functions of their inputs
 * and nothing else — `ComposerDock` holds the state, this decides what it
 * means, and the tests can ask every question without a DOM.
 *
 * # Height
 *
 * The number being sized is the *editor*, not the whole composer: the fields
 * above it and the legend below it are fixed furniture, so a body height is
 * what a person is actually choosing when they drag the top edge, and the
 * gesture is 1:1 with it. {@link COMPOSER_HEIGHT_BOUNDS} is what a *stored*
 * value may say; this adds the limit that only exists at render — the window,
 * which the store cannot know and which changes when the window is resized or
 * moved to another display.
 *
 * # Placement
 *
 * A reply is docked under the conversation it answers, because it is about
 * that conversation. A new message floats over the window, because it is
 * about nothing on screen. Popping out moves a reply to the second of those
 * without changing anything about the draft — which is why it is a view flag
 * keyed by draft id here, and not a second draft anywhere.
 */

import { COMPOSER_HEIGHT_BOUNDS } from "@/lib/prefs";
import type { DraftKind } from "@/lib/compose";
import type { ComposerPresentation } from "./Composer";

/** The height the body stood at before it could be dragged, plus some air. */
export const DEFAULT_COMPOSER_HEIGHT = 200;

/**
 * What the docked composer leaves for everything else: the title bar and the
 * status bar, its own fields, toolbar and legend, and enough of the
 * conversation above it to still be a conversation. A reply that fills the
 * reading pane has hidden the message it is answering, which is the one thing
 * the docked composer exists to avoid — at 320 the ceiling did exactly that,
 * leaving the sender line and nothing under it.
 */
export const RESERVED_READING_HEIGHT = 420;

/**
 * What the popped-out composer leaves: the overlay's own margins, and the same
 * fields and legend, which do not get smaller for being over the window.
 */
export const POPPED_WINDOW_CHROME = 260;

/**
 * A body height that fits both the bounds and this window.
 *
 * The floor wins ties, as it does for the agent drawer: in a window too short
 * for even the minimum, a composer at the minimum with the conversation
 * squeezed is a better answer than a two-pixel one. Anything that is not a
 * number lands on the default, because "the stored value is nonsense" and
 * "there is no stored value" should look the same on screen.
 */
export function clampComposerHeight(height: number, viewportHeight: number): number {
  const { min, max } = COMPOSER_HEIGHT_BOUNDS;
  // A viewport of zero is "not measured yet" — a server render, a test — and
  // the honest answer there is the bounds alone.
  const ceiling =
    viewportHeight > 0
      ? Math.max(min, Math.min(max, viewportHeight - RESERVED_READING_HEIGHT))
      : max;
  if (!Number.isFinite(height)) return Math.min(ceiling, DEFAULT_COMPOSER_HEIGHT);
  return Math.min(ceiling, Math.max(min, Math.round(height)));
}

/**
 * How tall the body is once the composer is over the window.
 *
 * Not the dragged height: the point of popping out is that the draft stops
 * negotiating with the conversation and takes the window, so the window is the
 * only input. The stored height is untouched and comes back with the composer.
 */
export function popOutComposerHeight(viewportHeight: number): number {
  const { min, max } = COMPOSER_HEIGHT_BOUNDS;
  if (viewportHeight <= 0) return Math.min(max, DEFAULT_COMPOSER_HEIGHT * 2);
  return Math.max(min, Math.min(max, Math.round(viewportHeight - POPPED_WINDOW_CHROME)));
}

/**
 * Whether this draft has a dock to leave.
 *
 * A new message is already over the window and has no thread to sit under, so
 * "pop out" would name a move it cannot make — and putting it back would dock
 * it under a conversation it has nothing to do with, which is the mistake the
 * overlay exists to prevent.
 */
export function canPopOut(kind: DraftKind): boolean {
  return kind !== "new";
}

/** Where a draft of this kind is drawn, given whether it has been popped out. */
export function composerPlacement(kind: DraftKind, poppedOut: boolean): ComposerPresentation {
  return canPopOut(kind) && !poppedOut ? "dock" : "overlay";
}

/** Whether this draft is currently popped out. */
export function isPoppedOut(popped: readonly string[], id: string): boolean {
  return popped.includes(id);
}

/**
 * The popped-out set with one draft flipped.
 *
 * Ids rather than a single flag, because several composers can be open at
 * once and the strip switches between them: a reply popped out and switched
 * away from is still popped out when it comes back.
 */
export function togglePopOut(popped: readonly string[], id: string): string[] {
  return isPoppedOut(popped, id) ? popped.filter((entry) => entry !== id) : [...popped, id];
}

/** The set with one draft forgotten — a composer that has closed or sent. */
export function forgetPopOut(popped: readonly string[], id: string): string[] {
  return popped.filter((entry) => entry !== id);
}
