import { describe, expect, it, vi } from "vitest";
import { createKeymap } from "./keymap";
import {
  connectQaBridge,
  describeUi,
  parseEditingKey,
  runVerb,
  type QaDom,
  type QaEdit,
  type QaRequest,
  type QaUiSource,
} from "./qa-bridge";

function dom(overrides: Partial<QaDom> = {}): QaDom {
  return {
    click: () => true,
    rightClick: () => true,
    count: () => 0,
    overlay: () => null,
    focused: () => null,
    insertText: () => null,
    pressKey: () => null,
    ...overrides,
  };
}

/**
 * A field that behaves the way the real ones do, without a DOM.
 *
 * The tests run in node (see `vitest.config.ts`), so the browser half of
 * `qa-bridge` — `execCommand`, `Selection.modify`, the prototype value setter —
 * has no engine to run against here. What this covers is the half that decides
 * *what* to ask for and reports what came back, which is where the vocabulary
 * lives. The rest is proved by driving a real window; see `scripts/qa`.
 */
function field(initial = "", options: { swallows?: string[] } = {}) {
  let value = initial;
  let caret = initial.length;
  const swallows = new Set(options.swallows ?? []);
  const done = (handled: boolean): QaEdit => ({ value, caret, handled });

  return {
    get value() {
      return value;
    },
    dom: (): QaDom =>
      dom({
        focused: () => ({ tagName: "INPUT", name: "To" }),
        insertText: (text) => {
          value = value.slice(0, caret) + text + value.slice(caret);
          caret += text.length;
          return done(false);
        },
        pressKey: (key) => {
          if (swallows.has(key.key)) return done(true);
          if (key.key === "Backspace" && (key.meta || key.ctrl)) {
            value = value.slice(caret);
            caret = 0;
          } else if (key.key === "Backspace" && caret > 0) {
            value = value.slice(0, caret - 1) + value.slice(caret);
            caret -= 1;
          }
          return done(false);
        },
      }),
  };
}

function ui(overrides: Partial<QaUiSource> = {}): QaUiSource {
  return {
    mode: "mail",
    labelId: "INBOX",
    calendarView: "week",
    threadId: null,
    eventId: null,
    selection: { ids: [] },
    focus: "list",
    paletteOpen: false,
    overlays: 0,
    ...overrides,
  };
}

function env(over: { keymap?: ReturnType<typeof createKeymap>; dom?: QaDom; ui?: QaUiSource } = {}) {
  return {
    keymap: over.keymap ?? createKeymap("meta"),
    dom: over.dom ?? dom(),
    ui: () => over.ui ?? ui(),
    mod: "meta" as const,
  };
}

describe("key", () => {
  it("runs the binding the token names", () => {
    const keymap = createKeymap("meta");
    const handler = vi.fn();
    keymap.register({ keys: "mod+2", handler });

    const result = runVerb({ verb: "key", argument: "mod+2" }, env({ keymap }));

    expect(handler).toHaveBeenCalledTimes(1);
    expect(result).toMatchObject({ ok: true, handled: true });
  });

  /**
   * The bug this is here to stop coming back.
   *
   * `keyEventFromToken` reads canonical modifier names, so an unnormalised
   * "mod+," becomes a bare comma with no ⌘ held and matches nothing at all —
   * silently, which is the worst way for a QA tool to fail. Normalising first
   * is one line and this is what holds it there.
   */
  it("resolves mod for this platform before synthesising the event", () => {
    const keymap = createKeymap("meta");
    const prefs = vi.fn();
    const comma = vi.fn();
    keymap.register({ keys: "mod+,", handler: prefs });
    keymap.register({ keys: ",", handler: comma });

    runVerb({ verb: "key", argument: "mod+," }, env({ keymap }));

    expect(prefs).toHaveBeenCalledTimes(1);
    expect(comma).not.toHaveBeenCalled();
  });

  it("replays a sequence one token at a time", () => {
    const keymap = createKeymap("meta");
    const handler = vi.fn();
    keymap.register({ keys: "g i", handler });

    runVerb({ verb: "key", argument: "g i" }, env({ keymap }));

    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("says so when nothing answered the key", () => {
    const result = runVerb({ verb: "key", argument: "q" }, env());
    expect(result).toMatchObject({ ok: true, handled: false });
  });

  /**
   * The bug this is here to stop coming back.
   *
   * A synthetic event with no target is never "typing", so every binding that
   * exists to stay out of the way of a text field fired through this port and
   * passed. The composer's keys were called fine by a QA run on the same day
   * the person using the app reported that r did not reply and the letters he
   * typed were being read as commands. The port has to press the key where the
   * caret is, or it is testing a state the app is never in.
   */
  it("presses the key at whatever has the caret", () => {
    const keymap = createKeymap("meta");
    const reply = vi.fn();
    keymap.register({ keys: "r", handler: reply });

    const writing = dom({
      focused: () => ({ tagName: "DIV", isContentEditable: true, name: "Message" }),
    });
    const result = runVerb({ verb: "key", argument: "r" }, env({ keymap, dom: writing }));

    expect(reply).not.toHaveBeenCalled();
    expect(result).toMatchObject({ handled: false, focused: "DIV[Message]" });
  });

  it("still runs a binding that asked to stay live in a text field", () => {
    const keymap = createKeymap("meta");
    const send = vi.fn();
    keymap.register({ keys: "mod+enter", allowInInput: true, handler: send });

    const writing = dom({
      focused: () => ({ tagName: "DIV", isContentEditable: true, name: "Message" }),
    });
    runVerb({ verb: "key", argument: "mod+enter" }, env({ keymap, dom: writing }));

    expect(send).toHaveBeenCalledTimes(1);
  });
});

describe("click", () => {
  it("reports the selector matched", () => {
    const seen: string[] = [];
    const result = runVerb(
      { verb: "click", argument: '[data-thread-id="41774"]' },
      env({
        dom: dom({
          click: (selector) => {
            seen.push(selector);
            return true;
          },
        }),
      }),
    );

    expect(seen).toEqual(['[data-thread-id="41774"]']);
    expect(result).toMatchObject({ ok: true, matched: true });
  });

  it("fails rather than passing silently when nothing matched", () => {
    const result = runVerb(
      { verb: "click", argument: ".nope" },
      env({ dom: dom({ click: () => false }) }),
    );
    expect(result).toMatchObject({ ok: false, matched: false });
  });
});

describe("rightclick", () => {
  /**
   * The gap this closes.
   *
   * `click`'s sequence is pointerdown, mousedown, pointerup, mouseup, click,
   * and neither of the app's two context menus is reachable from any of them —
   * both hang off `contextmenu`. Two menus shipped that this harness could not
   * open at all.
   */
  it("goes through the right-click path, not the click one", () => {
    const opened: string[] = [];
    const clicked: string[] = [];
    const result = runVerb(
      { verb: "rightclick", argument: '[data-thread-id="41774"]' },
      env({
        dom: dom({
          click: (selector) => (clicked.push(selector), true),
          rightClick: (selector) => (opened.push(selector), true),
        }),
      }),
    );

    expect(opened).toEqual(['[data-thread-id="41774"]']);
    expect(clicked).toEqual([]);
    expect(result).toMatchObject({ ok: true, verb: "rightclick", matched: true });
  });

  it("fails rather than passing silently when nothing matched", () => {
    const result = runVerb(
      { verb: "rightclick", argument: ".nope" },
      env({ dom: dom({ rightClick: () => false }) }),
    );
    expect(result).toMatchObject({ ok: false, matched: false });
  });
});

describe("type", () => {
  /**
   * The gap this closes.
   *
   * `key` runs a token through the keymap, which answers "what does this
   * *binding* do" and cannot put a character anywhere. Every composer bug in
   * one week — Return, Backspace, ⌘⌫, autolinking, the address typeahead — was
   * in text input, and the port structurally could not reach any of it.
   */
  it("puts the text in and reads back what landed", () => {
    const to = field();
    const result = runVerb(
      { verb: "type", argument: "someone@example.com" },
      env({ dom: to.dom() }),
    );

    expect(to.value).toBe("someone@example.com");
    expect(result).toMatchObject({
      ok: true,
      verb: "type",
      value: "someone@example.com",
      focused: "INPUT[To]",
    });
  });

  it("appends rather than replacing, so a sequence of edits composes", () => {
    const to = field("a@b.com");
    const dual = to.dom();
    runVerb({ verb: "type", argument: ", " }, env({ dom: dual }));
    runVerb({ verb: "type", argument: "c@d.com" }, env({ dom: dual }));
    expect(to.value).toBe("a@b.com, c@d.com");
  });

  /**
   * "Nothing happened" and "there was nothing to happen to" are different
   * findings. A composer that opens without taking the caret is a bug this app
   * has shipped, and a `type` that silently succeeded into nowhere would hide
   * exactly that.
   */
  it("fails when nothing has the caret", () => {
    const result = runVerb({ verb: "type", argument: "hello" }, env());
    expect(result).toMatchObject({ ok: false, verb: "type", focused: null });
    expect(String(result.error)).toContain("caret");
  });
});

describe("press", () => {
  it("performs the edit and reports the field", () => {
    const to = field("hello");
    const result = runVerb({ verb: "press", argument: "Backspace" }, env({ dom: to.dom() }));

    expect(to.value).toBe("hell");
    expect(result).toMatchObject({ ok: true, verb: "press", value: "hell", handled: false });
  });

  it("reads mod as this platform's modifier", () => {
    const to = field("a line of text");
    runVerb({ verb: "press", argument: "mod+Backspace" }, env({ dom: to.dom() }));
    expect(to.value).toBe("");
  });

  /**
   * The app claiming the key is a finding, not a failure. A composer that
   * swallows Return has not lost a line break, it has sent a message — and the
   * harness performing the edit anyway would put the app in a state a real
   * keyboard could never produce.
   */
  it("does not edit when the app called preventDefault, and says so", () => {
    const body = field("draft", { swallows: ["Enter"] });
    const result = runVerb({ verb: "press", argument: "Enter" }, env({ dom: body.dom() }));

    expect(body.value).toBe("draft");
    expect(result).toMatchObject({ ok: true, handled: true });
  });

  it("refuses a key it does not know rather than pressing nothing", () => {
    const result = runVerb({ verb: "press", argument: "Meh" }, env({ dom: field().dom() }));
    expect(result).toMatchObject({ ok: false });
    expect(String(result.error)).toContain("does not know");
  });

  /** A letter is not a press. Sending one here would edit nothing and pass. */
  it("sends you to type for a character", () => {
    const result = runVerb({ verb: "press", argument: "r" }, env({ dom: field().dom() }));
    expect(String(result.error)).toContain("type");
  });

  it("fails when nothing has the caret", () => {
    const result = runVerb({ verb: "press", argument: "Enter" }, env());
    expect(result).toMatchObject({ ok: false, focused: null });
  });
});

describe("parseEditingKey", () => {
  it("resolves mod per platform", () => {
    expect(parseEditingKey("mod+Backspace", "meta")).toMatchObject({ meta: true, ctrl: false });
    expect(parseEditingKey("mod+Backspace", "ctrl")).toMatchObject({ meta: false, ctrl: true });
  });

  it("takes the spellings a person would reach for", () => {
    for (const token of ["Enter", "enter", "Return", "esc", "left", "ArrowLeft"]) {
      expect(parseEditingKey(token, "meta"), token).not.toBeNull();
    }
  });

  /**
   * A dropped modifier turns "⌘⌫ deleted the line" into "Backspace deleted a
   * character" — a passing assertion about the wrong key.
   */
  it("refuses a modifier it does not recognise instead of ignoring it", () => {
    expect(parseEditingKey("hyper+Backspace", "meta")).toBeNull();
  });
});

describe("ui", () => {
  it("reports enough to assert against without reading pixels", () => {
    const report = describeUi(
      ui({ mode: "calendar", threadId: 7, selection: { ids: [7, 8, 9] }, overlays: 1 }),
      dom({
        count: () => 42,
        overlay: () => "Preferences",
        focused: () => ({ tagName: "INPUT", name: "Search" }),
      }),
    );

    expect(report).toEqual({
      mode: "calendar",
      mailbox: "INBOX",
      view: "week",
      thread: 7,
      event: null,
      selection: 3,
      focus: "list",
      palette: false,
      overlays: 1,
      overlay: "Preferences",
      rows: 42,
      focused: "INPUT[Search]",
    });
  });
});

describe("the vocabulary", () => {
  /**
   * The count is not the property; "every verb is a fixed word whose argument
   * is data" is. A selector is matched, text is inserted, a key name is looked
   * up in a table — nothing here evaluates a string, and there is no spelling
   * of "run this".
   */
  it("has no way to ask the window to run code", () => {
    for (const verb of ["eval", "js", "exec", "screenshot", "type ", ""]) {
      expect(runVerb({ verb, argument: "1+1" }, env())).toMatchObject({ ok: false });
    }
  });
});

describe("connectQaBridge", () => {
  function channel() {
    let handler: ((request: QaRequest) => void) | null = null;
    return {
      subscribe: async (h: (request: QaRequest) => void) => {
        handler = h;
        return () => {
          handler = null;
        };
      },
      fire: (request: QaRequest) => handler?.(request),
      live: () => handler !== null,
    };
  }

  it("answers with the id it was asked under", async () => {
    const keymap = createKeymap("meta");
    keymap.register({ keys: "mod+2", handler: () => {} });
    const answers: Record<string, unknown>[] = [];
    const ch = channel();

    connectQaBridge({
      keymap,
      ui: () => ui(),
      dom: dom(),
      subscribe: ch.subscribe,
      respond: (payload) => answers.push(payload),
      mod: "meta",
    });
    await Promise.resolve();

    ch.fire({ id: 17, verb: "key", argument: "mod+2" });
    expect(answers).toEqual([
      { id: 17, ok: true, verb: "key", binding: "mod+2", handled: true, focused: null },
    ]);
  });

  /**
   * A verb that threw and said nothing would leave `scripts/qa` waiting out the
   * Rust timeout and then reporting "the window did not answer" — the wrong
   * diagnosis, and the expensive kind of wrong.
   */
  it("answers even when the verb throws", async () => {
    const answers: Record<string, unknown>[] = [];
    const ch = channel();

    connectQaBridge({
      keymap: createKeymap("meta"),
      ui: () => ui(),
      dom: dom({
        click: () => {
          throw new Error("selector is not valid");
        },
      }),
      subscribe: ch.subscribe,
      respond: (payload) => answers.push(payload),
      mod: "meta",
    });
    await Promise.resolve();

    ch.fire({ id: 3, verb: "click", argument: "((" });
    expect(answers[0]).toMatchObject({ id: 3, ok: false });
  });

  it("stops listening when disconnected", async () => {
    const ch = channel();
    const off = connectQaBridge({
      keymap: createKeymap("meta"),
      ui: () => ui(),
      dom: dom(),
      subscribe: ch.subscribe,
      respond: () => {},
    });
    await Promise.resolve();

    expect(ch.live()).toBe(true);
    off();
    expect(ch.live()).toBe(false);
  });
});
