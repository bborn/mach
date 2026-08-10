/**
 * Swapping an evicted message's text for its HTML without moving the page.
 *
 * A message whose `body_html` was evicted (see `src-tauri/src/evict/`) renders
 * from `body_text` the moment it is opened and never waits for anything. The
 * HTML arrives a few hundred milliseconds later, and it is usually *taller* than
 * the text it replaces — a newsletter's text part is a few lines and its markup
 * is a page. Dropping that in naively does two visible things:
 *
 * 1. Everything below the message moves. In a forty-message thread the reply the
 *    reader is looking at slides off the screen because something above it grew.
 * 2. The paragraph under the reader's eyes is replaced mid-sentence.
 *
 * The first has an exact fix and the second does not, so they get different
 * treatment. The first is corrected: the message's own top is pinned in the
 * viewport across the swap, so nothing above it can move. The second is avoided:
 * while the reader is *inside* the message — its top scrolled past, its bottom
 * not yet reached — the upgrade waits. It is applied the moment that stops being
 * true, which is the next scroll.
 *
 * Everything here is a pure function so it can be tested without a WebView, a
 * scroll container, or a body.
 */

/** Where a message's box sits, in the scroll viewport's own coordinates. */
export interface MessagePosition {
  /** Top of the box. Negative means it has been scrolled past. */
  top: number;
  /** Bottom of the box. */
  bottom: number;
}

/**
 * Is the reader's eye inside this message?
 *
 * True when the top has gone off the top of the viewport and the bottom has not
 * arrived yet — the message fills the view, so any change to it changes what is
 * being read. A message entirely above, entirely below, or with its top still on
 * screen is not being read *through*, and growth in it is either invisible or
 * happens below the point of attention.
 */
export function isBeingRead(position: MessagePosition): boolean {
  return position.top < 0 && position.bottom > 0;
}

/**
 * Apply the upgrade now, or hold it?
 *
 * `engaged` is "the reader has scrolled since this message opened". Without it
 * the very common case — open a message, look at the top, HTML lands 200 ms
 * later — would be held whenever the message happened to be taller than the
 * pane, which is most of them.
 */
export function shouldApplyUpgrade(engaged: boolean, position: MessagePosition): boolean {
  if (!engaged) return true;
  return !isBeingRead(position);
}

/**
 * The scroll offset that puts the message's top back where it was.
 *
 * `anchor` is where the top sat before the swap and `moved` is where it sits
 * now, both in the viewport's coordinates. Positive drift means the box was
 * pushed down and the container has to scroll down by the same amount.
 *
 * Clamped at zero because a correction that would scroll above the top of the
 * content is not a correction, and because `scrollTop` refuses it anyway —
 * silently, which would leave the caller believing it had settled.
 */
export function correctedScrollTop(scrollTop: number, anchor: number, moved: number): number {
  const next = scrollTop + (moved - anchor);
  return next > 0 ? next : 0;
}

/** Drift small enough that acting on it costs a reflow and buys nothing. */
export const ANCHOR_EPSILON = 1;

export function needsCorrection(anchor: number, moved: number): boolean {
  return Math.abs(moved - anchor) > ANCHOR_EPSILON;
}

/**
 * How long the anchor is held after a swap.
 *
 * The correction cannot be a single measurement. The new HTML goes into a
 * sandboxed iframe that reports its height through a `ResizeObserver`, and that
 * height arrives over several frames — first paint, then the images. So the
 * anchor is re-pinned on every frame for this long, which covers the settling
 * without leaving a listener that fights the reader's own scrolling afterwards.
 */
export const ANCHOR_HOLD_MS = 1200;

/**
 * What the walk below needs to know about a node.
 *
 * A probe rather than an `HTMLElement` because `overflowY` is only reachable
 * through `getComputedStyle`, which needs a document — and the rule being tested
 * ("the first ancestor that both is scrollable and has something to scroll") has
 * nothing to do with one.
 */
export interface ScrollProbe<T> {
  parent(node: T): T | null;
  overflowY(node: T): string;
  scrollHeight(node: T): number;
  clientHeight(node: T): number;
}

/**
 * The nearest ancestor that actually scrolls.
 *
 * `document.scrollingElement` is not the answer in this app: the window never
 * scrolls, the reading pane does (`ui/scroll-area.tsx` — a plain
 * `overflow-y-auto` div), and which element that is depends on where the message
 * was mounted. The `scrollHeight > clientHeight` half matters as much as the
 * `overflow` half: a pane with nothing to scroll yet would swallow the
 * correction, and the pane above it is the one that has to move.
 */
export function findScroller<T>(from: T | null, probe: ScrollProbe<T>): T | null {
  let node = from === null ? null : probe.parent(from);
  while (node !== null) {
    const overflow = probe.overflowY(node);
    const scrolls = overflow === "auto" || overflow === "scroll";
    if (scrolls && probe.scrollHeight(node) > probe.clientHeight(node)) return node;
    node = probe.parent(node);
  }
  return null;
}
