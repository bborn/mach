/**
 * The action keys, as data.
 *
 * `MailMode` used to declare these inline, which made them unreachable from a
 * test: the only way to find out whether ⌘⌫ was registered, or whether it was
 * gated on the list having the keyboard, was to render the whole mail mode —
 * and the test environment here is `node`, with no DOM to render into. So the
 * bindings come out into a function that takes its gates and its handlers and
 * returns descriptors, and `MailMode` spreads the result.
 *
 * This is the same split as `rail-model.tsx`: the component keeps the markup
 * and the model becomes something you can assert about. The declaration order
 * is load-bearing and preserved — the help sheet prints these in the order they
 * are declared, which is the order somebody chose.
 *
 * Nothing here calls into the app. A gate is a function returning a boolean and
 * a handler is a function returning nothing, so a test supplies counters and
 * reads them back.
 */

import type { MachActions } from "@/hooks/useMach";
import type { KeyBinding } from "@/lib/keymap";
import { OVERLAY_KEY_FLOOR } from "@/lib/keymap";
import type { LabelId } from "@/types";
import { mailboxOffers, putBackLabel } from "./selection-actions";

/** The scopes the mail keys are gated on. See `MailMode` for what they mean. */
export interface MailGates {
  /** Mail mode is on screen and no dialog has the keyboard. */
  mail: () => boolean;
  /** …and the keyboard is in the thread list. */
  active: () => boolean;
  /**
   * Which mailbox the list is showing.
   *
   * A third gate rather than a third boolean, because the action keys are no
   * longer the same everywhere: `#` discards in Drafts and trashes elsewhere,
   * ⇧E only means something in a mailbox you can be put back out of. The
   * mailbox decides, and `selection-actions.ts` holds the table it decides
   * from — so a key is live in exactly the mailboxes whose selection bar
   * offers the button for it.
   */
  mailbox: () => LabelId;
}

export interface MailActionHandlers {
  archive: () => void;
  /**
   * Open the snooze picker.
   *
   * Not "snooze": `b` used to commit a snooze outright, to a time nothing on
   * screen ever named. The key opens a choice now, and the choice is what
   * commits.
   */
  openSnooze: () => void;
  star: () => void;
  markRead: () => void;
  markUnread: () => void;
  trash: () => void;
  /** Discard the selected drafts. Asks once; see `discardSelected`. */
  discard: () => void;
  /** Out of Trash, out of Spam, back to the inbox — whichever applies. */
  putBack: () => void;
  favorite: () => void;
  undo: () => void;
}

/**
 * The one place a key and a button are wired to the same command.
 *
 * Both `MailMode`, which registers the keys, and `SelectionBar`, which draws
 * the buttons, build their handlers here. Neither assembles a command of its
 * own, so "the button does what the key does" is not a thing to keep in step —
 * there is one function per verb and two things calling it.
 */
export function mailActionHandlers(actions: MachActions): MailActionHandlers {
  return {
    archive: () => actions.archiveSelected(),
    openSnooze: () => actions.setSnooze(true),
    star: () => actions.starSelected(),
    markRead: () => actions.markReadSelected(true),
    markUnread: () => actions.markReadSelected(false),
    trash: () => actions.trashSelected(),
    discard: () => actions.discardSelected(),
    putBack: () => actions.putBackSelected(),
    favorite: () => actions.toggleFavoriteFocused(),
    undo: () => actions.undo(),
  };
}

export function mailActionBindings(
  gates: MailGates,
  on: MailActionHandlers,
): KeyBinding[] {
  const { active, mail, mailbox } = gates;
  /** Live only where the selection bar draws the matching button. */
  const offers = (id: Parameters<typeof mailboxOffers>[1]) => () =>
    active() && mailboxOffers(mailbox(), id);
  return [
    {
      keys: "e",
      group: "Actions",
      description: "Archive",
      when: offers("archive"),
      handler: on.archive,
    },
    {
      /*
       * ⇧E — the way back out of wherever `e` and `#` put something.
       *
       * One key for three commands, because it is one idea: Restore from
       * Trash, Not spam, Move to inbox. Which one it is depends on the
       * mailbox, `useMach` picks it, and the description follows so the help
       * sheet names the command that is actually live rather than a generic
       * one covering all three. Gmail has no key here, so nothing is being
       * diverged from.
       */
      keys: "shift+e",
      group: "Actions",
      description: putBackLabel(mailbox()) ?? "Move back",
      when: offers("putBack"),
      handler: on.putBack,
    },
    {
      // Gmail's snooze key. Mach shipped `h` — a Superhuman habit — and `b` is
      // what the Gmail hand this app is for actually presses.
      keys: "b",
      group: "Actions",
      description: "Snooze",
      when: offers("snooze"),
      handler: on.openSnooze,
    },
    {
      // The old key, kept and undocumented. Nothing else wants `h`, and taking
      // a working key out from under someone mid-week buys nothing.
      keys: "h",
      when: offers("snooze"),
      handler: on.openSnooze,
    },
    {
      keys: "s",
      group: "Actions",
      description: "Star",
      when: offers("star"),
      handler: on.star,
    },
    {
      // Gmail's two, and the pair `u` already sits between: `u` goes back to
      // the list, ⇧I and ⇧U say what has been read. Both are registered
      // whenever either is offered — the bar draws the one that applies to
      // what is selected, and the other still works.
      keys: "shift+i",
      group: "Actions",
      description: "Mark read",
      when: offers("read"),
      handler: on.markRead,
    },
    {
      keys: "shift+u",
      group: "Actions",
      description: "Mark unread",
      when: offers("read"),
      handler: on.markUnread,
    },
    {
      keys: "#",
      group: "Actions",
      description: "Trash",
      when: offers("trash"),
      handler: on.trash,
    },
    {
      /*
       * `#` in Drafts throws the drafts away instead.
       *
       * Not a second meaning bolted onto Gmail's trash key — the same meaning,
       * which is "get rid of these", spelled the way the mailbox can honour
       * it. Trashing a draft is very nearly a no-op here: `list_threads`
       * matches Drafts on `messages.is_draft` as well as on the `DRAFT` label
       * (see `db/queries.rs`), so a trashed draft thread stays in the list it
       * was trashed from. Gmail's own Drafts toolbar makes the same
       * substitution and calls its bin "Discard drafts".
       *
       * The two bindings are mutually exclusive on the mailbox, so the
       * registry sees no tie — the same arrangement as the two Escapes in
       * `MailMode`.
       *
       * ⌘⌫ is deliberately *not* aliased onto this. It already means "throw
       * away the draft in the conversation I am looking at" — `ComposerDock`
       * registers it at priority 5 — and one key that discards one draft or
       * six depending on what is selected behind a reading pane is a worse
       * offer than one that always means the same thing.
       */
      keys: "#",
      group: "Actions",
      description: "Discard drafts",
      when: offers("discard"),
      handler: on.discard,
    },
    {
      /*
       * ⌘⌫ — the platform's spelling of the key above it.
       *
       * The relationship is exactly the one `z` and ⌘Z already have here: `#`
       * is Gmail's trash key and stays the documented one, because the standing
       * rule is not to make a Gmail hand learn a second vocabulary. ⌘⌫ is what
       * every other Mac app binds to "move this to the Trash" — Finder, Mail,
       * Photos — and a hand that has been trained by the OS for twenty years
       * reaches for it without being told. Registering both costs one entry.
       *
       * It is undocumented for the same reason `h` is: two rows in the help
       * sheet reading "Trash" teaches nobody anything, and the Gmail key is the
       * one this app publishes.
       *
       * `preventDefault` is left at its default of true, which matters more
       * here than for most keys — ⌘⌫ is a Back gesture in some WebViews, and a
       * mail client that navigates away when you delete a message is not one.
       *
       * The key is shared, and this binding is the one that yields. The
       * composer discards a draft with ⌘⌫ (`COMPOSER_KEYS.discard`) and the
       * event modal deletes an event with it; both register at
       * `OVERLAY_KEY_FLOOR`, and this one sits at 0 with the rest of the shell,
       * so a composer or a modal wins for as long as it is up. That is the
       * right way round — ⌘⌫ means "throw away the thing I am inside", and a
       * draft is a thing you are inside. There is no registry conflict either:
       * `conflicts()` reports same-priority ties, and a priority is a decision.
       *
       * No confirmation, deliberately. Undo is the safety net, exactly as it is
       * for archive: `trash` has an exact inverse (`untrash`, restoring the
       * labels the thread actually had) and it goes on the same stack ⌘Z reads.
       *
       * Off in Drafts, where the key above it is off for the same reason:
       * trashing a draft leaves it in Drafts. ⌘⌫ there goes on meaning what
       * `ComposerDock` registers it for — discard the draft in this
       * conversation.
       */
      keys: "mod+backspace",
      when: offers("trash"),
      handler: on.trash,
    },
    {
      /*
       * ⇧F, not F: `f` is forward, and Superhuman's muscle memory outranks a
       * new feature's first choice of key. The mnemonic survives the shift.
       *
       * Gmail spends ⇧F on "forward in a new window", which is not a thing
       * this app has — one window, composer inline — so the key is free and
       * this is a divergence with nothing on the other side of it.
       */
      keys: "shift+f",
      group: "Actions",
      description: "Favorite the conversation or mailbox",
      when: active,
      handler: on.favorite,
    },
    {
      /*
       * Gmail's undo key, and ⌘Z's, are one implementation.
       *
       * Both call `actions.undo()`, which walks the undo stack — so `z` is no
       * longer "take back the thing the status bar is still talking about". It
       * reaches as far back as ⌘Z does. The calendar's `z` is the same call
       * against the same stack, which is why undoing a drag and undoing an
       * archive are the same gesture even though they are different modes.
       *
       * It stays a bare key, and stays mode-scoped, because that is what a
       * Gmail hand presses. ⌘Z is the platform's spelling of it and lives in
       * `App.tsx` where it can be global.
       */
      keys: "z",
      group: "Actions",
      description: "Undo",
      when: mail,
      handler: on.undo,
    },
  ];
}

/* -------------------------------------------------------------------------- */
/* The snooze picker                                                           */
/* -------------------------------------------------------------------------- */

/** How many options a number key will reach. Past this, type the date. */
export const SNOOZE_DIGIT_LIMIT = 9;

export interface SnoozePickerHandlers {
  /** Move the cursor by `delta`, wrapping. */
  move: (delta: number) => void;
  /** Take the option at `index`. Out of range is a no-op the binding declines. */
  pick: (index: number) => void;
  /** Enter: take whatever the cursor is on, or commit the typed date. */
  commit: () => void;
  /** Escape: out of the typed field back to the list, or shut the picker. */
  close: () => void;
}

/**
 * The picker's keys.
 *
 * Everything is at {@link OVERLAY_KEY_FLOOR}, which is what makes them live
 * while the picker's own `Overlay` holds a claim on the keyboard — see
 * `claimKeyboard`. Below that floor nothing in the app answers, so `e` cannot
 * archive the conversation the picker is asking about.
 *
 * `stage` is what splits the two halves of the surface. On the list, digits and
 * arrows move and choose. In the typed field, an `<input>` has focus, so only
 * the bindings marked `allowInInput` are candidates at all — which is exactly
 * Enter and Escape, and is why a `4` typed into "next tuesday 4pm" is a
 * character rather than a fourth option.
 */
export function snoozePickerBindings(
  stage: () => "list" | "custom" | "closed",
  count: () => number,
  on: SnoozePickerHandlers,
): KeyBinding[] {
  const open = () => stage() !== "closed";
  const list = () => stage() === "list";

  const digits = Array.from({ length: SNOOZE_DIGIT_LIMIT }, (_, i) => ({
    keys: String(i + 1),
    priority: OVERLAY_KEY_FLOOR,
    when: list,
    // Declining rather than swallowing: a `7` with four options on screen
    // should do nothing at all, not silently eat the keystroke.
    handler: () => {
      if (i >= count()) return false;
      on.pick(i);
    },
  }));

  return [
    {
      keys: "escape",
      priority: OVERLAY_KEY_FLOOR,
      allowInInput: true,
      when: open,
      handler: on.close,
    },
    {
      keys: "enter",
      priority: OVERLAY_KEY_FLOOR,
      allowInInput: true,
      when: open,
      handler: on.commit,
    },
    { keys: "down", priority: OVERLAY_KEY_FLOOR, when: list, handler: () => on.move(1) },
    { keys: "up", priority: OVERLAY_KEY_FLOOR, when: list, handler: () => on.move(-1) },
    // The Emacs pair the palette also answers to, for the same hands.
    { keys: "ctrl+n", priority: OVERLAY_KEY_FLOOR, when: list, handler: () => on.move(1) },
    { keys: "ctrl+p", priority: OVERLAY_KEY_FLOOR, when: list, handler: () => on.move(-1) },
    { keys: "j", priority: OVERLAY_KEY_FLOOR, when: list, handler: () => on.move(1) },
    { keys: "k", priority: OVERLAY_KEY_FLOOR, when: list, handler: () => on.move(-1) },
    ...digits,
  ];
}
