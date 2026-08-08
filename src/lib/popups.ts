/**
 * How many floating popups are open, and nothing else.
 *
 * The keymap owns exactly one `keydown` listener and it runs in the **capture**
 * phase (see `hooks/useKeymap.tsx`), which is what makes Escape work while the
 * caret is inside a text field. The cost is that a component further down the
 * tree cannot intercept a key first: when a select menu is open inside the
 * event modal, the modal's Escape binding fires *before* Base UI ever sees the
 * event, so one press would close the menu **and** throw away the edit.
 *
 * The fix is for the modal's binding to decline while a popup is open — a
 * binding whose `when()` is false is never a candidate, so the key is neither
 * prevented nor stopped and Base UI closes just the popup. That requires a
 * synchronous answer to "is anything floating right now?", read during the
 * keydown, which is why this is a plain counter rather than React state.
 *
 * Every popup-bearing primitive in `components/ui` registers here. Nothing
 * else has to know.
 */

let open = new Set<string>();
const listeners = new Set<() => void>();

function emit(): void {
  for (const listener of listeners) listener();
}

/** Called by the ui primitives as their popups open and close. */
export function setPopupOpen(id: string, isOpen: boolean): void {
  const had = open.has(id);
  if (isOpen === had) return;
  // A fresh Set each time: `useSyncExternalStore` compares snapshots by
  // identity, and the snapshot here is the count, so this is only hygiene for
  // anyone who later wants the ids.
  open = new Set(open);
  if (isOpen) open.add(id);
  else open.delete(id);
  emit();
}

/** True while any select menu, popover or tooltip popup is on screen. */
export function anyPopupOpen(): boolean {
  return open.size > 0;
}

export function subscribePopups(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Test seam. */
export function resetPopups(): void {
  open = new Set();
  emit();
}
