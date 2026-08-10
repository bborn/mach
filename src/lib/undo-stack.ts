/**
 * Undo that behaves the way every other Mac app behaves.
 *
 * Before this, undo read the inverse off the transient status toast: it could
 * only ever undo the *last* action, and only while that message was still on
 * screen. Archive three things, glance away, and the ability to take any of it
 * back had quietly expired. ⌘Z was not bound at all.
 *
 * `useMach` owns the live stack and is where actions are recorded; ⌘Z, ⇧⌘Z and
 * Gmail's bare `z` all arrive at the same two functions at the bottom of this
 * file.
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
 *
 * The *offer* is a different thing from the stack and keeps its own clock: the
 * toast that says "Archived 3 conversations · Undo ⌘Z" times out after
 * `undoWindowSeconds`, because a note about what just happened is only about
 * what just happened. ⌘Z outlives it, and the status bar goes on naming what it
 * would do. See the note on the status timer in `useMach`.
 *
 * # Running one
 *
 * {@link runUndo} and {@link runRedo} at the bottom of this file are the whole
 * traversal — pop, clear the list's optimistic hide, dispatch, and either
 * refine the entry with what came back or put it back where it was. They take
 * the app as an {@link UndoHost} rather than reaching for it, which is what
 * makes the behaviour testable without a window.
 */

import type { ThreadId } from "@/types";
import type { Command, CommandResult } from "./data";

/**
 * One reversible thing that happened.
 *
 * `inverse` and `original` are each *a* command or a list of them, because one
 * thing the user did is not always one command: a plugin action that labels and
 * then archives is a single gesture and has to be a single ⌘Z. A list is stored
 * in the order it ran and dispatched in reverse — see {@link undoSteps}.
 */
export interface UndoEntry {
  /** Monotonic, for React keys and for tests. */
  id: number;
  /** The command that reverses it. */
  inverse: Command | Command[];
  /** What the user did, phrased for a status line: "Archived 3 conversations". */
  label: string;
  /**
   * The command that re-applies it — what redo runs.
   *
   * Absent until the entry has actually been undone once. The exact way to
   * re-apply an action is the inverse of its inverse, and the only honest
   * source of that is the command layer's answer to the undo itself, which
   * {@link runUndo} writes back here. Nothing guesses.
   */
  original?: Command | Command[];
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
  return recordUndo(state, label, result.undo, now, original);
}

/**
 * Records an action known only by its inverse.
 *
 * The command layer is not the only thing that produces one. A plugin action
 * hands back the inverses of every command it ran, as a group; the calendar's
 * write path builds its own status message. Both know what they did and how to
 * take it back, and neither has a {@link CommandResult} to hand over — so this
 * is the door they come through.
 *
 * `original` is optional and normally omitted, because guessing how to
 * re-apply an action is exactly what this stack refuses to do. It is filled in
 * for real the first time the entry is undone.
 */
export function recordUndo(
  state: UndoState,
  label: string,
  inverse: Command | Command[],
  now: number,
  original?: Command | Command[],
): UndoState {
  if (Array.isArray(inverse) && inverse.length === 0) return state;

  const entry: UndoEntry = {
    id: state.nextId,
    inverse,
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

/**
 * The commands ⌘Z dispatches for an entry, in the order they must go out.
 *
 * A group is recorded in the order it ran and unwound in reverse, because
 * unarchiving before un-labelling would put the thread back with the label
 * still on it. A refined list is written back in the order it was *dispatched*,
 * so reversing it again lands on the original order — which is what makes
 * undo → redo → undo keep meaning the same thing.
 */
export function undoSteps(entry: UndoEntry): Command[] {
  return stepsOf(entry.inverse);
}

/** The mirror of {@link undoSteps}. Empty when the entry cannot be re-applied. */
export function redoSteps(entry: UndoEntry): Command[] {
  return entry.original === undefined ? [] : stepsOf(entry.original);
}

function stepsOf(value: Command | Command[]): Command[] {
  return Array.isArray(value) ? [...value].reverse() : [value];
}

/** One command stays one command; a group stays a group. */
function packSteps(steps: Command[]): Command | Command[] {
  return steps.length === 1 ? steps[0]! : steps;
}

/**
 * Teaches an undone entry how to re-apply itself.
 *
 * `steps` are the inverses the command layer handed back while the undo ran,
 * in the order they ran. That is the original action, exactly — including for
 * the calendar, where nothing local could have worked it out: the inverse of
 * "delete this event" is a create carrying the whole event, and only the
 * command layer has it.
 */
export function refineRedo(state: UndoState, id: number, steps: Command[]): UndoState {
  if (steps.length === 0) return state;
  return {
    ...state,
    undone: state.undone.map((e) => (e.id === id ? { ...e, original: packSteps(steps) } : e)),
  };
}

/** The mirror of {@link refineRedo}, for the entry a redo just put back. */
export function refineUndo(state: UndoState, id: number, steps: Command[]): UndoState {
  if (steps.length === 0) return state;
  return {
    ...state,
    done: state.done.map((e) => (e.id === id ? { ...e, inverse: packSteps(steps) } : e)),
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
export function restoresThreads(
  command: Command,
): command is Extract<Command, { kind: "unarchive" | "untrash" | "unsnooze" }> {
  return (
    command.kind === "unarchive" ||
    command.kind === "untrash" ||
    command.kind === "unsnooze"
  );
}

/**
 * The mirror: commands whose effect is that rows leave the list.
 *
 * Redo needs this for the same reason undo needs {@link restoresThreads}. The
 * original archive hid the row the instant it was pressed; a redo that waited
 * for a round trip before the row left again would feel like a different,
 * slower key than the one it is repeating.
 */
export function hidesThreads(
  command: Command,
): command is Extract<Command, { kind: "archive" | "trash" | "snooze" }> {
  return command.kind === "archive" || command.kind === "trash" || command.kind === "snooze";
}

/** "Undo archived 3 conversations" — what a menu item or status line shows. */
export function describeUndo(entry: UndoEntry | null): string | null {
  return entry ? `Undo ${uncapitalize(entry.label)}` : null;
}

/** "Redo archived 3 conversations", for ⇧⌘Z. */
export function describeRedo(entry: UndoEntry | null): string | null {
  return entry ? `Redo ${uncapitalize(entry.label)}` : null;
}

/**
 * Drops the label's leading capital and nothing else.
 *
 * Lowercasing the whole string would read fine for "Archived 3 conversations"
 * and then mangle `Created “Lunch with Dana”`, which is a label the calendar
 * really produces.
 */
function uncapitalize(label: string): string {
  return label.charAt(0).toLowerCase() + label.slice(1);
}

/* -------------------------------------------------------------------------- */
/* Running one                                                                 */
/* -------------------------------------------------------------------------- */

/**
 * Everything a traversal needs from the app.
 *
 * `read`/`write` rather than a state argument because an undo spans a round
 * trip, and the stack can be pushed to while one is in flight — reading it
 * fresh on the way out is what stops a slow undo from swallowing an action the
 * user took during it.
 */
export interface UndoHost {
  read(): UndoState;
  write(next: UndoState): void;
  /** Dispatch a command. `null` means it never reached the command layer. */
  execute(command: Command): Promise<CommandResult | null>;
  /**
   * Retract the guess standing for these conversations.
   *
   * The two used to be the optimistic edit itself — clear the hide for an
   * unarchive, set it for a redone archive — because the command layer's caller
   * knew nothing about what a command did to the list. It does now: `execute`
   * projects the command it is handed. What is left to do here is drop the
   * *previous* guess, so an archive's delta is not still sitting on the row the
   * unarchive about to run is describing.
   *
   * They stay two methods rather than one because {@link restoresThreads} and
   * {@link hidesThreads} are two different questions about a command, and the
   * host is free to answer them differently.
   */
  restore(threadIds: ThreadId[]): void;
  /** The mirror of {@link UndoHost.restore}, for a redone action. */
  hide(threadIds: ThreadId[]): void;
  /**
   * Say what happened, once it has.
   *
   * `offer` is the button the message should carry: having just undone
   * something, the useful thing to hold out is the redo, and vice versa. The
   * traversal is the only thing that knows which way it went — the surface
   * showing the message would have to read the wording to guess — so it says
   * so outright. Omitted when there is nothing to hold out, which is what a
   * refusal like "Cannot redo …" is.
   */
  say(message: string, offer?: "undo" | "redo"): void;
}

export interface UndoOutcome {
  /** The entry that was traversed, or null when there was nothing to traverse. */
  entry: UndoEntry | null;
  /** False when a command failed and the entry was put back where it was. */
  ok: boolean;
}

/**
 * ⌘Z.
 *
 * The pop happens up front and through the host's own state, so a second ⌘Z
 * arriving in the same tick — a key held down, or the macOS menu replaying a
 * token behind the real keystroke — takes the *next* entry rather than running
 * this one's inverse twice.
 */
export async function runUndo(host: UndoHost): Promise<UndoOutcome> {
  const popped = popUndo(host.read());
  if (!popped) return { entry: null, ok: false };
  host.write(popped.state);

  const entry = popped.entry;
  const steps = undoSteps(entry);
  const applied = await dispatchSteps(host, steps);
  if (!applied) {
    // The affordance must not vanish because the network blipped. Whatever
    // failed has already said so; this only puts the entry back.
    host.write(restoreUndo(host.read(), entry));
    return { entry, ok: false };
  }

  // Only a complete set is a redo. A group that gave back two inverses out of
  // three would re-apply two thirds of an action, which is worse than a ⇧⌘Z
  // that honestly says it cannot.
  if (applied.length === steps.length) {
    host.write(refineRedo(host.read(), entry.id, applied));
  }
  host.say(`Undid ${uncapitalize(entry.label)}`, "redo");
  return { entry, ok: true };
}

/** ⇧⌘Z — the same traversal, the other way. */
export async function runRedo(host: UndoHost): Promise<UndoOutcome> {
  const popped = popRedo(host.read());
  if (!popped) return { entry: null, ok: false };

  const entry = popped.entry;
  const steps = redoSteps(entry);
  if (steps.length === 0) {
    // Nothing was learned about how to re-apply this, which means the undo
    // that produced it had no inverse of its own. Leave it where it is and say
    // so, rather than moving it and doing nothing.
    host.say(`Cannot redo ${uncapitalize(entry.label)}`);
    return { entry, ok: false };
  }

  host.write(popped.state);
  const applied = await dispatchSteps(host, steps);
  if (!applied) {
    host.write(restoreRedo(host.read(), entry));
    return { entry, ok: false };
  }
  if (applied.length === steps.length) {
    host.write(refineUndo(host.read(), entry.id, applied));
  }
  host.say(`Redid ${uncapitalize(entry.label)}`, "undo");
  return { entry, ok: true };
}

/**
 * Runs a traversal's commands and collects the inverses they hand back.
 *
 * The optimistic edits go out for the whole set before the first command does,
 * so a group of five lands on screen as one change rather than five. Returns
 * null the moment one fails: the rest of a half-undone group would compound
 * the mess, and the caller puts the entry back.
 */
async function dispatchSteps(host: UndoHost, steps: Command[]): Promise<Command[] | null> {
  for (const command of steps) {
    if (restoresThreads(command)) host.restore(command.threadIds);
    else if (hidesThreads(command)) host.hide(command.threadIds);
  }

  const applied: Command[] = [];
  for (const command of steps) {
    const result = await host.execute(command);
    if (!result || !result.ok) return null;
    if (result.undo) applied.push(result.undo);
  }
  return applied;
}
