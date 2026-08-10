/**
 * What a selection can be done to, and it depends on which mailbox it is in.
 *
 * The commands have taken a selection since `commandTargets` existed. Nothing
 * on screen said so: six ticked rows produced a header reading "6 selected" and
 * no verb anywhere, so the only way to find out that `e` archives all six was to
 * press `e` on six conversations and see what happened.
 *
 * # Why this is a table and not one row of buttons
 *
 * A fixed row would be wrong in four of the eight mailboxes in the rail, and
 * wrong in the specific way that teaches the user the wrong thing. Archive
 * removes `INBOX`; a draft has never had `INBOX`, so `e` in Drafts is a
 * keystroke that computes an empty diff, sends nothing, and leaves all six rows
 * exactly where they were. Offering it is offering a button that does nothing.
 * The same goes for Archive in Sent, in Archive itself, and in Trash.
 *
 * So the mailbox picks the verbs. What Drafts wants is *discard*, what Trash
 * wants is *restore*, what Spam wants is *not spam* — and each of those is the
 * one thing you actually came to that mailbox to do in bulk.
 *
 * # Every action names a key
 *
 * `keys` is the binding in `mail-bindings.ts` that does the same thing, and
 * `handler` is the entry in {@link MailActionHandlers} both of them go through.
 * That is the whole of the guarantee that the button and the key cannot drift:
 * there is no second path to the command, only two things holding the same
 * handler. The bar draws the key beside the label rather than instead of it, so
 * using the mouse once teaches the keystroke for next time.
 *
 * This file knows nothing about React. `SelectionBar` renders it and
 * `MailMode` gates the keys off the same table, which is what keeps a key live
 * in exactly the mailboxes that offer its button.
 */

import type { LabelId } from "@/types";
import type { MailActionHandlers } from "./mail-bindings";

/** The mailboxes whose selection means something different from the default. */
export const DRAFT = "DRAFT";
export const TRASH = "TRASH";
export const SPAM = "SPAM";
export const ARCHIVE = "ARCHIVE";
export const SENT = "SENT";
export const SNOOZED = "SNOOZED";

/**
 * One verb, not one command.
 *
 * `star` and `read` each cover both directions — the label and the key follow
 * what is selected, because a row of "Star" and "Unstar" side by side is two
 * controls for one axis and the user has to work out which applies.
 */
export type SelectionActionId =
  | "archive"
  | "snooze"
  | "star"
  | "read"
  | "trash"
  | "discard"
  | "putBack";

export interface SelectionAction {
  id: SelectionActionId;
  /** What the button says. A verb, in the imperative, and never a sentence. */
  label: string;
  /** The key that does the same thing, drawn beside the label. */
  keys: string;
  /** The handler the key and the button share. */
  handler: keyof MailActionHandlers;
  tone?: "danger";
  /**
   * Whether one press only asks.
   *
   * True for exactly one action — discarding drafts — and for the reason
   * `ComposerDock` gives for the single-draft case: a discard ends at
   * `drafts.delete` and Gmail does not hand the id back, so there is no inverse
   * for ⌘Z to run. Six drafts must not be easier to destroy than one.
   */
  confirm?: boolean;
}

/** What the selection looks like, as far as a label needs to know. */
export interface SelectionMarks {
  allStarred: boolean;
  anyUnread: boolean;
}

const ARCHIVE_ACTION: SelectionAction = {
  id: "archive",
  label: "Archive",
  keys: "e",
  handler: "archive",
};

const SNOOZE_ACTION: SelectionAction = {
  id: "snooze",
  label: "Snooze",
  keys: "b",
  handler: "openSnooze",
};

const TRASH_ACTION: SelectionAction = {
  id: "trash",
  label: "Trash",
  keys: "#",
  handler: "trash",
  tone: "danger",
};

const DISCARD_ACTION: SelectionAction = {
  id: "discard",
  label: "Discard",
  keys: "#",
  handler: "discard",
  tone: "danger",
  confirm: true,
};

// Matches `starSelected`: a mixed set gets starred, only an all-starred set is
// unstarred. The label has to say which, or the button is a coin toss.
function starAction(marks: SelectionMarks): SelectionAction {
  return {
    id: "star",
    label: marks.allStarred ? "Unstar" : "Star",
    keys: "s",
    handler: "star",
  };
}

// Same rule in the other axis, and Gmail's two keys either side of it.
function readAction(marks: SelectionMarks): SelectionAction {
  return marks.anyUnread
    ? { id: "read", label: "Mark read", keys: "shift+i", handler: "markRead" }
    : { id: "read", label: "Mark unread", keys: "shift+u", handler: "markUnread" };
}

/**
 * What "put it back" is called here.
 *
 * One key and one handler across three mailboxes, because it is one idea —
 * `e` takes a conversation out of where it is, ⇧E is the way back — but three
 * different commands underneath, and three different words for it. `useMach`
 * picks the command; this picks the word. `null` for a mailbox nothing can be
 * put back *from*.
 */
export function putBackLabel(labelId: LabelId): string | null {
  if (labelId === TRASH) return "Restore";
  if (labelId === SPAM) return "Not spam";
  if (labelId === ARCHIVE) return "Move to inbox";
  return null;
}

function putBackAction(labelId: LabelId): SelectionAction | null {
  const label = putBackLabel(labelId);
  return label ? { id: "putBack", label, keys: "shift+e", handler: "putBack" } : null;
}

/**
 * The verbs offered for a selection in `labelId`, in the order they are drawn.
 *
 * Destructive last, always, so the rightmost thing under the pointer is never
 * the one that cannot be taken back by accident.
 */
export function selectionActions(
  labelId: LabelId,
  marks: SelectionMarks,
): SelectionAction[] {
  switch (labelId) {
    /*
     * Drafts is the mailbox the report was filed from, and the only one whose
     * whole answer is a single verb. Archive is a no-op on a thread with no
     * `INBOX`; snooze hides a conversation from an inbox a draft is not in;
     * read and unread are states a draft does not have. Six unsent messages
     * you have decided against want one thing, and offering four dead controls
     * beside it would bury it.
     */
    case DRAFT:
      return [DISCARD_ACTION];

    /*
     * Trash offers the way out and nothing else.
     *
     * "Delete forever" belongs here and is not in the command vocabulary —
     * there is no `Command` for `messages.delete` in
     * `src-tauri/src/commands/types.rs`, and a bulk button is the wrong place
     * to introduce the one write in this app with no inverse at all. Emptying
     * the trash stays Gmail's job until it is a command.
     */
    case TRASH:
      return [putBackAction(TRASH)!];

    // Not spam first, because it is why anyone opens this mailbox; trash for
    // the rest, which is the honest second answer to junk.
    case SPAM:
      return [putBackAction(SPAM)!, TRASH_ACTION];

    case ARCHIVE:
      return [
        putBackAction(ARCHIVE)!,
        starAction(marks),
        readAction(marks),
        TRASH_ACTION,
      ];

    // Nothing here is unread and nothing here is in the inbox, so the two
    // triage verbs have nothing to say. Star and trash do.
    case SENT:
      return [starAction(marks), TRASH_ACTION];

    // Snooze on a snoozed conversation would re-ask a question already
    // answered; archive has no `INBOX` to remove.
    case SNOOZED:
      return [starAction(marks), readAction(marks), TRASH_ACTION];

    // Inbox, Starred, Important and every user label: the triage set.
    default:
      return [
        ARCHIVE_ACTION,
        SNOOZE_ACTION,
        starAction(marks),
        readAction(marks),
        TRASH_ACTION,
      ];
  }
}

/**
 * Whether a mailbox offers this verb at all — the gate its key is registered
 * behind.
 *
 * Asked of the table rather than of a second list of mailbox ids, so a key
 * cannot outlive the button it teaches: `#` stops trashing in Drafts because
 * Drafts stopped offering Trash, in one place.
 */
export function mailboxOffers(labelId: LabelId, id: SelectionActionId): boolean {
  return selectionActions(labelId, { allStarred: false, anyUnread: true }).some(
    (action) => action.id === id,
  );
}
