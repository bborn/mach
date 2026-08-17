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
 *
 * # A dropped file
 *
 * Which is a question about placement too: whether a drag is over a composer is
 * a hit test against the rectangle the two sections above decided on. The
 * subscription that delivers the drag is here with it, because the units it
 * arrives in are the units that hit test has to work in — see {@link DragPoint}.
 *
 * The answer is a {@link DropRegion} rather than a boolean, because a file let
 * go on the writing area means something different from one let go on the
 * footer: the first goes in the message, the second goes beside it.
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

/* -------------------------------------------------------------------------- */
/* Where a dropped file lands                                                  */
/* -------------------------------------------------------------------------- */

/** The attribute the composer's root carries so a drop can find it. */
export const DROP_TARGET = "data-mach-drop-target";

/**
 * The attribute the writing area carries, inside that root.
 *
 * The composer answers a drop two different ways depending on where inside it
 * the pointer was, so the hit test needs a second rectangle. It is an attribute
 * rather than a ref for the same reason [`DROP_TARGET`] is: the box being
 * tested is whatever is laid out at the moment of the drop, and the height of
 * the writing area is a number the owner drags.
 */
export const DROP_BODY = "data-mach-drop-body";

/**
 * Where inside a composer a drop landed.
 *
 * `body` is the writing area, and means the file goes *in* the message —
 * `<img src="cid:…">` at the caret, the way Gmail does it. `composer` is
 * everywhere else on the composer, and means the file goes beside it. Only
 * images can take the first road; a PDF dropped on the body is attached, which
 * is [`attach::add_bytes`]'s rule and not this one's.
 */
export type DropRegion = "body" | "composer";

/**
 * A point, in the units Tauri reports a drag in.
 *
 * The payload's type says `PhysicalPosition`, and on macOS that type is a lie.
 * wry builds the point from two AppKit values — `NSDraggingInfo`'s
 * `draggingLocation` and the webview's own `frame` — and AppKit measures both
 * in *points*, never in backing pixels; nothing between there and here
 * multiplies by the scale factor. See `wry/src/wkwebview/drag_drop.rs` (the
 * `frame.size.height - dl.y` flip) and `tauri-runtime-wry`, which wraps that
 * pair in `PhysicalPosition::new` unconverted. On Windows the same field really
 * is physical — `ScreenToClient` on an HWND — which is where the type name
 * comes from and why it is wrong here.
 *
 * So the point arrives in the same CSS pixels `getBoundingClientRect` returns,
 * and [`isOverDropTarget`] compares them directly. Dividing by
 * `devicePixelRatio` is what a reading of the *type* suggests, and on this
 * Retina machine it halved every coordinate into the top-left quadrant of the
 * window: the docked composer sits in the bottom-right and could never be hit,
 * so a file dropped on a reply did nothing at all.
 *
 * Mach ships `.app` and `.dmg` and nothing else. A Windows build would have to
 * scale here, and would find this comment.
 */
export interface DragPoint {
  x: number;
  y: number;
}

/**
 * Which part of a composer this drag is over, if any.
 *
 * Asked of the document rather than of React state, for the same reason
 * `keyboardInComposer` is: the composer's rectangle is a fact about what is
 * currently laid out — the reading pane's scroll position, the height the
 * owner dragged the top edge to, whether the overlay is up — and none of that
 * is in the store at this resolution.
 *
 * More than one composer can be on screen; any of them counts, because
 * dropping on the one in front is the only thing the pointer can be over.
 *
 * The writing area is looked for *within* the composer that was hit rather
 * than across the document, so a second composer's editor can never claim a
 * drop that landed on this one's footer.
 */
export function dropRegionAt(point: DragPoint, doc: Document = document): DropRegion | null {
  for (const target of doc.querySelectorAll(`[${DROP_TARGET}]`)) {
    if (!contains(target, point)) continue;
    for (const body of target.querySelectorAll(`[${DROP_BODY}]`)) {
      if (contains(body, point)) return "body";
    }
    return "composer";
  }
  return null;
}

/**
 * Is this drag over a composer at all?
 *
 * The question the guard on the drop still asks — a release outside every
 * composer does nothing, whichever region the inside would have been.
 */
export function isOverDropTarget(point: DragPoint, doc: Document = document): boolean {
  return dropRegionAt(point, doc) !== null;
}

/**
 * A point inside an element's box, in the units [`DragPoint`] arrives in.
 *
 * A box of no size is skipped rather than tested: a composer being unmounted,
 * or a writing area inside a `display: none` background composer, reports
 * `0,0,0,0`, and the origin of the window would otherwise be inside all of them.
 */
function contains(element: Element, point: DragPoint): boolean {
  const box = element.getBoundingClientRect();
  if (box.width === 0 && box.height === 0) return false;
  return (
    point.x >= box.left &&
    point.x <= box.right &&
    point.y >= box.top &&
    point.y <= box.bottom
  );
}

/* -------------------------------------------------------------------------- */
/* Hearing about the drag at all                                               */
/* -------------------------------------------------------------------------- */

/**
 * One thing the window told us about a file being dragged over it.
 *
 * A member per `type` rather than `"enter" | "over"` on one of them: a
 * discriminant that is itself a union does not narrow away in the *negative*
 * branch, so the drop — the only member carrying paths — could not be reached
 * by elimination.
 */
export type DragDropSignal =
  | { type: "enter"; position: DragPoint; paths: string[] }
  | { type: "over"; position: DragPoint }
  | { type: "drop"; position: DragPoint; paths: string[] }
  | { type: "leave" };

/** How [`subscribeDragDrop`] reaches Tauri. Injected so tests need no app. */
export type DragDropRegistrar = (
  handler: (signal: DragDropSignal) => void,
) => Promise<() => void>;

/**
 * Listen for files dragged over this window, with a cleanup that is synchronous.
 *
 * The shape is the one `subscribeLinkFailures` already uses, and it exists for
 * the same reason: **registering is a promise and React's cleanup is not.** An
 * effect that pushes its `unlisten` into a variable *after* two `await`s hands
 * back a cleanup that, when it runs first, has nothing to call — so the
 * listener stays registered for the life of the window and only a captured flag
 * stops it acting. The composer's effect used to re-run on every keystroke,
 * because its dependency was a callback closed over the drafts, so that was one
 * abandoned registration per character typed.
 *
 * Tauri does not lose the drop over it — every listener registered for an event
 * is called, so the live one still heard the file. What it costs is four Rust
 * listener entries and four JS callbacks per keystroke, all of them woken on
 * every frame of every drag, forever.
 *
 * Either order is now safe: a cleanup that runs before the registration
 * resolves sets `cancelled`, and the registration undoes itself the moment it
 * arrives.
 */
export function subscribeDragDrop(
  handler: (signal: DragDropSignal) => void,
  register: DragDropRegistrar = tauriDragDrop,
): () => void {
  let unlisten: (() => void) | null = null;
  let cancelled = false;

  void register(handler)
    .then((off) => {
      if (cancelled) off();
      else unlisten = off;
    })
    // A browser tab has no webview to listen to, and no paths either — the
    // composer is developed against Vite most of the time. Failing to subscribe
    // is not something to report through the subscription.
    .catch(() => {});

  return () => {
    cancelled = true;
    unlisten?.();
    unlisten = null;
  };
}

const tauriDragDrop: DragDropRegistrar = async (handler) => {
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  const off = await getCurrentWebview().onDragDropEvent((event) => {
    handler(event.payload as DragDropSignal);
  });
  return () => void off();
};
