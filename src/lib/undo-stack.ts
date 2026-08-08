/**
 * Undo that behaves the way every other Mac app behaves.
 *
 * Before this, undo read the inverse off the transient status toast: it could
 * only ever undo the *last* action, and only while that message was still on
 * screen. Archive three things, glance away, and the ability to take any of it
 * back had quietly expired. ⌘Z was not bound at all.
 *
 * # What belongs on the stack
 *
 * Only actions that already happened and can be reversed by dispatching their
 * own inverse. The command layer hands one back on every `CommandResult`, and
 * an inverse is *exact* — undoing an archive restores the labels the thread
 * actually had, not a guess like INBOX.
 *
 * Sending mail deliberately does **not** go on the stack. It has its own
 * ten-second window, and once the message is gone it is gone; offering ⌘Z
 * afterwards would imply a recall that does not exist.
 *
 * # Why entries do not expire
 *
 * An expiring undo is a trap: the affordance is there, you reach for it late,
 * and it silently does nothing. Entries stay until the stack overflows or the
 * app quits. The risk is undoing against state that has since changed — which
 * the command layer already handles, because every command is idempotent and
 * reports per-id failure rather than corrupting.
 */

import type { Command, CommandResult } from "./data";

/** One reversible thing that happened. */
export interface UndoEntry {
  /** Monotonic, for React keys and for tests. */
  id: number;
  /** The command that reverses it. */
  inverse: Command;
  /** What the user did, phrased for a status line: "Archived 3 conversations". */
  label: string;
  /** The command that was originally run — what redo re-applies. */
  original: Command;
  at: number;
}

export interface UndoState {
  done: UndoEntry[];
  undone: UndoEntry[];
  nextId: number;
}

/**
 * Deep enough to cover a triage session, shallow enough that the oldest entry
 * is never so stale it surprises.
 */
export const MAX_DEPTH = 50;

export function emptyUndo(): UndoState {
  return { done: [], undone: [], nextId: 1 };
}

/**
 * Records a completed action.
 *
 * A result with no inverse is not recorded — some commands have none (an
 * unsnooze whose threads had different wake times cannot be reversed to a
 * single snooze), and a stack entry that cannot act is worse than no entry.
 *
 * A partial failure still records: the inverse the command layer returns covers
 * only the ids that actually applied, so undoing it cannot resurrect a change
 * that never happened.
 */
export function pushUndo(
  state: UndoState,
  original: Command,
  result: CommandResult,
  label: string,
  now: number,
): UndoState {
  if (!result.undo) return state;
  if (result.applied.length === 0) return state;

  const entry: UndoEntry = {
    id: state.nextId,
    inverse: result.undo,
    original,
    label,
    at: now,
  };

  return {
    // Anything undone is no longer redoable once a new action happens —
    // the timeline forked, which is what every editor does.
    undone: [],
    done: [...state.done, entry].slice(-MAX_DEPTH),
    nextId: state.nextId + 1,
  };
}

/** The entry ⌘Z would reverse, or null. */
export function peekUndo(state: UndoState): UndoEntry | null {
  return state.done[state.done.length - 1] ?? null;
}

/** The entry ⇧⌘Z would re-apply, or null. */
export function peekRedo(state: UndoState): UndoEntry | null {
  return state.undone[state.undone.length - 1] ?? null;
}

/**
 * Moves the top entry to the redo side and returns the command to dispatch.
 *
 * The state transition happens up front rather than on success, so holding ⌘Z
 * cannot fire the same inverse twice. If the dispatch fails the caller can put
 * it back with {@link restoreUndo}.
 */
export function popUndo(
  state: UndoState,
): { state: UndoState; entry: UndoEntry } | null {
  const entry = peekUndo(state);
  if (!entry) return null;
  return {
    entry,
    state: {
      ...state,
      done: state.done.slice(0, -1),
      undone: [...state.undone, entry].slice(-MAX_DEPTH),
    },
  };
}

/** The mirror of {@link popUndo}, for ⇧⌘Z. */
export function popRedo(
  state: UndoState,
): { state: UndoState; entry: UndoEntry } | null {
  const entry = peekRedo(state);
  if (!entry) return null;
  return {
    entry,
    state: {
      ...state,
      undone: state.undone.slice(0, -1),
      done: [...state.done, entry].slice(-MAX_DEPTH),
    },
  };
}

/** Puts an entry back after a failed undo, so the affordance does not vanish. */
export function restoreUndo(state: UndoState, entry: UndoEntry): UndoState {
  return {
    ...state,
    done: [...state.done, entry].slice(-MAX_DEPTH),
    undone: state.undone.filter((e) => e.id !== entry.id),
  };
}

/** Mirror of {@link restoreUndo}, for a failed redo. */
export function restoreRedo(state: UndoState, entry: UndoEntry): UndoState {
  return {
    ...state,
    undone: [...state.undone, entry].slice(-MAX_DEPTH),
    done: state.done.filter((e) => e.id !== entry.id),
  };
}

/**
 * The commands whose inverses put threads back on screen.
 *
 * The list is optimistically hidden the moment something is archived, so undo
 * has to clear that hide as well as run the command — otherwise the thread
 * returns to the store and stays invisible.
 */
export function restoresThreads(command: Command): boolean {
  return (
    command.kind === "unarchive" ||
    command.kind === "untrash" ||
    command.kind === "unsnooze"
  );
}

/** "Undo archive 3 conversations" — what a menu item or status line shows. */
export function describeUndo(entry: UndoEntry | null): string | null {
  return entry ? `Undo ${entry.label.toLowerCase()}` : null;
}
