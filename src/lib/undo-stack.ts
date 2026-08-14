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
 * traversal — pop, put the entire result on screen, dispatch, and either refine
 * the entry with what came back or put it back where it was. They take
 * the app as an {@link UndoHost} rather than reaching for it, which is what
 * makes the behaviour testable without a window.
 */

import type { AccountId, LabelId, ThreadId } from "@/types";
import type { Command, CommandResult } from "./data";
import type { Selection } from "./selection";

/**
 * Where the list stood when an action was taken.
 *
 * Archiving moves the cursor onward, which is right — the conversation is gone
 * and you want to keep going. Undo puts the conversation back, and until this
 * existed it left the cursor standing next to it: "when I'm on a message
 * (selected) and I archive, then undo, i'd expect the same message selected".
 *
 * It is remembered rather than derived, because a group cannot be derived from.
 * Fifty archived conversations restore fifty rows, and the cursor belongs where
 * the hand actually was — not on whichever member sorts first.
 *
 * The mailbox is part of it for the same reason a row id is not enough to name
 * a row: a cursor only means anything in the list it was in. Restoring one into
 * a mailbox the user has since navigated away from would move them somewhere
 * they did not ask to go, so {@link UndoHost.returnTo} declines instead.
 */
export interface UndoPlace {
  /** The cursor row — the conversation under the hand. */
  threadId: ThreadId | null;
  /** The ticked rows, whole, so the anchor a ⇧J would grow from comes back too. */
  selection: Selection;
  /** The mailbox the list was showing, and the account filter on it. */
  labelId: LabelId;
  accountId: AccountId | null;
}

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
  /**
   * Where the list stood when the action was taken — what ⌘Z returns to.
   *
   * Absent for everything that is not a conversation in a list. A calendar
   * write and a plugin group both record themselves through
   * {@link recordUndo} without one, and an entry that never had a place never
   * acquires one: undoing an RSVP must not move the mail cursor.
   */
  place?: UndoPlace;
  /**
   * Where it stood *after* the action — what ⇧⌘Z returns to.
   *
   * Learned the first time the entry is undone, exactly as {@link
   * UndoEntry.original} is: at that moment the cursor is still wherever the
   * original action left it, and nothing recorded at the time could have known
   * that. Only ever filled in for an entry that has a `place`.
   */
  after?: UndoPlace;
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
  place?: UndoPlace,
): UndoState {
  if (!result.undo) return state;
  if (result.applied.length === 0) return state;
  return recordUndo(state, label, result.undo, now, original, place);
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
 *
 * `place` is omitted by everything that does not act on the thread list — see
 * {@link UndoEntry.place}.
 */
export function recordUndo(
  state: UndoState,
  label: string,
  inverse: Command | Command[],
  now: number,
  original?: Command | Command[],
  place?: UndoPlace,
): UndoState {
  if (Array.isArray(inverse) && inverse.length === 0) return state;

  const entry: UndoEntry = {
    id: state.nextId,
    inverse,
    original,
    place,
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
): command is Extract<Command, { kind: "unarchive" | "untrash" | "notSpam" | "unsnooze" }> {
  return (
    command.kind === "unarchive" ||
    command.kind === "untrash" ||
    command.kind === "notSpam" ||
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
): command is Extract<Command, { kind: "archive" | "trash" | "reportSpam" | "snooze" }> {
  return (
    command.kind === "archive" ||
    command.kind === "trash" ||
    command.kind === "reportSpam" ||
    command.kind === "snooze"
  );
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
   * Say what this whole set of commands is about to do to the list — all of it,
   * now, before any of it is dispatched.
   *
   * `execute` projects the one command it is handed, which is right for a
   * keystroke and wrong for a traversal: a group of three is one gesture, and
   * projecting inside `execute` meant step two's rows could not move until step
   * one's round trip had come back. A three-step ⌘Z repainted in three stages,
   * a Gmail round trip apart — measured at 54ms, 295ms and 519ms for the three
   * rows of one group.
   *
   * The commands arrive in the order they will be dispatched, and a later guess
   * about a conversation replaces an earlier one, so a set that touches the same
   * thread twice lands on the same answer it would have reached one at a time.
   */
  project(commands: Command[]): void;
  /** Where the cursor and the ticked rows are right now. */
  place(): UndoPlace;
  /**
   * Put the cursor and the ticked rows back where the entry remembers them.
   *
   * `arriving` is every conversation the traversal is about to name, which is
   * the answer to "will the row be there". A remembered cursor is reachable if
   * it is in the list already or if it is one of these — an entry undone long
   * after the fact names a row the list dropped hours ago, and the unarchive
   * about to run is precisely what brings it back.
   *
   * Anything else — a different mailbox on screen, a conversation a sync has
   * moved, a filter that no longer shows it — leaves the cursor exactly where
   * the user left it. Pointing it at a row that is not coming would be an
   * invisible cursor, and `j` from an invisible cursor jumps to the top of the
   * list, which is the broken state this refuses to create.
   *
   * Called with `undefined` for every entry that has no place, which is most of
   * the calendar and all of the plugin host; it does nothing then.
   */
  returnTo(place: UndoPlace | undefined, arriving: readonly ThreadId[]): void;
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

  // Where the archive left the cursor, read before this undo moves it. Nothing
  // recorded at the time could have known — an archive's answer says which rows
  // left, never where the hand went next — so ⇧⌘Z learns it here, the same way
  // it learns how to re-apply the action at all.
  const entry = placed(popped.entry) ? { ...popped.entry, after: host.place() } : popped.entry;
  host.write({ ...popped.state, undone: withEntry(popped.state.undone, entry) });

  const steps = undoSteps(entry);
  // Everything the user can see happens here, in the tick the keystroke
  // produced: every step's rows, the cursor, and the sentence saying so. See
  // `showSteps`.
  showSteps(host, steps, `Undid ${uncapitalize(entry.label)}`, "redo", entry.place);

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
  return { entry, ok: true };
}

/** ⇧⌘Z — the same traversal, the other way. */
export async function runRedo(host: UndoHost): Promise<UndoOutcome> {
  const popped = popRedo(host.read());
  if (!popped) return { entry: null, ok: false };

  const steps = redoSteps(popped.entry);
  if (steps.length === 0) {
    // Nothing was learned about how to re-apply this, which means the undo
    // that produced it had no inverse of its own. Leave it where it is and say
    // so, rather than moving it and doing nothing.
    host.say(`Cannot redo ${uncapitalize(popped.entry.label)}`);
    return { entry: popped.entry, ok: false };
  }

  // The mirror of the read in `runUndo`. The undo put the cursor back on the
  // restored conversation; that is where the ⌘Z after this redo should return
  // to, and it is only the same as what the entry already remembers if the
  // user has not moved since.
  const entry = placed(popped.entry) ? { ...popped.entry, place: host.place() } : popped.entry;
  host.write({ ...popped.state, done: withEntry(popped.state.done, entry) });
  showSteps(host, steps, `Redid ${uncapitalize(entry.label)}`, "undo", entry.after);

  const applied = await dispatchSteps(host, steps);
  if (!applied) {
    host.write(restoreRedo(host.read(), entry));
    return { entry, ok: false };
  }
  if (applied.length === steps.length) {
    host.write(refineUndo(host.read(), entry.id, applied));
  }
  return { entry, ok: true };
}

/**
 * The whole visible half of a traversal, in one tick.
 *
 * Nothing here is awaited, so a ⌘Z arriving in a keydown handler has put the
 * entire result on screen — every step's rows, and the sentence saying what
 * happened — in the frame that keystroke produced.
 *
 * # Why the message goes out before the network answers
 *
 * It used to wait for the last round trip, and that was the lag. A single-step
 * undo already moved its rows in the same frame; the toast underneath went on
 * saying "Archived 1 conversation · Undo ⌘Z" until the command answered, which
 * reads as "the key did not take" — the more so when the rows that moved are
 * off screen, which after an archive they usually are. Measured at 59ms to the
 * rows and 308ms to the confirmation, against a 250ms command; at 2s a command
 * the rows still moved at 60ms and the confirmation took 2055ms.
 *
 * This is the same bargain every other write in the app makes: say what
 * happened, and take it back if Google refuses. The taking-back is real and is
 * two things. {@link runUndo} puts the entry back on the stack, so ⌘Z is still
 * offered; and the failed command's own status — `run` reports a refusal even
 * when it was told to be quiet — replaces this message with what Google
 * actually said, because it is dispatched after this one. A confirmation that
 * survived a failure would be the silent-failure bug this project has paid for
 * more than once, so the ordering there is not incidental.
 */
function showSteps(
  host: UndoHost,
  steps: Command[],
  message: string,
  offer: "undo" | "redo",
  place?: UndoPlace,
): void {
  // Drop the previous guess first, then state the new one: an archive's delta
  // must not still be sitting on the row the unarchive is about to describe.
  for (const command of steps) {
    if (restoresThreads(command)) host.restore(command.threadIds);
    else if (hidesThreads(command)) host.hide(command.threadIds);
  }
  host.project(steps);
  // After the projection, because the row the cursor is being put back on is
  // one the projection is putting back — and in the same tick, because a cursor
  // that arrived a round trip after its row would be the 966ms lag all over
  // again, on the one thing the eye is actually following.
  host.returnTo(place, namedThreads(steps));
  host.say(message, offer);
}

/** Every conversation a set of steps names, in the order they are dispatched. */
function namedThreads(steps: Command[]): ThreadId[] {
  const ids: ThreadId[] = [];
  for (const command of steps) {
    if ("threadIds" in command) ids.push(...command.threadIds);
  }
  return ids;
}

/** Whether an entry is one the cursor is tracked for. See {@link UndoEntry.place}. */
function placed(entry: UndoEntry): boolean {
  return entry.place !== undefined;
}

function withEntry(list: UndoEntry[], entry: UndoEntry): UndoEntry[] {
  return list.map((e) => (e.id === entry.id ? entry : e));
}

/**
 * Runs a traversal's commands and collects the inverses they hand back.
 *
 * Returns null the moment one fails: the rest of a half-undone group would
 * compound the mess, and the caller puts the entry back.
 *
 * # In series, still, and on purpose
 *
 * Firing a group's steps concurrently would shorten this loop and buy nothing
 * anybody can see — {@link showSteps} has already put the whole undo on screen,
 * and what is left to wait for is the inverse each command hands back, which
 * only ⇧⌘Z reads and only after the fact. Against that:
 *
 *  * **A group is ordered because it has to be.** It is unwound in reverse for
 *    a reason — unarchiving before un-labelling puts the conversation back with
 *    the label still on it — and `commands::mail` computes each command's label
 *    delta against the store as it finds it. Two commands touching one thread
 *    at once would diff against the same pre-write state and send Gmail the
 *    wrong difference.
 *  * **Stopping at the first failure is only possible in series.** Concurrent
 *    steps are already gone by the time one is refused, so "the rest of a
 *    half-undone group would compound the mess" stops being avoidable.
 *  * **One SQLite writer, one rate limit.** A burst is the shape both of them
 *    are worst at, for a saving of nothing.
 */
async function dispatchSteps(host: UndoHost, steps: Command[]): Promise<Command[] | null> {
  const applied: Command[] = [];
  for (const command of steps) {
    const result = await host.execute(command);
    if (!result || !result.ok) return null;
    if (result.undo) applied.push(result.undo);
  }
  return applied;
}
