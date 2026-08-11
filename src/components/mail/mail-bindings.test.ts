/**
 * That the keys are registered, and gated, and reach the right handler.
 *
 * These run against a real `createKeymap`, not against the descriptor array:
 * asserting "there is an object in this list whose `keys` is `mod+backspace`"
 * would pass just as happily if the token were unparseable, if the binding sat
 * below a modal floor, or if two bindings fought over the same key. Registering
 * them and then pressing the key is the only version of this test that can fail
 * for the reasons it is meant to catch.
 *
 * `createKeymap("meta")` pins the platform, so `mod` resolves to ⌘ whatever
 * machine this runs on.
 */

import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { createKeymap, type KeyEventLike, type Keymap } from "@/lib/keymap";
import {
  SNOOZE_DIGIT_LIMIT,
  mailActionBindings,
  snoozePickerBindings,
  type MailActionHandlers,
} from "./mail-bindings";

/** A keydown, with only the fields the dispatcher reads. */
function press(
  key: string,
  modifiers: Partial<Pick<KeyEventLike, "metaKey" | "ctrlKey" | "altKey" | "shiftKey">> = {},
  target?: KeyEventLike["target"],
): KeyEventLike {
  return {
    key,
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...modifiers,
    target,
    preventDefault: () => {},
    stopPropagation: () => {},
  };
}

const TYPING = { tagName: "INPUT" };

describe("the mail action keys", () => {
  let keymap: Keymap;
  let handlers: MailActionHandlers;
  let active: boolean;
  let mail: boolean;

  beforeEach(() => {
    keymap = createKeymap("meta");
    active = true;
    mail = true;
    handlers = {
      archive: vi.fn(),
      openSnooze: vi.fn(),
      star: vi.fn(),
      trash: vi.fn(),
      favorite: vi.fn(),
      undo: vi.fn(),
    };
    for (const binding of mailActionBindings(
      { active: () => active, mail: () => mail },
      handlers,
    )) {
      keymap.register(binding);
    }
  });

  it("archives on e", () => {
    expect(keymap.handle(press("e"))).toBe(true);
    expect(handlers.archive).toHaveBeenCalledTimes(1);
  });

  /* ------------------------------------------------------------- trash --- */

  it("trashes on ⌘⌫ — the key the owner asked for", () => {
    expect(keymap.handle(press("Backspace", { metaKey: true }))).toBe(true);
    expect(handlers.trash).toHaveBeenCalledTimes(1);
  });

  it("still trashes on #, Gmail's key", () => {
    expect(keymap.handle(press("#"))).toBe(true);
    expect(handlers.trash).toHaveBeenCalledTimes(1);
  });

  it("does not trash on a bare backspace", () => {
    // Otherwise every stray delete outside a text field bins a conversation.
    expect(keymap.handle(press("Backspace"))).toBe(false);
    expect(handlers.trash).not.toHaveBeenCalled();
  });

  it("does not trash on ⌥⌫ or ⇧⌫", () => {
    expect(keymap.handle(press("Backspace", { altKey: true }))).toBe(false);
    expect(keymap.handle(press("Backspace", { shiftKey: true }))).toBe(false);
    expect(handlers.trash).not.toHaveBeenCalled();
  });

  it("swallows ⌘⌫ rather than letting the WebView treat it as Back", () => {
    const event = press("Backspace", { metaKey: true });
    const prevented = vi.fn();
    keymap.handle({ ...event, preventDefault: prevented });
    expect(prevented).toHaveBeenCalled();
  });

  it("does not trash while the keyboard is out of the list", () => {
    active = false;
    expect(keymap.handle(press("Backspace", { metaKey: true }))).toBe(false);
    expect(keymap.handle(press("#"))).toBe(false);
    expect(handlers.trash).not.toHaveBeenCalled();
  });

  it("does not trash while a ⌘⌫ is typed into a field", () => {
    expect(keymap.handle(press("Backspace", { metaKey: true }, TYPING))).toBe(false);
    expect(handlers.trash).not.toHaveBeenCalled();
  });

  it("does not trash from behind a dialog", () => {
    const release = keymap.claimKeyboard();
    expect(keymap.handle(press("Backspace", { metaKey: true }))).toBe(false);
    expect(handlers.trash).not.toHaveBeenCalled();
    release();
    expect(keymap.handle(press("Backspace", { metaKey: true }))).toBe(true);
  });

  /* ------------------------------------------------------------ snooze --- */

  it("opens the picker on b rather than snoozing outright", () => {
    expect(keymap.handle(press("b"))).toBe(true);
    expect(handlers.openSnooze).toHaveBeenCalledTimes(1);
  });

  it("opens the picker on the undocumented h as well", () => {
    expect(keymap.handle(press("h"))).toBe(true);
    expect(handlers.openSnooze).toHaveBeenCalledTimes(1);
  });

  it("does not open the picker while the keyboard is out of the list", () => {
    active = false;
    expect(keymap.handle(press("b"))).toBe(false);
    expect(handlers.openSnooze).not.toHaveBeenCalled();
  });

  /* ------------------------------------------------------------- scope --- */

  it("keeps undo alive across the whole mode, not just the list", () => {
    active = false;
    expect(keymap.handle(press("z"))).toBe(true);
    expect(handlers.undo).toHaveBeenCalledTimes(1);
  });

  it("registers no two live bindings that fight over one key", () => {
    expect(keymap.conflicts()).toEqual([]);
  });

  it("yields ⌘⌫ to the overlay in front of it", () => {
    /*
     * ⌘⌫ is spoken for above this binding: the event modal deletes an event
     * with it, at 100, the overlay class. This one sits at 0 with the rest of
     * the shell.
     *
     * That is the whole arbitration, and it is the right way round. An event
     * modal is a thing you are *inside*, and ⌘⌫ means "throw away the thing I
     * am inside" in every Mac app. Trashing the conversation behind it would be
     * answering a question the user did not ask.
     *
     * `conflicts()` reports same-priority ties among live bindings, so this is
     * not a conflict either — an explicit priority is a decision, not an
     * accident of which component mounted last.
     *
     * The composer is no longer one of these: it discards on ⇧⌘⌫, because ⌘⌫
     * is macOS's "delete to the beginning of the line" and the composer's keys
     * are live while you type. See `COMPOSER_KEYS.discard`.
     */
    const modalDelete = vi.fn();
    keymap.register({
      keys: "mod+backspace",
      priority: 100,
      allowInInput: true,
      handler: modalDelete,
    });

    expect(keymap.handle(press("Backspace", { metaKey: true }))).toBe(true);
    expect(modalDelete).toHaveBeenCalledTimes(1);
    expect(handlers.trash).not.toHaveBeenCalled();
    expect(keymap.conflicts()).toEqual([]);
  });

  it("takes ⌘⌫ back the moment the overlay goes away", () => {
    const release = keymap.register({
      keys: "mod+backspace",
      priority: 100,
      allowInInput: true,
      handler: () => {},
    });
    release();
    expect(keymap.handle(press("Backspace", { metaKey: true }))).toBe(true);
    expect(handlers.trash).toHaveBeenCalledTimes(1);
  });

  it("publishes exactly the keys the help sheet should print", () => {
    // `h` and ⌘⌫ are deliberately absent: both are aliases for a key that is
    // already documented, and a sheet with two rows reading "Trash" teaches
    // nobody anything. See the comments in `mail-bindings.ts`.
    const documented = mailActionBindings(
      { active: () => true, mail: () => true },
      handlers,
    )
      .filter((b) => b.description)
      .map((b) => b.keys);
    expect(documented).toEqual(["e", "b", "s", "#", "shift+f", "z"]);
  });
});

describe("the snooze picker keys", () => {
  let keymap: Keymap;
  let stage: "list" | "custom" | "closed";
  let count: number;
  let on: {
    move: Mock<(delta: number) => void>;
    pick: Mock<(index: number) => void>;
    commit: Mock<() => void>;
    close: Mock<() => void>;
  };

  beforeEach(() => {
    keymap = createKeymap("meta");
    stage = "list";
    count = 5;
    on = {
      move: vi.fn<(delta: number) => void>(),
      pick: vi.fn<(index: number) => void>(),
      commit: vi.fn<() => void>(),
      close: vi.fn<() => void>(),
    };
    for (const binding of snoozePickerBindings(
      () => stage,
      () => count,
      on,
    )) {
      keymap.register(binding);
    }
  });

  it("moves on the arrows", () => {
    keymap.handle(press("ArrowDown"));
    keymap.handle(press("ArrowUp"));
    expect(on.move.mock.calls).toEqual([[1], [-1]]);
  });

  it("moves on j/k and on ⌃n/⌃p, as the palette does", () => {
    keymap.handle(press("j"));
    keymap.handle(press("k"));
    keymap.handle(press("n", { ctrlKey: true }));
    keymap.handle(press("p", { ctrlKey: true }));
    expect(on.move.mock.calls).toEqual([[1], [-1], [1], [-1]]);
  });

  it("picks by number, zero-indexed under a one-indexed label", () => {
    keymap.handle(press("1"));
    keymap.handle(press("4"));
    expect(on.pick.mock.calls).toEqual([[0], [3]]);
  });

  it("declines a number past the end of the list rather than eating it", () => {
    count = 3;
    expect(keymap.handle(press("4"))).toBe(false);
    expect(on.pick).not.toHaveBeenCalled();
  });

  it("reaches every option a one-key press can name", () => {
    count = SNOOZE_DIGIT_LIMIT;
    for (let i = 1; i <= SNOOZE_DIGIT_LIMIT; i += 1) {
      expect(keymap.handle(press(String(i)))).toBe(true);
    }
    expect(on.pick).toHaveBeenCalledTimes(SNOOZE_DIGIT_LIMIT);
  });

  it("commits on Enter and closes on Escape", () => {
    keymap.handle(press("Enter"));
    keymap.handle(press("Escape"));
    expect(on.commit).toHaveBeenCalledTimes(1);
    expect(on.close).toHaveBeenCalledTimes(1);
  });

  it("survives the picker's own claim on the keyboard", () => {
    // The Overlay claims at OVERLAY_KEY_FLOOR as it mounts; these bindings are
    // the ones that must stay live through it.
    const release = keymap.claimKeyboard();
    expect(keymap.handle(press("Enter"))).toBe(true);
    expect(keymap.handle(press("Escape"))).toBe(true);
    expect(keymap.handle(press("2"))).toBe(true);
    release();
  });

  describe("with the typed field open", () => {
    beforeEach(() => {
      stage = "custom";
    });

    it("lets Enter and Escape through from inside the input", () => {
      expect(keymap.handle(press("Enter", {}, TYPING))).toBe(true);
      expect(keymap.handle(press("Escape", {}, TYPING))).toBe(true);
      expect(on.commit).toHaveBeenCalledTimes(1);
      expect(on.close).toHaveBeenCalledTimes(1);
    });

    it("leaves digits alone, so a time can be typed", () => {
      // "next tuesday 4pm" must not have its 4 stolen as a fourth option.
      expect(keymap.handle(press("4", {}, TYPING))).toBe(false);
      expect(keymap.handle(press("4"))).toBe(false);
      expect(on.pick).not.toHaveBeenCalled();
    });

    it("leaves j and k alone, so a date can be spelled", () => {
      expect(keymap.handle(press("j", {}, TYPING))).toBe(false);
      expect(on.move).not.toHaveBeenCalled();
    });
  });

  it("goes quiet entirely once the picker is shut", () => {
    stage = "closed";
    for (const key of ["Enter", "Escape", "ArrowDown", "j", "1"]) {
      expect(keymap.handle(press(key))).toBe(false);
    }
    expect(on.commit).not.toHaveBeenCalled();
    expect(on.close).not.toHaveBeenCalled();
    expect(on.move).not.toHaveBeenCalled();
    expect(on.pick).not.toHaveBeenCalled();
  });
});
