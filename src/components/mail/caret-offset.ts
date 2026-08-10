/**
 * The caret, as a number.
 *
 * Popping the composer out and putting it back moves the editor to a different
 * place in the tree, and React answers that by unmounting one and mounting
 * another — the contenteditable element the caret lived in is gone, so a saved
 * `Range` points at nothing. What survives is the *text*: the caret was 214
 * characters into the message, and it can be 214 characters in again on the
 * other side.
 *
 * That is all this is. `caretOffsetIn` turns a live range into that number and
 * `caretRangeIn` turns the number back into a range in whatever element is
 * there now, with {@link locateOffset} — which knows nothing about the DOM —
 * doing the arithmetic in between.
 *
 * The two halves measure the same thing: `Range.toString()` concatenates text
 * nodes, and the walk here visits text nodes, so a `<br>` counts for nothing on
 * both sides and cannot make them disagree.
 */

/** Which text node the caret lands in, and how far into it. */
export interface CaretPosition {
  index: number;
  offset: number;
}

/**
 * The text node holding character `target`, given every node's length.
 *
 * A boundary belongs to the node *before* it — the caret at the end of "hello"
 * is at the end of "hello", not at the start of whatever follows — which is
 * what keeps a caret typed to the end of a paragraph from reappearing at the
 * top of the next one. Out-of-range targets are clamped rather than refused: a
 * document can only have shrunk between the two calls, and the end of it is the
 * closest honest answer.
 */
export function locateOffset(lengths: readonly number[], target: number): CaretPosition | null {
  if (lengths.length === 0) return null;
  const wanted = Math.max(0, Math.round(target));
  let seen = 0;
  for (let index = 0; index < lengths.length; index++) {
    const length = lengths[index]!;
    if (wanted <= seen + length) return { index, offset: wanted - seen };
    seen += length;
  }
  const last = lengths.length - 1;
  return { index: last, offset: lengths[last]! };
}

/** Every text node under `root`, in document order. */
function textNodes(root: Node): Text[] {
  const doc = root.ownerDocument;
  if (!doc) return [];
  const walker = doc.createTreeWalker(root, 0x4 /* NodeFilter.SHOW_TEXT */);
  const found: Text[] = [];
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    found.push(node as Text);
  }
  return found;
}

/**
 * How many characters into `root` the start of `range` is, or `null` when the
 * range is not in this element at all — which is what an editor that has never
 * been focused hands back, and is not a caret worth remembering.
 */
export function caretOffsetIn(root: HTMLElement, range: Range | null | undefined): number | null {
  if (!range || !root.contains(range.startContainer)) return null;
  const doc = root.ownerDocument;
  if (!doc) return null;
  const before = doc.createRange();
  before.selectNodeContents(root);
  try {
    before.setEnd(range.startContainer, range.startOffset);
  } catch {
    // A container that is somehow not comparable with root. Nothing here is
    // worth throwing over: the caret simply goes back to where it started.
    return null;
  }
  return before.toString().length;
}

/**
 * A collapsed range `offset` characters into `root`, for handing to Squire.
 *
 * An empty document has no text node to point into, so the range is collapsed
 * inside the element itself — which is exactly where a new draft's caret is.
 */
export function caretRangeIn(root: HTMLElement, offset: number): Range | null {
  const doc = root.ownerDocument;
  if (!doc) return null;
  const nodes = textNodes(root);
  const range = doc.createRange();
  const position = locateOffset(
    nodes.map((node) => node.data.length),
    offset,
  );
  if (!position) {
    range.setStart(root, 0);
    range.collapse(true);
    return range;
  }
  const node = nodes[position.index]!;
  range.setStart(node, Math.min(position.offset, node.data.length));
  range.collapse(true);
  return range;
}
