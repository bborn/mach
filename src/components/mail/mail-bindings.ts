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

import type { KeyBinding } from "@/lib/keymap";
import { OVERLAY_KEY_FLOOR } from "@/lib/keymap";

/** The two scopes the mail keys are gated on. See `MailMode` for what they mean. */
export interface MailGates {
  /** Mail mode is on screen and no dialog has the keyboard. */
  mail: () => boolean;
  /** …and the keyboard is in the thread list. */
  active: () => boolean;
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
  trash: () => void;
  favorite: () => void;
  undo: () => void;
}

export function mailActionBindings(
  gates: MailGates,
  on: MailActionHandlers,
): KeyBinding[] {
  const { active, mail } = gates;
  return [
    {
      keys: "e",
      group: "Actions",
      description: "Archive",
      when: active,
      handler: on.archive,
    },
    {
      // Gmail's snooze key. Mach shipped `h` — a Superhuman habit — and `b` is
      // what the Gmail hand this app is for actually presses.
      keys: "b",
      group: "Actions",
      description: "Snooze",
      when: active,
      handler: on.openSnooze,
    },
    {
      // The old key, kept and undocumented. Nothing else wants `h`, and taking
      // a working key out from under someone mid-week buys nothing.
      keys: "h",
      when: active,
      handler: on.openSnooze,
    },
    {
      keys: "s",
      group: "Actions",
      description: "Star",
      when: active,
      handler: on.star,
    },
    {
      keys: "#",
      group: "Actions",
      description: "Trash",
      when: active,
      handler: on.trash,
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
       * The key is shared with the event modal, which deletes an event with it
       * above `OVERLAY_KEY_FLOOR`, and this binding is the one that yields: it
       * sits at 0 with the rest of the shell, so the modal wins for as long as
       * it is up. ⌘⌫ means "throw away the thing I am inside", and an open
       * modal is a thing you are inside. There is no registry conflict either:
       * `conflicts()` reports same-priority ties, and a priority is a decision.
       *
       * The composer used to claim it as well, for discard. It does not any
       * more: ⌘⌫ deletes to the start of the line on macOS, and the composer's
       * bindings run while you are typing, so a second press in the editor
       * destroyed the draft. Discard is ⇧⌘⌫ now — `COMPOSER_KEYS.discard`.
       *
       * This binding never had that problem: it is dead while the target is a
       * field, so ⌘⌫ typed into a composer reaches the editor untouched.
       *
       * No confirmation, deliberately. Undo is the safety net, exactly as it is
       * for archive: `trash` has an exact inverse (`untrash`, restoring the
       * labels the thread actually had) and it goes on the same stack ⌘Z reads.
       */
      keys: "mod+backspace",
      when: active,
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
