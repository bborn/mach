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
 *
 * # The calendar half
 *
 * The second half of this file does the same job for events, and it exists
 * because the calendar had none of it. Every calendar command — `rsvp`,
 * `createEvent`, `updateEvent`, `deleteEvent`, `moveEvent` — went out and the
 * grid did not move until Google had answered and the event window had been
 * refetched. Answering "Going" from the right-click menu changed nothing on
 * screen for the length of a round trip, which is the report this was written
 * for.
 *
 * An event is not a label set, so the guess is a different shape: the fields
 * the command already decided, plus a "this is gone" flag for a delete. The
 * three rules are the same ones —
 *
 *  * the guess is made **before** the command is dispatched, in the same tick
 *    as the gesture;
 *  * it has an exact inverse, which is dropping it, and a refusal drops it;
 *  * it retires when the store agrees, never on a clock.
 *
 * A create is the one command with no id to key a guess by, because the row it
 * makes does not exist until it has run. {@link placeholderEvent} answers that
 * with a block drawn from the draft under a negative id, which
 * {@link settledPendingEvents} retires once the real row shows up.
 */

import type { AccountId, CalendarEvent, EventId, LabelId, Thread, ThreadId } from "@/types";
import {
  isMailCommand,
  type Command,
  type EventPatch,
  type MailCommand,
} from "./data";

/** Gmail's system labels this module names directly, as `commands::mail` does. */
export const INBOX = "INBOX";
export const UNREAD = "UNREAD";
export const STARRED = "STARRED";
export const SPAM = "SPAM";
export const TRASH = "TRASH";
export const DRAFT = "DRAFT";

/**
 * The labels that decide which mailbox a conversation is in.
 *
 * A restore state names a whole label set, and turning one into a delta needs
 * to say what came *off* as well as what went on. Without the row there is no
 * way to enumerate the labels it might be carrying, and no need to: only these
 * three move a conversation between mailboxes, so only these three have to be
 * named for the answer to be right about membership.
 */
const MEMBERSHIP: LabelId[] = [INBOX, TRASH, SPAM];

/**
 * The archive, which is not a label at all — see `mailboxes.ts`.
 *
 * Its membership is the absence of the three in `MEMBERSHIP`: a conversation is
 * archived while it is in none of them. The store's definition also names
 * `SENT` and `DRAFT`, and those are absent here for the same reason they never
 * appear in a delta — no command in this app puts either on a conversation, so
 * a rule about them would never fire.
 */
export const ARCHIVE = "ARCHIVE";

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
 * conversations, for `unsubscribe`, which changes no local row at all, and for
 * an empty target set.
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
     * `INBOX`. With no restore at all the fallback is what the backend falls
     * back to: put `INBOX` on, take `TRASH` (or `SPAM`) off.
     */
    case "unarchive":
    case "notSpam":
    case "untrash": {
      const state = command.restore?.find((s) => s.threadId === id);
      if (!state) {
        return command.kind === "untrash"
          ? { add: [INBOX], remove: [TRASH] }
          : command.kind === "notSpam"
            ? { add: [INBOX], remove: [SPAM] }
            : { add: [INBOX], remove: [] };
      }
      /*
       * The label set as the command layer will write it, verbatim.
       *
       * A restore state names the read flag twice — as `isUnread`, and as
       * `UNREAD` inside `labelIds` — and the two come from different columns
       * (`threads.is_unread` and `thread_labels`). Reconciling them here looks
       * tempting and is wrong: `commands::mail` applies both verbatim through
       * `set_thread_state`, so a restore that disagreed with itself produces a
       * refetched row that disagrees with itself in exactly the same way, and a
       * guess that had "corrected" one of them would be the thing that never
       * agreed.
       */
      const target = state.labelIds;

      const row = rows.find((r) => r.id === id);
      if (row) {
        return {
          add: target.filter((l) => !row.labelIds.includes(l)),
          remove: row.labelIds.filter((l) => !target.includes(l)),
          unread: state.isUnread,
        };
      }
      /*
       * No row to diff against, which is the case a ⇧⌘Z is usually in: the
       * action being re-applied took the conversation out of this mailbox, so
       * the list dropped it a refetch ago.
       *
       * This used to fall through to the same `add: [INBOX]` as a command with
       * no restore state at all, and that is a claim about the row rather than
       * a gap in one. A redo whose restore set does *not* contain `INBOX` had
       * the projection saying the conversation was coming back to the inbox
       * while the command it described took it out — measured in the real
       * window as `{"add":["INBOX"]}` for a ⇧⌘Z that re-archived. The guess
       * then agreed with nothing and was never retired.
       *
       * The whole target set is here, so state it: everything it names goes
       * on, and the three labels that decide which mailbox a row is in come off
       * when it does not name them. Adding a label the row already carries is a
       * no-op in {@link applyGuess}, so the over-broad `add` costs nothing, and
       * naming the missing memberships is what lets {@link leavesMailbox} and
       * {@link settledGuesses} answer at all.
       */
      return {
        add: target,
        remove: MEMBERSHIP.filter((l) => !target.includes(l)),
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
  // Archive is the one mailbox whose membership is a *negative*: a conversation
  // is in it while it is filed nowhere else, so nothing can ever "remove
  // ARCHIVE" and the rule above would keep a trashed row on screen for the
  // 600ms until the refetch. What takes a row out of the archive is any command
  // that files it somewhere — see `db::queries::mailbox_clause`, which asks the
  // same question of SQLite.
  if (labelId === ARCHIVE) return guess.add.some((l) => MEMBERSHIP.includes(l));
  return guess.remove.includes(labelId);
}

/**
 * Whether the guess is what puts the row *back* into the mailbox on screen.
 *
 * The mirror of {@link leavesMailbox}, and it exists because the two questions
 * were never symmetric in effect. A guess that takes `INBOX` off a loaded row
 * hides it, and needs nothing but the row. A guess that puts `INBOX` back on a
 * row the list has already dropped has nothing to hide or show: the row is not
 * in the list any more, so the guess lands on nothing and the conversation
 * returns only when `list_threads` next answers. That is the whole of why a
 * ⌘Z felt instant when it came straight after the archive and took most of a
 * second when it came after the refetch — 966ms to the row against a 300ms
 * command, measured in the real window.
 *
 * Deliberately "does the guess *add* this label", for the same reason
 * {@link leavesMailbox} asks about `remove`: it is the claim the command made,
 * rather than a property of the label set it leaves behind.
 */
export function entersMailbox(guess: ThreadGuess, labelId: LabelId): boolean {
  // The mirror of the archive case above: a command that takes the conversation
  // out of everywhere else is what puts it in the archive.
  if (labelId === ARCHIVE) return guess.remove.some((l) => MEMBERSHIP.includes(l));
  return guess.add.includes(labelId);
}

/**
 * The rows a guess says belong in this mailbox again, drawn from memory.
 *
 * `remembered` is the list's own copy of rows it has since dropped — see
 * `useMach`, which keeps one for every conversation a command speaks for. A row
 * is redrawn only when four things hold: the loaded list does not already carry
 * it, a guess adds the mailbox's label back, the row is still remembered, and
 * it belongs to the account being shown. Anything else and there is nothing
 * honest to draw.
 *
 * That last one is not hypothetical. A guess outlives a switch of account, and
 * the memory does too, so without it an archive taken back in one mailbox would
 * draw its row into the next mailbox the user opened. `accountId` is `null` for
 * the all-accounts list, which is every row.
 *
 * The result is *not* sorted here; the caller merges it into the list it
 * already has and sorts once. Returns an empty array in the ordinary case,
 * which is every case where nothing has been taken back.
 */
export function returningRows(
  guesses: Guesses,
  remembered: ReadonlyMap<ThreadId, Thread>,
  labelId: LabelId,
  present: readonly Pick<Thread, "id">[],
  accountId: AccountId | null,
): Thread[] {
  const ids = Object.keys(guesses);
  if (ids.length === 0 || remembered.size === 0) return [];
  const loaded = new Set(present.map((row) => row.id));
  const out: Thread[] = [];
  for (const key of ids) {
    const id = Number(key);
    if (loaded.has(id)) continue;
    const guess = guesses[id]!;
    if (!entersMailbox(guess, labelId)) continue;
    const row = remembered.get(id);
    if (!row) continue;
    if (accountId !== null && row.accountId !== accountId) continue;
    out.push(applyGuess(row, guess));
  }
  return out;
}

/**
 * `list_threads`' own order: newest first, ties broken by id.
 *
 * The same comparator the fixture source sorts with — see `byRecency` in
 * `data.ts`, which cannot be shared without `data` and this module importing
 * each other.
 */
export function byRecency(
  a: Pick<Thread, "id" | "timestamp">,
  b: Pick<Thread, "id" | "timestamp">,
): number {
  return b.timestamp - a.timestamp || b.id - a.id;
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
 *
 * Both shapes have the same precondition, and leaving it out is what the
 * reported lag's second half was. **The list has to have been fetched since the
 * guess was made.** The one on screen is always a copy from some point in the
 * past, and a copy taken before the command ran cannot be evidence about it —
 * it agrees, when it agrees, because it has not heard yet. A ⌘Z pressed while
 * the archive it takes back was still going out made `{ add: ["INBOX"] }`
 * against a list that still had the row in the inbox, which agreed instantly;
 * the guess was retired ~160ms later and the row it had just put back vanished
 * again when the archive's own refetch landed. Measured in the real window
 * against a 3s command layer: back at 3168ms, retired at 3270ms, gone at
 * 3820ms.
 *
 * `listVersion` counts the times `list_threads` has actually changed the list,
 * and `guessedAt` is the count each guess was made at. Equal means nothing has
 * arrived since, so there is nothing to decide on yet.
 */
export function settledGuesses(
  rows: readonly Pick<Thread, "id" | "labelIds" | "unread">[],
  guesses: Guesses,
  labelId: LabelId,
  guessedAt: Readonly<Record<ThreadId, number>> = {},
  listVersion = -1,
): ThreadId[] {
  const ids = Object.keys(guesses);
  // Cheap when nothing is pending, which is almost always.
  if (ids.length === 0) return [];

  const byId = new Map(rows.map((r) => [r.id, r]));
  const settled: ThreadId[] = [];
  for (const key of ids) {
    const id = Number(key);
    if (guessedAt[id] === listVersion) continue;
    const guess = guesses[id]!;
    const row = byId.get(id);
    if (row) {
      if (agrees(row, guess)) settled.push(id);
    } else if (leavesMailbox(guess, labelId)) {
      // Through `leavesMailbox` and not the inlined `remove.includes` it used
      // to be, so "the row went" and "the list agrees it went" stay one
      // sentence. They had already parted company for the archive, whose
      // membership no `remove` can describe: a guess for a row trashed out of
      // Archive matched neither branch and would never have retired.
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

/* ========================================================================== */
/* The calendar half                                                          */
/* ========================================================================== */

/** The fields of an event a command can decide without asking Google. */
export type EventFields = Partial<
  Pick<
    CalendarEvent,
    | "start"
    | "end"
    | "allDay"
    | "title"
    | "location"
    | "description"
    | "attendees"
    | "recurrence"
    | "rsvp"
    | "calendarId"
    | "accountId"
  >
>;

/**
 * One event's worth of "this has happened, the store just does not say so yet".
 *
 * Fields rather than a delta, because an event is not a set: `updateEvent`
 * carries the values it is setting, and `rsvp` carries the one answer. There is
 * nothing here for `conferencing` on purpose — a Meet link is minted by Google
 * and read back, so the only honest guess about one is no guess at all.
 */
export interface EventGuess {
  patch?: EventFields;
  /** The event is not there any more. `deleteEvent`, and only that. */
  gone?: boolean;
}

export type EventGuesses = Record<EventId, EventGuess>;

/**
 * A block drawn while the create that made it is still in flight.
 *
 * `realId` is the row id the command layer minted, once it has answered — which
 * is what retires the placeholder: the moment an event with that id is in the
 * store, the store is drawing the same block and this one is a duplicate.
 */
export interface PendingEvent {
  event: CalendarEvent;
  realId: EventId | null;
}

/* -------------------------------------------------------------------------- */
/* Making one                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * The guess a calendar command implies for the event it names.
 *
 * Returns `null` for a mail command, and for `createEvent` — see
 * {@link placeholderEvent} for what stands in for that one.
 */
export function projectEvent(command: Command): EventGuesses | null {
  switch (command.kind) {
    /*
     * The reported case. `event.rsvp` is what the block's fill is derived from
     * (`toneFor` in `TimeGrid` and `MonthGrid`), what the context menu ticks,
     * and what the "show declined" filter reads — so one field carries the
     * whole visible answer to "did it hear me".
     */
    case "rsvp":
      return { [command.eventId]: { patch: { rsvp: command.response } } };

    case "updateEvent": {
      const patch = patchFields(command.patch);
      return patch ? { [command.eventId]: { patch } } : null;
    }

    case "deleteEvent":
      return { [command.eventId]: { gone: true } };

    /*
     * A move is insert-into-destination then delete-from-source, so the row
     * this names really does go away. Claiming `gone` would be the more
     * literal guess and the worse one: the event has not been cancelled, it
     * has changed calendars, and a block that vanishes for a round trip and
     * comes back in another colour reads as a failed drag. Re-pointing it at
     * the destination shows the colour change at once and settles either way.
     */
    case "moveEvent":
      return {
        [command.eventId]: {
          patch: { calendarId: command.calendarId, accountId: command.accountId },
        },
      };

    case "createEvent":
      return null;

    default:
      return null;
  }
}

/** The event fields an `EventPatch` decides, or `null` when it decides none. */
function patchFields(patch: EventPatch): EventFields | null {
  const fields: EventFields = {};
  if (patch.startTs !== undefined) fields.start = patch.startTs;
  if (patch.endTs !== undefined) fields.end = patch.endTs;
  if (patch.isAllDay !== undefined) fields.allDay = patch.isAllDay;
  if (patch.title !== undefined) fields.title = patch.title;
  if (patch.location !== undefined) fields.location = patch.location;
  if (patch.description !== undefined) fields.description = patch.description;
  if (patch.attendees !== undefined) fields.attendees = patch.attendees;
  if (patch.recurrence !== undefined) fields.recurrence = patch.recurrence;
  return Object.keys(fields).length > 0 ? fields : null;
}

/**
 * The ids a calendar command's guess is about. Empty for everything else.
 *
 * Read off the field rather than off the `kind`: `createEvent` has no id yet,
 * and neither a mail command nor `unsubscribe` names an event at all, so the
 * one question worth asking is whether the command carries one.
 */
export function guessedEventIds(command: Command): EventId[] {
  return "eventId" in command ? [command.eventId] : [];
}

/*
 * Placeholder ids count down from -1.
 *
 * Real ids are SQLite `INTEGER PRIMARY KEY` row ids and start at 1, so a
 * negative id can never collide with one and {@link isPendingEvent} is a total
 * answer rather than a lookup. Everything that writes to Google checks it: a
 * placeholder has no id Google knows, so an edit to one has nowhere to go.
 */
let nextPendingId = -1;

export function pendingEventId(): EventId {
  return nextPendingId--;
}

export function isPendingEvent(id: EventId): boolean {
  return id < 0;
}

/** The block to draw for a create, from the draft the create carries. */
export function placeholderEvent(
  command: Extract<Command, { kind: "createEvent" }>,
  id: EventId,
): CalendarEvent {
  const draft = command.draft;
  return {
    id,
    calendarId: command.calendarId,
    accountId: command.accountId,
    title: draft.title,
    start: draft.startTs,
    end: draft.endTs,
    allDay: draft.isAllDay,
    location: draft.location,
    description: draft.description,
    attendees: draft.attendees,
    recurrence: draft.recurrence.length > 0 ? draft.recurrence : undefined,
  };
}

/* -------------------------------------------------------------------------- */
/* Applying one                                                                */
/* -------------------------------------------------------------------------- */

/**
 * The event as the guess says it now is.
 *
 * Returns the **same object** when the guess changes nothing, so a grid of
 * eighty blocks re-renders as eighty unchanged references.
 *
 * An answered `rsvp` also rewrites the signed-in account's own row in `guests`,
 * which is the list the detail panel draws its chips from. It is derived here
 * rather than carried in the guess for the same reason `starred` is in
 * {@link applyGuess}: it is not an independent fact, it is the same answer read
 * a second way, and a guess that stated both could state them differently.
 */
export function applyEventGuess(event: CalendarEvent, guess: EventGuess): CalendarEvent {
  const patch = guess.patch;
  if (!patch) return event;
  const keys = Object.keys(patch) as (keyof EventFields)[];
  if (keys.every((key) => sameValue(event[key], patch[key]))) return event;
  const next: CalendarEvent = { ...event, ...patch };
  if (patch.rsvp !== undefined && event.guests) {
    next.guests = event.guests.map((guest) =>
      guest.isSelf ? { ...guest, response: patch.rsvp } : guest,
    );
  }
  return next;
}

/** Shallow equality, one array deep — enough for `attendees` and `recurrence`. */
function sameValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((item, i) => shallowEqual(item, b[i]));
  }
  return false;
}

function shallowEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
  const keys = Object.keys(left);
  if (keys.length !== Object.keys(right).length) return false;
  return keys.every((key) => left[key] === right[key]);
}

/**
 * The events as the outstanding guesses leave them, placeholders included.
 *
 * The one place a guess becomes a row, so the grid, the cursor, the context
 * menu and the detail panel are all looking at the same events. Returns the
 * **same array** when there is nothing pending, which is almost always.
 */
export function applyEventGuesses(
  events: CalendarEvent[],
  guesses: EventGuesses,
  pending: readonly PendingEvent[],
): CalendarEvent[] {
  if (Object.keys(guesses).length === 0 && pending.length === 0) return events;
  const rows: CalendarEvent[] = [];
  for (const event of events) {
    const guess = guesses[event.id];
    if (!guess) {
      rows.push(event);
      continue;
    }
    if (guess.gone) continue;
    rows.push(applyEventGuess(event, guess));
  }
  /*
   * A placeholder is skipped the moment its real row is in the window, rather
   * than waiting for the effect that retires it from state.
   *
   * Those are one render apart, and one render is enough: the retirement effect
   * runs *after* the commit, so drawing both here would put a frame on screen
   * with two copies of the event that was just created. Deciding it in the same
   * pass makes the duplicate impossible rather than brief.
   */
  for (const item of pending) {
    if (!hasLanded(events, item)) rows.push(item.event);
  }
  return rows;
}

/* -------------------------------------------------------------------------- */
/* Retiring one                                                                */
/* -------------------------------------------------------------------------- */

/**
 * The ids whose guess the loaded events now agree with.
 *
 * Same rule as {@link settledGuesses}, and the same two shapes:
 *
 *  * **The event is loaded.** It agrees when every guessed field matches — and
 *    a `gone` guess never agrees with a row that is still there, because the
 *    delete has plainly not landed yet.
 *  * **The event is absent.** Then it agrees outright. A delete that landed is
 *    the obvious case; so is the source row of a move, which the command layer
 *    removes. The one thing it must be asked about is the *whole* loaded
 *    window rather than the visible subset — hiding a calendar in the sidebar
 *    takes rows off screen without anything having happened to them, and
 *    retiring on that would drop a guess that is still doing work.
 */
export function settledEventGuesses(
  events: readonly CalendarEvent[],
  guesses: EventGuesses,
): EventId[] {
  const keys = Object.keys(guesses);
  if (keys.length === 0) return [];

  const byId = new Map(events.map((event) => [event.id, event] as const));
  const settled: EventId[] = [];
  for (const key of keys) {
    const id = Number(key);
    const event = byId.get(id);
    if (!event) {
      settled.push(id);
      continue;
    }
    const guess = guesses[id]!;
    if (guess.gone) continue;
    if (agreesEvent(event, guess.patch)) settled.push(id);
  }
  return settled;
}

function agreesEvent(event: CalendarEvent, patch: EventFields | undefined): boolean {
  if (!patch) return true;
  return (Object.keys(patch) as (keyof EventFields)[]).every((key) =>
    sameValue(event[key], patch[key]),
  );
}

/**
 * The placeholder ids the store has caught up with.
 *
 * Two ways it can, and both are needed. `realId` is the exact one: the command
 * layer answers with the row id it minted, and the placeholder goes the moment
 * that row is in the window. The field match is the fallback for a source that
 * cannot name an id — the fixture one does not — and for a create whose answer
 * arrived after the refetch that already carried the row.
 */
export function settledPendingEvents(
  events: readonly CalendarEvent[],
  pending: readonly PendingEvent[],
): EventId[] {
  if (pending.length === 0) return [];
  return pending.filter((item) => hasLanded(events, item)).map((item) => item.event.id);
}

/** Whether the row a placeholder was standing in for is in the window now. */
function hasLanded(events: readonly CalendarEvent[], item: PendingEvent): boolean {
  return events.some((event) =>
    item.realId !== null ? event.id === item.realId : sameSlot(event, item.event),
  );
}

/**
 * Whether a stored event is the one a placeholder was standing in for.
 *
 * The four fields the command layer writes to SQLite verbatim, before it says
 * anything to Google. Deliberately not the description or the guest list: both
 * come back normalized, and a placeholder that never matched would sit on the
 * grid as a second copy of an event that did save.
 */
function sameSlot(event: CalendarEvent, placeholder: CalendarEvent): boolean {
  return (
    event.calendarId === placeholder.calendarId &&
    event.start === placeholder.start &&
    event.end === placeholder.end &&
    event.title === placeholder.title
  );
}
