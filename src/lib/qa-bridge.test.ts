import { describe, expect, it, vi } from "vitest";
import { createKeymap } from "./keymap";
import {
  connectQaBridge,
  describeUi,
  runVerb,
  type QaDom,
  type QaRequest,
  type QaUiSource,
} from "./qa-bridge";

function dom(overrides: Partial<QaDom> = {}): QaDom {
  return {
    click: () => true,
    count: () => 0,
    overlay: () => null,
    focused: () => null,
    ...overrides,
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
  it("has no fourth verb", () => {
    for (const verb of ["eval", "js", "exec", "screenshot", ""]) {
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
