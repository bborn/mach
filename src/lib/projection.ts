/**
 * What a command is known to do to a conversation, before anything is refetched.
 *
 * # Why this exists at all
 *
 * `execute_command` writes SQLite and answers in well under a millisecond, and
 * then emits `threads-changed`. The frontend coalesces that over 600ms and
 * `list_threads` has its own round trip after it. For that whole span the list
 * on screen is a copy fetched *before* the write, so anything that waits for it
 * is a keystroke that visibly does nothing for most of a second.
 *
 * Archive, trash and snooze already avoided that with a set of hidden ids, and
 * star with a map of booleans, and mark-read with a third set. Four bespoke
 * pieces of state, three of which were never retired — a thread archived here
 * and unarchived on the phone stayed invisible in Mach until relaunch, because
 * nothing ever took the id back out of the hidden set. Label had none at all,
 * and neither did the inverse a ⌘Z dispatches, so undoing a star flashed for
 * the same 600ms the star itself used to.
 *
 * # One mechanism
 *
 * Every mail command in this app is a **label delta**. That is not a
 * simplification for the frontend's benefit; it is what `commands::mail` does
 * on the other side of the IPC seam — it computes a target label set per
 * thread, diffs it against the prior one, and sends that diff to Gmail.
 * Archive removes `INBOX`. Trash adds `TRASH` and removes `INBOX`. Star adds or
 * removes `STARRED`. Snooze removes `INBOX`. Mark-read removes `UNREAD`.
 * Label adds or removes the label it names.
 *
 * So one guess covers all of them: the labels a command adds, the labels it
 * removes, and the unread flag it sets. From that, everything the list needs
 * falls out —
 *
 *  * **the row's appearance**, by applying the delta to the row's own labels;
 *  * **whether the row still belongs in the mailbox on screen**, by asking
 *    whether the mailbox's label survived the delta. This is the same question
 *    `list_threads` answers with `EXISTS (thread_labels …)`, asked locally.
 *
 * That second point is what replaces the hidden-id set, and it is strictly more
 * correct than one: archiving in the inbox hides the row *in the inbox*, and
 * the same conversation goes on showing in All Mail, in a label, and in search,
 * because those mailboxes' labels are still on it. The hidden-id set had no way
 * to express that and hid the row everywhere.
 *
 * # Retiring one
 *
 * A guess stops being a guess when the loaded list agrees with it — never on a
 * clock. See {@link settledGuesses}.
 */

import type { LabelId, Thread, ThreadId } from "@/types";
import { isMailCommand, type Command, type MailCommand } from "./data";

/** Gmail's system labels this module names directly, as `commands::mail` does. */
export const INBOX = "INBOX";
export const UNREAD = "UNREAD";
export const STARRED = "STARRED";
export const SPAM = "SPAM";
export const TRASH = "TRASH";
export const DRAFT = "DRAFT";

/**
 * One conversation's worth of "this has happened, the store just does not say
 * so yet".
 *
 * A delta rather than a target label set, and the difference matters twice.
 * Applied, a delta lands correctly on a row that has been refetched since the
 * guess was made. Compared, it only claims what the command actually decided —
 * which is what lets a snooze guess retire at all, given that the frontend
 * cannot name the per-account `Mach/Snoozed` label the backend also applies.
 */
export interface ThreadGuess {
  add: LabelId[];
  remove: LabelId[];
  /** The read state the command set, when it set one. */
  unread?: boolean;
}

export type Guesses = Record<ThreadId, ThreadGuess>;

/** The guess that says "this conversation has been read". */
export const READ_GUESS: ThreadGuess = { add: [], remove: [UNREAD], unread: false };

/* -------------------------------------------------------------------------- */
/* Making one                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * The guess a command implies for each thread it names.
 *
 * `rows` is whatever the list currently holds; it is needed only by the two
 * commands whose target is a *set* rather than a delta — `unarchive` and
 * `untrash` carrying a `restore`, which is how undo puts back the exact labels
 * a thread had. Everything else is a delta already and does not care whether
 * the row is loaded.
 *
 * Returns `null` for a calendar command, which has no effect on any list of
 * conversations, and for an empty target set.
 */
export function project(
  command: Command,
  rows: readonly Pick<Thread, "id" | "labelIds" | "unread">[],
): Guesses | null {
  if (!isMailCommand(command)) return null;
  const ids = command.threadIds;
  if (ids.length === 0) return null;

  const guesses: Guesses = {};
  for (const id of ids) {
    const guess = guessFor(command, id, rows);
    if (guess) guesses[id] = guess;
  }
  return Object.keys(guesses).length > 0 ? guesses : null;
}

function guessFor(
  command: MailCommand,
  id: ThreadId,
  rows: readonly Pick<Thread, "id" | "labelIds" | "unread">[],
): ThreadGuess | null {
  switch (command.kind) {
    case "archive":
      return { add: [], remove: [INBOX] };

    /*
     * Trash is the one command with a dedicated Gmail endpoint, and its label
     * effect is still exactly this: the conversation gains TRASH and leaves the
     * inbox.
     *
     * `DRAFT` goes with it, for a row that has one. Trashing a conversation
     * discards the draft it was holding — `commands::drafts` deletes it through
     * `drafts.delete`, because no label delta can — so the row leaves the Drafts
     * mailbox as well, and pressing delete in Drafts has to take the row off
     * screen in the frame the keystroke produced. Only for rows that carry the
     * label: claiming to remove it from every conversation would make an
     * ordinary archive-to-trash look like it had touched a draft.
     */
    case "trash": {
      const row = rows.find((r) => r.id === id);
      const remove = row?.labelIds.includes(DRAFT) ? [INBOX, DRAFT] : [INBOX];
      return { add: [TRASH], remove };
    }

    // Gmail's `!`: the conversation gains SPAM and leaves the inbox. Both
    // halves matter on screen — the second is what takes the row out of the
    // inbox in the frame the keystroke produced, and the first is what puts it
    // in Spam if that is the mailbox being viewed.
    case "reportSpam":
      return { add: [SPAM], remove: [INBOX] };

    // Snooze also applies a per-account `Mach/Snoozed` label, which lives in
    // the store and has no id the frontend can know. Leaving it out of the
    // guess costs nothing on screen — the row is leaving the inbox either way
    // — and is what lets the guess ever agree with the list again.
    case "snooze":
      return { add: [], remove: [INBOX] };

    case "unsnooze":
      return { add: [INBOX], remove: [] };

    case "markRead":
      return command.read
        ? { add: [], remove: [UNREAD], unread: false }
        : { add: [UNREAD], remove: [], unread: true };

    case "star":
      return command.starred
        ? { add: [STARRED], remove: [] }
        : { add: [], remove: [STARRED] };

    case "label":
      return command.add
        ? { add: [command.labelId], remove: [] }
        : { add: [], remove: [command.labelId] };

    /*
     * The three that undo dispatches, and the only three whose target is a set.
     *
     * `restore` carries the labels the thread actually had before the action
     * being taken back — that is what makes undo exact rather than a guess at
     * `INBOX`. Turning it into a delta needs the row's current labels, so a
     * thread that is not loaded falls back to the same thing the backend falls
     * back to when it has no restore state: put `INBOX` on, take `TRASH` (or
     * `SPAM`) off.
     */
    case "unarchive":
    case "notSpam":
    case "untrash": {
      const fallback: ThreadGuess =
        command.kind === "untrash"
          ? { add: [INBOX], remove: [TRASH] }
          : command.kind === "notSpam"
            ? { add: [INBOX], remove: [SPAM] }
            : { add: [INBOX], remove: [] };
      const state = command.restore?.find((s) => s.threadId === id);
      if (!state) return fallback;
      const row = rows.find((r) => r.id === id);
      if (!row) return { ...fallback, unread: state.isUnread };
      return {
        add: state.labelIds.filter((l) => !row.labelIds.includes(l)),
        remove: row.labelIds.filter((l) => !state.labelIds.includes(l)),
        unread: state.isUnread,
      };
    }
  }
}

/* -------------------------------------------------------------------------- */
/* Applying one                                                                */
/* -------------------------------------------------------------------------- */

/**
 * The row as the guess says it now is.
 *
 * Returns the **same object** when the guess changes nothing about it, which is
 * what lets a list of sixty rows re-render as sixty unchanged references.
 */
export function applyGuess<T extends Pick<Thread, "labelIds" | "unread" | "starred">>(
  row: T,
  guess: ThreadGuess,
): T {
  const labelIds = nextLabels(row.labelIds, guess);
  const unread = guess.unread ?? row.unread;
  // `starred` is `labelIds.includes("STARRED")` — see `ipc.ts`, where the row is
  // built. Re-derived only when the guess actually moved that label, so a guess
  // about something else can never restate the star.
  const starred =
    guess.add.includes(STARRED) || guess.remove.includes(STARRED)
      ? labelIds.includes(STARRED)
      : row.starred;
  if (labelIds === row.labelIds && unread === row.unread && starred === row.starred) {
    return row;
  }
  return { ...row, labelIds, unread, starred };
}

/** The label set after the delta, or the original array when nothing moved. */
function nextLabels(labels: LabelId[], guess: ThreadGuess): LabelId[] {
  const drops = guess.remove.some((l) => labels.includes(l));
  const gains = guess.add.filter((l) => !labels.includes(l));
  if (!drops && gains.length === 0) return labels;
  return [...labels.filter((l) => !guess.remove.includes(l)), ...gains];
}

/**
 * Whether the guess is what takes the row out of the mailbox on screen.
 *
 * The membership half of a guess, and the whole of what replaces the old set of
 * hidden ids. Archive removes `INBOX`, so an archived conversation leaves the
 * inbox — and stays in every label it still carries, in All Mail and in search,
 * which a flat set of ids could not express.
 *
 * Deliberately "does the guess *remove* this label" rather than "does the
 * projected label set still contain it". The two agree on every command, and
 * only the first is safe against a mailbox whose membership does not come from
 * the label set: `list_threads` matches Drafts on `messages.is_draft` as well
 * as on `DRAFT`, so a draft that is starred while the Drafts mailbox is open
 * would vanish from it under the wider rule, for a command that had nothing to
 * do with drafts.
 *
 * It is also exactly the condition {@link settledGuesses} retires an absent row
 * on, which is what makes "the row went" and "the list agrees it went" the same
 * sentence rather than two that can drift.
 */
export function leavesMailbox(guess: ThreadGuess, labelId: LabelId): boolean {
  return guess.remove.includes(labelId);
}

/**
 * Which of a command's targets will leave the mailbox on screen.
 *
 * The cursor's question, and the only thing a caller still has to ask before
 * dispatching: rows going away means the cursor has somewhere else to be. It
 * used to be a `hides` flag each caller passed by hand, which said "archive
 * removes rows" as a fact about the command rather than about the mailbox —
 * so archiving from a label the conversation still carries moved the cursor
 * off a row that had not gone anywhere.
 */
export function leavingIds(
  command: Command,
  rows: readonly Pick<Thread, "id" | "labelIds" | "unread">[],
  labelId: LabelId,
): ThreadId[] {
  const guesses = project(command, rows);
  if (!guesses) return [];
  return rows
    .filter((row) => {
      const guess = guesses[row.id];
      return guess !== undefined && leavesMailbox(guess, labelId);
    })
    .map((row) => row.id);
}

/**
 * The ids the guesses say are no longer in `labelId`, or have been read.
 *
 * What the rail's unread badge subtracts. It has to fall with the rows rather
 * than a round trip later — an archive-everything gesture that empties the list
 * while the rail still claims fifty unread is the lie it guards against.
 */
export function suppressedIds(guesses: Guesses, labelId: LabelId): Set<ThreadId> {
  const out = new Set<ThreadId>();
  for (const [id, guess] of Object.entries(guesses)) {
    if (guess.remove.includes(labelId) || guess.unread === false) out.add(Number(id));
  }
  return out;
}

/* -------------------------------------------------------------------------- */
/* Retiring one                                                                */
/* -------------------------------------------------------------------------- */

/**
 * The ids whose guess the loaded list now agrees with.
 *
 * A guess has to stop being one at some point, or a star turned off later — on
 * the phone, in another window, by a filter — would be held out of the row
 * forever by something nobody remembers guessing. The condition is not "enough
 * time has passed" but "the store now says the same thing", which is the only
 * version of it that cannot produce an intermediate frame: while the two
 * disagree the guess is still doing work, and the moment they agree, dropping
 * it changes nothing on screen.
 *
 * Agreement has two shapes, because a guess makes two claims:
 *
 *  * **The row is loaded.** Then it agrees when every added label is on it,
 *    no removed label is, and the read state matches.
 *  * **The row is absent.** Then it agrees only if the guess is what took it
 *    out of this mailbox — `remove` naming the label being viewed. A row absent
 *    for any other reason (it is on page two, the mailbox is mid-load, the list
 *    was emptied by `g i`) keeps its guess, because dropping it would put a
 *    star back out for the length of a round trip over a change that did
 *    happen.
 */
export function settledGuesses(
  rows: readonly Pick<Thread, "id" | "labelIds" | "unread">[],
  guesses: Guesses,
  labelId: LabelId,
): ThreadId[] {
  const ids = Object.keys(guesses);
  // Cheap when nothing is pending, which is almost always.
  if (ids.length === 0) return [];

  const byId = new Map(rows.map((r) => [r.id, r]));
  const settled: ThreadId[] = [];
  for (const key of ids) {
    const id = Number(key);
    const guess = guesses[id]!;
    const row = byId.get(id);
    if (row) {
      if (agrees(row, guess)) settled.push(id);
    } else if (guess.remove.includes(labelId)) {
      settled.push(id);
    }
  }
  return settled;
}

function agrees(row: Pick<Thread, "labelIds" | "unread">, guess: ThreadGuess): boolean {
  if (guess.unread !== undefined && row.unread !== guess.unread) return false;
  if (guess.add.some((l) => !row.labelIds.includes(l))) return false;
  if (guess.remove.some((l) => row.labelIds.includes(l))) return false;
  return true;
}
