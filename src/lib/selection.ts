/**
 * Multi-select algebra for the thread list.
 *
 * Selection is the one piece of list state that gets *felt* rather than seen:
 * every mail client and file manager implements the same rules, and a UI that
 * gets one of them subtly wrong reads as broken even when nobody can say why.
 * So the rules live here, as pure functions over three fields, and are tested
 * rather than eyeballed. `useMach` holds one `Selection` and does nothing to it
 * that is not one of these.
 *
 * The three fields:
 *
 *  * `ids`     — what is selected, in list order.
 *  * `anchor`  — the row a shift-range grows from. Set by a plain click, by a
 *                ⌘-click, and by `x`; never moved by shift itself.
 *  * `base`    — the selection as it stood *before* the current shift-range.
 *
 * `base` is the whole trick. Shift+click is `base ∪ range(anchor → target)`,
 * so shift-clicking a second time from the same anchor **replaces** the range
 * instead of accumulating one — which is what Finder, Gmail and every list on
 * the platform do — while a selection built with ⌘-click before the range
 * survives it. Without `base` you either lose the ⌘-clicks or leave a trail of
 * every row the pointer ever swept over.
 */

import type { ThreadId } from "@/types";

export interface Selection {
  readonly ids: readonly ThreadId[];
  readonly anchor: ThreadId | null;
  readonly base: readonly ThreadId[];
}

export const emptySelection: Selection = { ids: [], anchor: null, base: [] };

export function hasSelection(selection: Selection): boolean {
  return selection.ids.length > 0;
}

export function isSelected(selection: Selection, id: ThreadId): boolean {
  return selection.ids.includes(id);
}

/**
 * A plain click, or any cursor move: this row becomes the anchor and any
 * multi-selection goes away.
 *
 * Note what this does *not* do: it does not select the row. A single row under
 * the cursor is not a selection — it is the thing you are reading — and calling
 * it one would put "1 selected" on screen every time the user pressed `j`.
 */
export function anchorAt(selection: Selection, id: ThreadId): Selection {
  if (selection.anchor === id && selection.ids.length === 0) return selection;
  return { ids: [], anchor: id, base: [] };
}

/**
 * The cursor moved without shift — `j`, `k`, an arrow.
 *
 * The range now starts here, but nothing is selected or deselected: walking
 * past a row you already ticked must not untick it. Committing `ids` as the new
 * `base` is what makes the *next* shift-range grow on top of the selection
 * rather than replace it.
 */
export function reanchor(selection: Selection, id: ThreadId): Selection {
  if (selection.anchor === id && sameIds(selection.base, selection.ids)) return selection;
  return { ids: selection.ids, anchor: id, base: selection.ids };
}

/** ⌘/Ctrl+click, and `x`: flip exactly one row and leave the rest alone. */
export function toggle(selection: Selection, id: ThreadId): Selection {
  const ids = isSelected(selection, id)
    ? selection.ids.filter((other) => other !== id)
    : [...selection.ids, id];
  // The toggled row is the new anchor — a range dragged out afterwards starts
  // where the hand last was, not where it was three clicks ago.
  return { ids, anchor: id, base: ids };
}

/**
 * Shift+click, ⇧J and ⇧K: select everything between the anchor and `id`.
 *
 * Replaces whatever the previous range from this anchor covered (see `base`),
 * so dragging a range shorter does not leave the long version behind.
 */
export function extendTo(
  selection: Selection,
  id: ThreadId,
  order: readonly ThreadId[],
): Selection {
  const anchor = selection.anchor ?? id;
  const range = between(order, anchor, id);
  const ids = inOrder([...selection.base, ...(range.length > 0 ? range : [id])], order);
  return { ids, anchor, base: selection.base };
}

/** Everything currently in the list. The anchor is left where it was. */
export function selectAll(selection: Selection, order: readonly ThreadId[]): Selection {
  const ids = [...order];
  return { ids, anchor: selection.anchor ?? ids[0] ?? null, base: ids };
}

/** True when the whole list is selected and nothing else is. */
export function allSelected(selection: Selection, order: readonly ThreadId[]): boolean {
  if (order.length === 0 || selection.ids.length !== order.length) return false;
  const selected = new Set(selection.ids);
  return order.every((id) => selected.has(id));
}

/** ⌘A: select the loaded list, or clear it if it is already all selected. */
export function toggleAll(selection: Selection, order: readonly ThreadId[]): Selection {
  return allSelected(selection, order) ? clear(selection) : selectAll(selection, order);
}

/**
 * Escape, and the end of a bulk action.
 *
 * The anchor survives: the row the user last clicked is still where a range
 * would sensibly start, and forgetting it would make the shift+click after an
 * Escape select from nowhere.
 */
export function clear(selection: Selection): Selection {
  if (selection.ids.length === 0 && selection.base.length === 0) return selection;
  return { ids: [], anchor: selection.anchor, base: [] };
}

/** Select exactly these ids — how the survivors of a partial failure come back. */
export function selectOnly(
  selection: Selection,
  ids: readonly ThreadId[],
  order: readonly ThreadId[],
): Selection {
  const next = inOrder(ids, order);
  return { ids: next, anchor: next[0] ?? selection.anchor, base: next };
}

/**
 * Drop ids that have left the list.
 *
 * `threads-changed` fires constantly during a sync, and the list is refetched
 * under the selection every time. Anything archived elsewhere, moved out of the
 * mailbox, or simply gone has to leave the selection too — a count that
 * includes rows that no longer exist is a count that lies.
 *
 * Returns the *same object* when nothing changed, so the effect that calls it
 * on every list change can dispatch only on a real difference.
 */
export function prune(selection: Selection, order: readonly ThreadId[]): Selection {
  const present = new Set(order);
  const ids = selection.ids.filter((id) => present.has(id));
  const base = selection.base.filter((id) => present.has(id));
  const anchor = selection.anchor !== null && present.has(selection.anchor) ? selection.anchor : null;
  if (
    ids.length === selection.ids.length &&
    base.length === selection.base.length &&
    anchor === selection.anchor
  ) {
    return selection;
  }
  return { ids, anchor, base };
}

/**
 * What a command should act on: the selection if there is one, otherwise the
 * row under the cursor.
 *
 * One array, however many threads — the command layer batches by (account,
 * label delta) and issues one Gmail `batchModify` per group, which it can only
 * do if the ids arrive together. Fifty commands of one would be fifty round
 * trips and fifty undo entries.
 */
export function commandTargets(
  selection: Selection,
  focused: ThreadId | null,
): ThreadId[] {
  if (selection.ids.length > 0) return [...selection.ids];
  return focused === null ? [] : [focused];
}

/**
 * Where the cursor lands once these ids leave the list: the next row down, or
 * the last one up if the removal took the tail.
 */
export function nextAfterRemoval(
  order: readonly ThreadId[],
  removed: readonly ThreadId[],
  focused: ThreadId | null,
): ThreadId | null {
  const gone = new Set(removed);
  if (focused === null || !gone.has(focused)) return focused;
  const at = order.indexOf(focused);
  if (at === -1) return null;
  for (let i = at + 1; i < order.length; i++) {
    const id = order[i];
    if (id !== undefined && !gone.has(id)) return id;
  }
  for (let i = at - 1; i >= 0; i--) {
    const id = order[i];
    if (id !== undefined && !gone.has(id)) return id;
  }
  return null;
}

/** The rows between two ids, inclusive, in list order. Empty if either is gone. */
export function between(
  order: readonly ThreadId[],
  a: ThreadId,
  b: ThreadId,
): ThreadId[] {
  const from = order.indexOf(a);
  const to = order.indexOf(b);
  if (from === -1 || to === -1) return [];
  return order.slice(Math.min(from, to), Math.max(from, to) + 1);
}

/**
 * What ⌘A actually did, said out loud.
 *
 * The list is a page of an infinite one. Selecting "all" can only ever mean
 * "all of what has been fetched", and claiming 40,000 threads before archiving
 * 250 of them is the failure this sentence exists to prevent.
 */
export function selectAllMessage(count: number, hasMore: boolean): string {
  return hasMore
    ? `${count} selected — every conversation loaded so far, not the whole mailbox`
    : `${count} selected — every conversation in this mailbox`;
}

/** Ids sorted into list order, deduplicated, with anything unknown dropped. */
function inOrder(ids: readonly ThreadId[], order: readonly ThreadId[]): ThreadId[] {
  const wanted = new Set(ids);
  return order.filter((id) => wanted.has(id));
}

function sameIds(a: readonly ThreadId[], b: readonly ThreadId[]): boolean {
  return a.length === b.length && a.every((id, i) => id === b[i]);
}
