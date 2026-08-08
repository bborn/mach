import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  createKeymap,
  formatBinding,
  isTypingTarget,
  normalizeToken,
  tokenFromEvent,
  type KeyEventLike,
  type Keymap,
} from "./keymap";

interface FakeOptions {
  meta?: boolean;
  ctrl?: boolean;
  alt?: boolean;
  shift?: boolean;
  tagName?: string;
  contentEditable?: boolean;
}

function press(key: string, options: FakeOptions = {}): KeyEventLike & { prevented: boolean } {
  const event = {
    key,
    metaKey: options.meta ?? false,
    ctrlKey: options.ctrl ?? false,
    altKey: options.alt ?? false,
    shiftKey: options.shift ?? false,
    target: { tagName: options.tagName ?? "DIV", isContentEditable: options.contentEditable ?? false },
    prevented: false,
    preventDefault() {
      event.prevented = true;
    },
    stopPropagation() {},
  };
  return event;
}

describe("tokenFromEvent", () => {
  it("lowercases plain keys", () => {
    expect(tokenFromEvent(press("J"))).toBe("j");
  });

  it("records shift for alphabetic keys only", () => {
    expect(tokenFromEvent(press("A", { shift: true }))).toBe("shift+a");
    expect(tokenFromEvent(press("?", { shift: true }))).toBe("?");
  });

  it("names special keys", () => {
    expect(tokenFromEvent(press("Escape"))).toBe("escape");
    expect(tokenFromEvent(press("ArrowDown"))).toBe("down");
    expect(tokenFromEvent(press(" "))).toBe("space");
  });

  it("orders modifiers deterministically", () => {
    expect(tokenFromEvent(press("k", { meta: true, alt: true }))).toBe("meta+alt+k");
  });

  it("ignores bare modifier presses", () => {
    expect(tokenFromEvent(press("Shift", { shift: true }))).toBeNull();
    expect(tokenFromEvent(press("Meta", { meta: true }))).toBeNull();
  });
});

describe("normalizeToken", () => {
  it("resolves mod per platform", () => {
    expect(normalizeToken("mod+k", "meta")).toBe("meta+k");
    expect(normalizeToken("mod+k", "ctrl")).toBe("ctrl+k");
  });

  it("accepts aliases", () => {
    expect(normalizeToken("Cmd+Enter", "meta")).toBe("meta+enter");
    expect(normalizeToken("esc", "meta")).toBe("escape");
  });
});

describe("isTypingTarget", () => {
  it("is true for form fields and contenteditable", () => {
    expect(isTypingTarget({ tagName: "INPUT" })).toBe(true);
    expect(isTypingTarget({ tagName: "textarea" })).toBe(true);
    expect(isTypingTarget({ isContentEditable: true })).toBe(true);
  });

  it("is false for everything else", () => {
    expect(isTypingTarget({ tagName: "DIV" })).toBe(false);
    expect(isTypingTarget(null)).toBe(false);
  });
});

describe("keymap dispatcher", () => {
  let keymap: Keymap;

  beforeEach(() => {
    keymap = createKeymap("meta");
  });

  it("fires the matching handler and nothing else", () => {
    const j = vi.fn();
    const k = vi.fn();
    keymap.register({ keys: "j", handler: j });
    keymap.register({ keys: "k", handler: k });

    expect(keymap.handle(press("j"))).toBe(true);
    expect(j).toHaveBeenCalledTimes(1);
    expect(k).not.toHaveBeenCalled();
  });

  it("prevents the default when it handles a key", () => {
    keymap.register({ keys: "e", handler: () => {} });
    const event = press("e");
    keymap.handle(event);
    expect(event.prevented).toBe(true);
  });

  it("leaves unbound keys alone", () => {
    keymap.register({ keys: "e", handler: () => {} });
    const event = press("q");
    expect(keymap.handle(event)).toBe(false);
    expect(event.prevented).toBe(false);
  });

  it("stops firing once a binding is unregistered", () => {
    const handler = vi.fn();
    const off = keymap.register({ keys: "j", handler });
    off();
    expect(keymap.handle(press("j"))).toBe(false);
    expect(handler).not.toHaveBeenCalled();
  });

  it("respects `when` gates", () => {
    const mail = vi.fn();
    const calendar = vi.fn();
    let mode = "mail";
    keymap.register({ keys: "j", when: () => mode === "mail", handler: mail });
    keymap.register({ keys: "j", when: () => mode === "calendar", handler: calendar });

    keymap.handle(press("j"));
    expect(mail).toHaveBeenCalledTimes(1);

    mode = "calendar";
    keymap.handle(press("j"));
    expect(calendar).toHaveBeenCalledTimes(1);
    expect(mail).toHaveBeenCalledTimes(1);
  });

  it("tries the next candidate when a handler declines by returning false", () => {
    const declining = vi.fn(() => false as const);
    const accepting = vi.fn();
    keymap.register({ keys: "e", handler: accepting });
    keymap.register({ keys: "e", handler: declining, priority: 5 });

    keymap.handle(press("e"));
    expect(declining).toHaveBeenCalledTimes(1);
    expect(accepting).toHaveBeenCalledTimes(1);
  });

  describe("input suppression", () => {
    it("suppresses ordinary bindings while typing", () => {
      const archive = vi.fn();
      keymap.register({ keys: "e", handler: archive });

      const event = press("e", { tagName: "INPUT" });
      expect(keymap.handle(event)).toBe(false);
      expect(archive).not.toHaveBeenCalled();
      expect(event.prevented).toBe(false);
    });

    it("suppresses bindings inside contenteditable too", () => {
      const archive = vi.fn();
      keymap.register({ keys: "e", handler: archive });
      keymap.handle(press("e", { tagName: "DIV", contentEditable: true }));
      expect(archive).not.toHaveBeenCalled();
    });

    it("still fires bindings marked allowInInput", () => {
      const palette = vi.fn();
      keymap.register({ keys: "mod+k", allowInInput: true, handler: palette });
      expect(keymap.handle(press("k", { meta: true, tagName: "INPUT" }))).toBe(true);
      expect(palette).toHaveBeenCalledTimes(1);
    });

    it("does not start a sequence from inside an input", () => {
      const goInbox = vi.fn();
      keymap.register({ keys: "g i", handler: goInbox });
      expect(keymap.handle(press("g", { tagName: "INPUT" }))).toBe(false);
      expect(keymap.pending()).toBeNull();
    });
  });

  describe("sequences", () => {
    it("fires `g i` as a two-key sequence", () => {
      const goInbox = vi.fn();
      keymap.register({ keys: "g i", handler: goInbox });

      expect(keymap.handle(press("g"), 1000)).toBe(true);
      expect(goInbox).not.toHaveBeenCalled();
      expect(keymap.pending()).toBe("g");

      expect(keymap.handle(press("i"), 1100)).toBe(true);
      expect(goInbox).toHaveBeenCalledTimes(1);
      expect(keymap.pending()).toBeNull();
    });

    it("keeps distinct sequences on the same prefix apart", () => {
      const inbox = vi.fn();
      const calendar = vi.fn();
      keymap.register({ keys: "g i", handler: inbox });
      keymap.register({ keys: "g c", handler: calendar });

      keymap.handle(press("g"), 0);
      keymap.handle(press("c"), 50);
      expect(calendar).toHaveBeenCalledTimes(1);
      expect(inbox).not.toHaveBeenCalled();
    });

    it("times the prefix out", () => {
      const goInbox = vi.fn();
      const jump = vi.fn();
      keymap.register({ keys: "g i", handler: goInbox });
      keymap.register({ keys: "i", handler: jump });

      keymap.handle(press("g"), 0);
      keymap.handle(press("i"), 5000);
      expect(goInbox).not.toHaveBeenCalled();
      expect(jump).toHaveBeenCalledTimes(1);
    });

    it("swallows the follow-up key when the sequence misses", () => {
      const goInbox = vi.fn();
      const archive = vi.fn();
      keymap.register({ keys: "g i", handler: goInbox });
      keymap.register({ keys: "e", handler: archive });

      keymap.handle(press("g"), 0);
      expect(keymap.handle(press("e"), 100)).toBe(true);
      expect(archive).not.toHaveBeenCalled();
      expect(goInbox).not.toHaveBeenCalled();
      expect(keymap.pending()).toBeNull();
    });

    it("prefers an exact single-key binding over opening a sequence", () => {
      const goSomewhere = vi.fn();
      const gAlone = vi.fn();
      keymap.register({ keys: "g i", handler: goSomewhere });
      keymap.register({ keys: "g", handler: gAlone });

      keymap.handle(press("g"));
      expect(gAlone).toHaveBeenCalledTimes(1);
      expect(keymap.pending()).toBeNull();
    });
  });

  describe("escape precedence", () => {
    it("gives the highest-priority Escape binding the key", () => {
      const shell = vi.fn();
      const dialog = vi.fn();
      keymap.register({ keys: "escape", handler: shell });
      keymap.register({ keys: "escape", handler: dialog, priority: 100 });

      keymap.handle(press("Escape"));
      expect(dialog).toHaveBeenCalledTimes(1);
      expect(shell).not.toHaveBeenCalled();
    });

    it("falls back to the shell once the dialog unregisters", () => {
      const shell = vi.fn();
      const dialog = vi.fn();
      keymap.register({ keys: "escape", handler: shell });
      const closeDialog = keymap.register({ keys: "escape", handler: dialog, priority: 100 });

      keymap.handle(press("Escape"));
      closeDialog();
      keymap.handle(press("Escape"));

      expect(dialog).toHaveBeenCalledTimes(1);
      expect(shell).toHaveBeenCalledTimes(1);
    });

    it("breaks equal priority in favour of the most recent registration", () => {
      const older = vi.fn();
      const newer = vi.fn();
      keymap.register({ keys: "escape", handler: older });
      keymap.register({ keys: "escape", handler: newer });

      keymap.handle(press("Escape"));
      expect(newer).toHaveBeenCalledTimes(1);
      expect(older).not.toHaveBeenCalled();
    });

    it("works from inside an input when allowed", () => {
      const close = vi.fn();
      keymap.register({ keys: "escape", allowInInput: true, priority: 100, handler: close });
      expect(keymap.handle(press("Escape", { tagName: "INPUT" }))).toBe(true);
      expect(close).toHaveBeenCalledTimes(1);
    });
  });

  it("enumerates its live bindings in precedence order", () => {
    keymap.register({ keys: "j", description: "Next", handler: () => {} });
    keymap.register({ keys: "escape", description: "Close", priority: 100, handler: () => {} });
    keymap.register({ keys: "x", description: "Hidden", when: () => false, handler: () => {} });

    const active = keymap.active();
    expect(active.map((b) => b.description)).toEqual(["Close", "Next"]);
  });
});

describe("formatBinding", () => {
  it("renders modifiers as glyphs", () => {
    expect(formatBinding("mod+k", "meta")).toBe("⌘K");
    expect(formatBinding("mod+k", "ctrl")).toBe("⌃K");
  });

  it("renders sequences readably", () => {
    expect(formatBinding("g i", "meta")).toBe("G then I");
  });

  it("renders named keys as their glyph", () => {
    expect(formatBinding("enter", "meta")).toBe("↩");
    expect(formatBinding("escape", "meta")).toBe("Esc");
  });
});

// ---------------------------------------------------------------------------
// conflicts
// ---------------------------------------------------------------------------

describe("conflicts", () => {
  it("reports two live bindings claiming the same key", () => {
    const k = createKeymap("meta");
    k.register({ keys: "mod+1", description: "Mail", handler: () => {} });
    k.register({ keys: "mod+1", description: "Toggle calendar 1", handler: () => {} });

    const conflicts = k.conflicts();
    expect(conflicts).toHaveLength(1);
    // Normalised to the platform key — "mod" resolves at registration.
    expect(conflicts[0]!.keys).toBe("meta+1");
    // Winner first — ties go to whoever registered last, which is exactly why
    // this is worth reporting rather than leaving to discover in use.
    expect(conflicts[0]!.bindings[0]!.description).toBe("Toggle calendar 1");
  });

  it("does not report bindings that cannot fire at the same time", () => {
    const k = createKeymap("meta");
    let mode = "mail";
    k.register({ keys: "j", description: "Next thread", when: () => mode === "mail", handler: () => {} });
    k.register({ keys: "j", description: "Next day", when: () => mode === "calendar", handler: () => {} });

    expect(k.conflicts()).toHaveLength(0);
    mode = "calendar";
    expect(k.conflicts()).toHaveLength(0);
  });

  it("is quiet when nothing collides", () => {
    const k = createKeymap("meta");
    k.register({ keys: "e", description: "Archive", handler: () => {} });
    k.register({ keys: "r", description: "Reply", handler: () => {} });
    expect(k.conflicts()).toEqual([]);
  });
});

describe("alt + digits on a Mac layout", () => {
  it("matches alt+1 even though the layout produced ¡", () => {
    // Option remaps the number row on macOS (¡™£¢∞), so reading event.key
    // alone made every alt+<digit> binding unreachable. The physical key is
    // the honest source.
    const k = createKeymap("meta");
    let fired = false;
    k.register({ keys: "alt+1", handler: () => { fired = true; } });

    k.handle({ key: "¡", code: "Digit1", metaKey: false, ctrlKey: false, altKey: true, shiftKey: false });
    expect(fired).toBe(true);
  });

  it("leaves letters alone — only digits are re-read", () => {
    const k = createKeymap("meta");
    let fired = false;
    k.register({ keys: "alt+c", handler: () => { fired = true; } });

    k.handle({ key: "ç", code: "KeyC", metaKey: false, ctrlKey: false, altKey: true, shiftKey: false });
    // "ç" is what the layout produced and what a binding would have to name;
    // this documents that we did not silently change letter behaviour.
    expect(fired).toBe(false);
  });
});

describe("the wildcard", () => {
  it("swallows any key, so a modal surface can block what is behind it", () => {
    const k = createKeymap("meta");
    let leaked = false;
    let swallowed = 0;
    k.register({ keys: "j", handler: () => { leaked = true; } });
    k.register({ keys: "*", priority: 119, handler: () => { swallowed++; } });

    k.handle({ key: "j", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false });
    expect(leaked).toBe(false);
    expect(swallowed).toBe(1);
  });

  it("does not trap you — a higher-priority Escape still wins", () => {
    const k = createKeymap("meta");
    let closed = false;
    k.register({ keys: "*", priority: 119, handler: () => {} });
    k.register({ keys: "escape", priority: 120, handler: () => { closed = true; } });

    k.handle({ key: "Escape", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false });
    expect(closed).toBe(true);
  });

  it("is inert when its `when` is false", () => {
    const k = createKeymap("meta");
    let fired = false;
    k.register({ keys: "j", handler: () => { fired = true; } });
    k.register({ keys: "*", priority: 119, when: () => false, handler: () => {} });

    k.handle({ key: "j", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false });
    expect(fired).toBe(true);
  });
});
