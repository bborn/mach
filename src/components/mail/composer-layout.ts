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
 * What the docked composer leaves the conversation it is docked under: its own
 * fields, toolbar and legend, and enough of the thread above it to still be a
 * thread. A reply that fills the reading pane has hidden the message it is
 * answering, which is the one thing the docked composer exists to avoid — at
 * 320 the ceiling did exactly that, leaving the sender line and nothing under
 * it.
 *
 * Measured against the reading column, not the window. It was 420 against the
 * window, which is the same number with the title bar (40) and the status bar
 * (24) folded into it — and folding them in is what broke when a third box
 * appeared at the bottom of the window. See {@link clampComposerHeight}.
 */
export const RESERVED_READING_HEIGHT = 356;

/**
 * What the popped-out composer leaves: the overlay's own margins, and the same
 * fields and legend, which do not get smaller for being over the window.
 */
export const POPPED_WINDOW_CHROME = 260;

/**
 * The title bar and the status bar — what the window has that the reading
 * column does not.
 *
 * Only for the case where the column could not be measured at all. `0` means
 * "not measured yet" to {@link clampComposerHeight}, and the answer it gives
 * there is the bounds alone: a 900px composer, which in a column is not a
 * fallback but a worse bug than the one being fixed. So a column that never
 * arrives falls back to the window less this, which is where the number came
 * from in the first place.
 */
export const CHROME_OUTSIDE_COLUMN = 64;

/** The column's height, or the closest thing to it if it could not be read. */
export function readingColumnHeight(column: number, viewportHeight: number): number {
  return column > 0 ? column : Math.max(0, viewportHeight - CHROME_OUTSIDE_COLUMN);
}

/**
 * A body height that fits both the bounds and the column the composer is in.
 *
 * # Two docks in one column need one owner of the height
 *
 * `columnHeight` is the reading column — the pane holding the conversation and
 * the composer under it — and not the window. The distinction is the whole
 * defect this argument was changed to fix.
 *
 * The bottom of the window is a stack: the agent drawer, the agent's pill
 * strip, the status bar. Every one of them is `shrink-0`, so each takes its
 * height off the reading column. The composer sat in that column asking the
 * *window* how much room it had, which is a number nothing subtracts from when
 * a second dock opens. With the agent drawer up at 1440×757 the composer stood
 * 352px tall in a 336px column and hung 87px out of the bottom of it — the
 * footer drawn clear of the pane, over the drawer's content, because no box
 * between the two clips.
 *
 * So: the column owns the height, and everything in it asks the column. The
 * composer also gives way rather than overflow — `COMPOSER_COLUMN` on the
 * docked root, so a measurement that is one frame stale costs a shorter message
 * instead of a detached footer. A dock that sizes itself from the window is
 * only correct while it is the only dock, and it never was.
 *
 * The floor wins ties, as it does for the agent drawer: in a column too short
 * for even the minimum, a composer at the minimum with the conversation
 * squeezed is a better answer than a two-pixel one. Anything that is not a
 * number lands on the default, because "the stored value is nonsense" and
 * "there is no stored value" should look the same on screen.
 */
export function clampComposerHeight(height: number, columnHeight: number): number {
  const { min, max } = COMPOSER_HEIGHT_BOUNDS;
  // A column of zero is "not measured yet" — a server render, a test, the
  // render before the observer has fired — and the honest answer there is the
  // bounds alone.
  const ceiling =
    columnHeight > 0
      ? Math.max(min, Math.min(max, columnHeight - RESERVED_READING_HEIGHT))
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

/* -------------------------------------------------------------------------- */
/* Which box gives up space                                                    */
/* -------------------------------------------------------------------------- */

/**
 * The composer's own column, for a composer inside something that limits it.
 *
 * The overlay's panel is `flex flex-col`, `max-h-[68vh]`, `overflow: hidden`.
 * The composer inside it was a plain block whose height was the sum of its
 * parts, and one of those parts — the editor — is sized from the *window*
 * ({@link popOutComposerHeight} is the window less some chrome), not from the
 * panel. The two numbers were never reconciled, so at 1440×900 the composer
 * asked for 800px inside a 612px panel and the last 188 of it — the legend,
 * the paperclip, discard — were cut off by that `overflow: hidden`. It was in
 * the DOM the whole time, 173px below the panel's bottom edge.
 *
 * A column fixes it by making the constraint reach the editor. `min-h-0` is
 * the half that is easy to leave out: a flex item's automatic minimum size is
 * its content, so without it the box refuses to shrink and overflows instead,
 * which is exactly the shape of the original bug one level down.
 */
export const COMPOSER_COLUMN = "flex min-h-0 flex-col";

/**
 * A row of that column that keeps its size.
 *
 * Every row of the composer carries this except the editor, which is
 * {@link COMPOSER_BODY}. That is the whole rule: **the message gives up space,
 * and nothing else does.** The footer is the only mouse route to attach,
 * discard and send, so it is the last thing that may be squeezed, and a row
 * that is 18px tall has nothing useful to give anyway.
 */
export const COMPOSER_FIXED_ROW = "shrink-0";

/** The one row that gives way. Sized in pixels, shrinkable to nothing. */
export const COMPOSER_BODY = "min-h-0";

/**
 * The attribute the reading column carries so the composer can measure it.
 *
 * The column is the conversation and the composer under it, and its height is a
 * fact about what is currently laid out — how tall the agent drawer is standing,
 * whether it has a pill strip at all — which no store holds and no prop carries
 * down. Asked of the document for the same reason {@link isOverDropTarget} is.
 */
export const READING_COLUMN = "data-mach-reading-column";

/* -------------------------------------------------------------------------- */
/* Where a dropped file lands                                                  */
/* -------------------------------------------------------------------------- */

/** The attribute the composer's root carries so a drop can find it. */
export const DROP_TARGET = "data-mach-drop-target";

/**
 * A point, in the units Tauri reports a drag in.
 *
 * `PhysicalPosition` — device pixels, relative to the window's content area.
 * The DOM measures in CSS pixels, which on this machine are two device pixels
 * to one. Converting is [`isOverDropTarget`]'s first job and the reason a
 * drop over the composer on a Retina display was landing in the header.
 */
export interface DragPoint {
  x: number;
  y: number;
}

/**
 * Is this drag over a composer?
 *
 * Asked of the document rather than of React state, for the same reason
 * `keyboardInComposer` is: the composer's rectangle is a fact about what is
 * currently laid out — the reading pane's scroll position, the height the
 * owner dragged the top edge to, whether the overlay is up — and none of that
 * is in the store at this resolution.
 *
 * More than one composer can be on screen; any of them counts, because
 * dropping on the one in front is the only thing the pointer can be over.
 */
export function isOverDropTarget(
  point: DragPoint,
  doc: Document = document,
  scale: number = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1,
): boolean {
  const x = point.x / scale;
  const y = point.y / scale;
  for (const target of doc.querySelectorAll(`[${DROP_TARGET}]`)) {
    const box = target.getBoundingClientRect();
    if (box.width === 0 && box.height === 0) continue;
    if (x >= box.left && x <= box.right && y >= box.top && y <= box.bottom) return true;
  }
  return false;
}
