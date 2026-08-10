/**
 * The session pane's keyboard contract, pressed at.
 *
 * These register the pane's bindings against a real keymap next to the app's
 * real ones — the palette's ⌘K at 200, a mail key, a `g i` jump, the shell's
 * Escape — and then press keys. So the assertions are about *routing*, which is
 * the thing the contract is: which of two live surfaces answers, and whether
 * the key survives to reach the emulator afterwards.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { createKeymap, type Keymap } from "@/lib/keymap";
import { sessionBindings, type SessionKeyActions } from "./session-keys";

interface Options {
  meta?: boolean;
  ctrl?: boolean;
  alt?: boolean;
  shift?: boolean;
  /** The emulator's textarea is a typing target, like every real focus here. */
  tagName?: string;
}

function press(key: string, options: Options = {}) {
  const event = {
    key,
    metaKey: options.meta ?? false,
    ctrlKey: options.ctrl ?? false,
    altKey: options.alt ?? false,
    shiftKey: options.shift ?? false,
    target: { tagName: options.tagName ?? "TEXTAREA", isContentEditable: false },
    prevented: false,
    stopped: false,
    preventDefault() {
      event.prevented = true;
    },
    stopPropagation() {
      event.stopped = true;
    },
  };
  return event;
}

/** What the app registers, at the priorities it really uses. */
function app(keymap: Keymap) {
  const fired = {
    palette: vi.fn(),
    slash: vi.fn(),
    mailNext: vi.fn(),
    archive: vi.fn(),
    inbox: vi.fn(),
    shortcuts: vi.fn(),
    preferences: vi.fn(),
    undo: vi.fn(),
    shellEscape: vi.fn(),
    agentTaller: vi.fn(),
  };
  keymap.register({
    keys: "mod+k",
    allowInInput: true,
    priority: 200,
    description: "Search and commands",
    handler: fired.palette,
  });
  keymap.register({ keys: "/", priority: 200, description: "Search mail", handler: fired.slash });
  keymap.register({ keys: "j", description: "Next", handler: fired.mailNext });
  keymap.register({ keys: "e", description: "Archive", handler: fired.archive });
  keymap.register({ keys: "g i", description: "Inbox", handler: fired.inbox });
  keymap.register({ keys: "?", priority: 90, description: "Shortcuts", handler: fired.shortcuts });
  keymap.register({
    keys: "mod+,",
    allowInInput: true,
    priority: 200,
    description: "Preferences",
    handler: fired.preferences,
  });
  keymap.register({ keys: "mod+z", description: "Undo", handler: fired.undo });
  keymap.register({ keys: "escape", description: "Close", handler: fired.shellEscape });
  // The agent drawer's pair, which the pane deliberately outranks while the
  // terminal has focus.
  keymap.register({
    keys: "mod+alt+up",
    allowInInput: true,
    description: "Agent taller",
    handler: fired.agentTaller,
  });
  return fired;
}

describe("the session pane's keyboard contract", () => {
  let keymap: Keymap;
  let fired: ReturnType<typeof app>;
  let pane: {
    live: boolean;
    focused: boolean;
    finished: boolean;
    paletteOpen: boolean;
  };
  let actions: Record<keyof SessionKeyActions, ReturnType<typeof vi.fn<() => void>>>;

  beforeEach(() => {
    keymap = createKeymap("meta");
    fired = app(keymap);
    pane = { live: true, focused: true, finished: false, paletteOpen: false };
    actions = {
      toggleFocus: vi.fn<() => void>(),
      end: vi.fn<() => void>(),
      taller: vi.fn<() => void>(),
      shorter: vi.fn<() => void>(),
    };
    for (const binding of sessionBindings(
      {
        live: () => pane.live,
        focused: () => pane.focused,
        present: () => pane.live || pane.finished,
        finished: () => pane.finished,
        paletteOpen: () => pane.paletteOpen,
      },
      actions,
    )) {
      keymap.register(binding);
    }
  });

  /* --------------------------------------------------- what the process gets */

  it.each([
    ["j", {}],
    ["e", {}],
    ["/", {}],
    ["?", { shift: true }],
    ["Escape", {}],
    ["Tab", {}],
    ["c", { ctrl: true }],
    ["ArrowUp", {}],
    ["g", {}],
  ])("gives %s to the process while the terminal has focus", (key, options) => {
    const event = press(key, options as Options);
    expect(keymap.handle(event)).toBe(true);
    for (const [name, fn] of Object.entries(fired)) {
      expect(fn, `${name} must not answer ${key}`).not.toHaveBeenCalled();
    }
  });

  it("leaves a claimed key alone so the emulator can encode it", () => {
    // The point of `passthrough`: the registry takes the key away from the app
    // and then does nothing to the event, so it still reaches the textarea.
    const event = press("j");
    keymap.handle(event);
    expect(event.prevented).toBe(false);
    expect(event.stopped).toBe(false);
  });

  it("does not let a half-typed sequence reach the app", () => {
    // `g` then `i` is "go to Inbox" everywhere else. Inside a session it is two
    // characters, and neither may start the app's sequence.
    expect(keymap.handle(press("g"))).toBe(true);
    expect(keymap.pending()).toBeNull();
    expect(keymap.handle(press("i"))).toBe(true);
    expect(fired.inbox).not.toHaveBeenCalled();
  });

  /* ------------------------------------------------------- what the app keeps */

  it("lets ⌘K reach the palette while the terminal has focus", () => {
    const event = press("k", { meta: true });
    expect(keymap.handle(event)).toBe(true);
    expect(fired.palette).toHaveBeenCalledTimes(1);
    // And the palette's own binding swallows it, so the emulator never sees it.
    expect(event.prevented).toBe(true);
  });

  it("lets every other global ⌘ chord reach the app", () => {
    keymap.handle(press(",", { meta: true }));
    expect(fired.preferences).toHaveBeenCalledTimes(1);
  });

  it("treats the terminal as a typing target, so ⌘Z is text editing", () => {
    // Not a decision this file makes: the app's undo is registered without
    // `allowInInput` precisely so that ⌘Z means "un-type that" wherever a caret
    // is, and the emulator's textarea is a caret like any other. The pane's own
    // binding never sees it — the registry drops it one step earlier.
    const event = press("z", { meta: true });
    expect(keymap.handle(event)).toBe(false);
    expect(fired.undo).not.toHaveBeenCalled();
  });

  it("leaves a ⌘ chord nothing binds to the webview, which is how paste works", () => {
    const event = press("v", { meta: true });
    expect(keymap.handle(event)).toBe(false);
    expect(event.prevented).toBe(false);
    expect(event.stopped).toBe(false);
  });

  /* ------------------------------------------------------------ getting out */

  it("toggles focus on ⇧⌘T, from inside the terminal and from outside it", () => {
    keymap.handle(press("t", { meta: true, shift: true }));
    expect(actions.toggleFocus).toHaveBeenCalledTimes(1);

    pane.focused = false;
    keymap.handle(press("t", { meta: true, shift: true, tagName: "DIV" }));
    expect(actions.toggleFocus).toHaveBeenCalledTimes(2);
  });

  it("ends the session on ⇧⌘W from inside the terminal", () => {
    keymap.handle(press("w", { meta: true, shift: true }));
    expect(actions.end).toHaveBeenCalledTimes(1);
  });

  it("resizes on ⌥⌘↑ rather than the agent drawer, while the terminal has focus", () => {
    keymap.handle(press("ArrowUp", { meta: true, alt: true }));
    expect(actions.taller).toHaveBeenCalledTimes(1);
    expect(fired.agentTaller).not.toHaveBeenCalled();
  });

  it("gives ⌥⌘↑ back to the agent drawer once the terminal is not focused", () => {
    pane.focused = false;
    keymap.handle(press("ArrowUp", { meta: true, alt: true, tagName: "DIV" }));
    expect(actions.taller).not.toHaveBeenCalled();
    expect(fired.agentTaller).toHaveBeenCalledTimes(1);
  });

  /* ---------------------------------------------------------------- Escape */

  it("never lets Escape close a running session", () => {
    keymap.handle(press("Escape"));
    expect(actions.end).not.toHaveBeenCalled();

    // Not even from outside the pane: a key pressed while reading mail must not
    // kill the process running in the pane below it.
    pane.focused = false;
    keymap.handle(press("Escape", { tagName: "DIV" }));
    expect(actions.end).not.toHaveBeenCalled();
    expect(fired.shellEscape).toHaveBeenCalledTimes(1);
  });

  it("dismisses the pane on Escape once the process has exited", () => {
    pane.live = false;
    pane.focused = false;
    pane.finished = true;
    keymap.handle(press("Escape", { tagName: "DIV" }));
    expect(actions.end).toHaveBeenCalledTimes(1);
    expect(fired.shellEscape).not.toHaveBeenCalled();
  });

  it("leaves Escape to the palette while the palette is open over the pane", () => {
    pane.live = false;
    pane.finished = true;
    pane.paletteOpen = true;
    keymap.handle(press("Escape", { tagName: "INPUT" }));
    expect(actions.end).not.toHaveBeenCalled();
  });

  /* ---------------------------------------------- when the pane is not there */

  it("is entirely inert while the terminal does not have focus", () => {
    pane.focused = false;
    keymap.handle(press("j", { tagName: "DIV" }));
    keymap.handle(press("e", { tagName: "DIV" }));
    expect(fired.mailNext).toHaveBeenCalledTimes(1);
    expect(fired.archive).toHaveBeenCalledTimes(1);
  });

  it("is entirely inert when there is no session", () => {
    pane.live = false;
    pane.focused = false;
    pane.finished = false;
    keymap.handle(press("j", { tagName: "DIV" }));
    keymap.handle(press("t", { meta: true, shift: true, tagName: "DIV" }));
    expect(fired.mailNext).toHaveBeenCalledTimes(1);
    expect(actions.toggleFocus).not.toHaveBeenCalled();
  });

  it("reports no tie to the conflict checker", () => {
    // Two bindings answering the same key at the same priority is the thing the
    // dev-mode checker shouts about. The pane's overlap with the agent drawer
    // is a decision, expressed as a priority.
    expect(keymap.conflicts()).toEqual([]);
  });
});
