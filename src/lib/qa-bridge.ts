/**
 * The window's half of the QA control port.
 *
 * `src-tauri/src/qa/` opens a loopback port for QA instances in development
 * builds and emits `mach://qa/request` with `{ id, verb, argument }`. This
 * turns that into something that actually happens on screen, and emits
 * `mach://qa/response` back with what it saw.
 *
 * # Why the verbs live here and not in Rust
 *
 * Rust owns the socket, so Rust is the half an attacker would reach; it must
 * therefore know as little as possible. It knows three words. The half that
 * knows how to press a key and dispatch a click is this one, which is not
 * reachable from a socket at all — it can only be spoken to by the process it
 * is embedded in. That split is what makes the port safe to open, and it is why
 * there is no `eval`: Rust never receives code, and nothing here evaluates a
 * string.
 *
 * # What each verb is
 *
 * **`key`** takes a token in the syntax `keymap.ts` already parses — `mod+2`,
 * `g i`, `?` — normalises it for this platform, synthesises the event the
 * keymap would have seen, and hands it to the one registry. Not a DOM keydown:
 * the registry is the thing that decides what a key means, and going through it
 * is what makes a QA keystroke and a real one the same code path. A sequence is
 * replayed a token at a time and the keymap's own timer stitches it back
 * together, exactly as the menu bridge does.
 *
 * The event carries whatever currently has DOM focus as its `target`, because
 * half of what the keymap decides is decided from there: `allowInInput` exists
 * precisely so that `r` means "reply" in the list and the letter r inside a
 * composer. A synthetic event with `target: null` is always in the first world
 * and never the second, so every binding tested through this port passed while
 * the same key, pressed by a hand, was being filtered out. Composer keys were
 * reported broken by the one person using the app after a QA run had called
 * them fine — the port could not see the state they break in.
 *
 * **`click`** dispatches a real event sequence — pointerdown, mousedown,
 * pointerup, mouseup, click — on the first element matching a CSS selector.
 * `element.click()` alone would miss every handler written against mousedown,
 * of which this app has several (the dialog backdrop, the list-width drag, the
 * calendar's drag-to-create).
 *
 * **`rightclick`** is that sequence with `contextmenu` in it, and with
 * `button: 2`. The app has two context menus, mail and calendar, and both hang
 * off `contextmenu` — so with only `click` neither could be opened from this
 * port at all, which is a strange gap in a harness whose job is looking at
 * what shipped.
 *
 * **`type`** and **`press`** are the ones that put characters in a field.
 *
 * `key` cannot, and the reason is worth keeping: it hands a synthetic event to
 * the keymap registry, which is the right question for a *binding* and the
 * wrong one for a character. A `KeyboardEvent` the page constructs has
 * `isTrusted: false` and therefore no default action — dispatching one at a
 * composer moves no caret and inserts no letter. `scripts/webqa.ts` says the
 * same thing about its own `keys` and `type`, and reaches for CDP's
 * `Input.insertText`, which goes in below the DOM at the browser's input layer.
 *
 * There is no CDP here, so the edit has to be performed rather than requested:
 * `document.execCommand` for the insert and the delete, `Selection.modify` for
 * caret movement, and a native-setter fallback for `<input>`/`<textarea>` where
 * `execCommand` declines. Everything the app can observe still happens —
 * `beforeinput`, `input`, the value tracker React reads — because the browser
 * is the one making the change.
 *
 * `press` dispatches a real `keydown` on the focused element first, and only
 * performs the edit if nothing called `preventDefault`. That ordering is the
 * app's own: `useKeymap` owns one capture-phase `keydown` listener on `window`,
 * so a dispatched event reaches the keymap exactly as a typed one does, and a
 * composer binding that swallows Return is reported as `handled` instead of
 * silently inserting a paragraph the real app would never have inserted.
 *
 * Both answer with what the field holds afterwards. Reading back what landed is
 * most of the value: "the address typeahead ate the comma" is a sentence you
 * can only write if you can see the field.
 *
 * **`ui`** answers "what is on screen" in terms an assertion can be written
 * against: which mode, which mailbox, where the cursor is, how many rows are
 * selected, whether an overlay is up and which one, how many rows are rendered.
 * The alternative is reading pixels, and a screenshot cannot tell you that a
 * keystroke was ignored.
 *
 * # It answers even when it fails
 *
 * Every path emits a response, including the ones that throw. A verb that
 * quietly did nothing would leave `scripts/qa` waiting out the Rust timeout and
 * then reporting "the window did not answer", which is the wrong diagnosis and
 * the expensive kind of wrong.
 */

import { isTauri } from "./ipc";
import {
  detectModKey,
  normalizeToken,
  type EventTargetLike,
  type Keymap,
  type ModKey,
} from "./keymap";
import { keyEventFromToken } from "./menu";

/** Must match `qa::REQUEST_EVENT`. */
export const QA_REQUEST_EVENT = "mach://qa/request";

/** Must match `qa::RESPONSE_EVENT`. */
export const QA_RESPONSE_EVENT = "mach://qa/response";

export interface QaRequest {
  id: number;
  verb: string;
  argument: string;
}

/**
 * The slice of `useMach`'s state the report is built from.
 *
 * Structural rather than the real `UiState`, which is not exported — and that
 * is the better dependency anyway: this file wants five fields, and saying so
 * keeps a test from having to build a whole application state.
 */
export interface QaUiSource {
  mode: string;
  labelId: string;
  calendarView: string;
  threadId: number | null;
  eventId: number | null;
  selection: { readonly ids: readonly number[] };
  focus: string;
  paletteOpen: boolean;
  overlays: number;
}

/** What `qa ui` reports. */
export interface QaUiReport {
  mode: string;
  /** The mailbox in mail mode; the view in calendar mode. */
  mailbox: string;
  view: string;
  /** The cursor — the thread a command would act on if nothing is selected. */
  thread: number | null;
  event: number | null;
  /** How many rows a command would act on. Zero means "just the cursor". */
  selection: number;
  /** Which mail pane has the keyboard. */
  focus: string;
  palette: boolean;
  /** How many modal surfaces are up. Zero means the app has the keyboard. */
  overlays: number;
  /** The topmost overlay's accessible name, or null when none is open. */
  overlay: string | null;
  /** Thread rows currently rendered. */
  rows: number;
  /**
   * What has DOM focus, as `TAG` or `TAG[name]` — "the caret is here".
   *
   * Distinct from `focus` above, which is the app's own idea of which mail pane
   * the keyboard belongs to. The two disagree exactly when a bug is present:
   * a composer that opened without taking the caret leaves `focus` reading
   * "list" and this reading `BODY`, and every keystroke meant for the message
   * goes to the thread list instead.
   */
  focused: string | null;
}

/**
 * The document, as three questions.
 *
 * An interface rather than a `Document` because the test environment is node
 * with no DOM (see `vitest.config.ts`), and because these three are genuinely
 * all this file needs from a page.
 */
export interface QaDom {
  /** Click the first match. False when the selector matched nothing. */
  click(selector: string): boolean;
  /** Right-click the first match. False when the selector matched nothing. */
  rightClick(selector: string): boolean;
  /** How many elements match. */
  count(selector: string): number;
  /** The topmost open overlay's accessible name, or null. */
  overlay(): string | null;
  /**
   * Whatever has the caret, in the shape the keymap reads a target in.
   *
   * `name` is for the report; `tagName` and `isContentEditable` are what
   * `isTypingTarget` asks, and they are the reason a QA keystroke can be
   * filtered the same way a real one is.
   */
  focused(): (EventTargetLike & { name?: string }) | null;
  /** Insert literal text at the caret. Null when nothing has the caret. */
  insertText(text: string): QaEdit | null;
  /** One editing key at the caret. Null when nothing has the caret. */
  pressKey(key: EditingKey): QaEdit | null;
}

/** What a field holds after an edit — the point of doing the edit. */
export interface QaEdit {
  /** `value` for a field, `textContent` for a contenteditable. */
  value: string | null;
  /** Where the caret ended up, when the element can say. */
  caret: number | null;
  /**
   * True when something in the page called `preventDefault` on the keydown, so
   * no edit was performed. The app handling the key *is* the finding, half the
   * time: a composer that claims Return has not lost a line break, it has sent
   * a message.
   */
  handled: boolean;
  /** Why nothing happened, when nothing happened. */
  error?: string;
}

export interface QaBridgeOptions {
  keymap: Keymap;
  ui: () => QaUiSource;
  /** Defaults to the real document. */
  dom?: QaDom;
  /** Injected in tests; defaults to the Tauri event channel. */
  subscribe?: (handler: (request: QaRequest) => void) => Promise<() => void>;
  respond?: (payload: Record<string, unknown>) => void;
  mod?: ModKey;
}

// ===========================================================================
// Editing keys
// ===========================================================================

/**
 * One key press, resolved from a token.
 *
 * A closed table, not a passthrough. Rust refuses anything outside its six
 * verbs; this is the same idea one level down — the *argument* to `press` is
 * looked up here and anything not in the table is an error with the list in it,
 * rather than a `KeyboardEvent` with an arbitrary `key` that does nothing and
 * reports success. `webqa.ts press` learned the same lesson (`press does not
 * know <name>`).
 */
export interface EditingKey {
  key: string;
  code: string;
  meta: boolean;
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
}

/**
 * The keys that edit, move a caret, or that a field is expected to answer.
 *
 * Letters are not here on purpose: `qa type` is how a letter goes in, and it
 * inserts the whole string in one edit rather than pretending to be sixteen
 * keystrokes. `press` is for the keys that have no character.
 *
 * `Tab` and `Escape` are dispatched and nothing more. Their default action is
 * the browser's own focus navigation and overlay dismissal, which this cannot
 * reproduce and should not try to: if the app moves focus on Tab, `qa ui` will
 * show it moving, and if it does not, that is the finding. Measured in the real
 * window, `press Tab` in the composer's To field leaves the caret where it was
 * — so a `type` after it lands back in To, which is worth knowing before you
 * write a test that assumes otherwise.
 */
export const EDITING_KEYS: Readonly<Record<string, string>> = {
  enter: "Enter",
  return: "Enter",
  backspace: "Backspace",
  delete: "Delete",
  tab: "Tab",
  escape: "Escape",
  esc: "Escape",
  arrowleft: "ArrowLeft",
  arrowright: "ArrowRight",
  arrowup: "ArrowUp",
  arrowdown: "ArrowDown",
  left: "ArrowLeft",
  right: "ArrowRight",
  up: "ArrowUp",
  down: "ArrowDown",
  home: "Home",
  end: "End",
  space: " ",
};

/**
 * `Backspace`, `mod+Backspace`, `shift+ArrowLeft` → an `EditingKey`.
 *
 * `mod` becomes ⌘ on this platform and Ctrl elsewhere, the same word the
 * keymap and the menu use, so a binding and a press are written the same way.
 */
export function parseEditingKey(token: string, mod: ModKey): EditingKey | null {
  const parts = token.trim().split("+").filter(Boolean);
  const name = parts.pop();
  if (!name) return null;

  const key = EDITING_KEYS[name.toLowerCase()];
  if (!key) return null;

  const modifiers = new Set(parts.map((part) => part.toLowerCase()));
  const wantsMod = modifiers.delete("mod");
  const meta = modifiers.delete("meta") || modifiers.delete("cmd") || (wantsMod && mod === "meta");
  const ctrl = modifiers.delete("ctrl") || (wantsMod && mod === "ctrl");
  const shift = modifiers.delete("shift");
  const alt = modifiers.delete("alt") || modifiers.delete("option");
  // An unrecognised modifier is a typo, and a typo that silently drops ⌘ turns
  // "⌘⌫ deleted the line" into "Backspace deleted a character" — a passing test
  // for the wrong key.
  if (modifiers.size > 0) return null;

  return { key, code: key === " " ? "Space" : key, meta, ctrl, shift, alt };
}

/** For an error message that tells you what you could have said instead. */
export function editingKeyNames(): string {
  return [...new Set(Object.values(EDITING_KEYS))].join(", ");
}

// ===========================================================================
// The verbs
// ===========================================================================

/**
 * Run one request and return what to send back.
 *
 * Pure with respect to the transport, so a test can exercise the whole
 * vocabulary without an event channel or a window.
 */
export function runVerb(
  request: { verb: string; argument: string },
  env: { keymap: Keymap; ui: () => QaUiSource; dom: QaDom; mod?: ModKey },
): Record<string, unknown> {
  const mod = env.mod ?? detectModKey();

  switch (request.verb) {
    case "key": {
      const tokens = request.argument.trim().split(/\s+/).filter(Boolean);
      const target = env.dom.focused();
      let handled = false;
      for (const token of tokens) {
        /*
         * Normalise first. The tokens people write — and the ones the menu
         * carries — say "mod", which `keyEventFromToken` does not know: it
         * reads canonical modifier names, so an unnormalised "mod+2" becomes a
         * bare "2" with no ⌘ held and matches nothing.
         */
        const event = { ...keyEventFromToken(normalizeToken(token, mod)), target };
        handled = env.keymap.handle(event) || handled;
      }
      return {
        ok: true,
        verb: "key",
        binding: request.argument,
        handled,
        // Which world the key was pressed in. A binding that "did nothing" and
        // a binding that was filtered because the caret is in a text field are
        // different findings, and this is the difference.
        focused: describeFocus(target),
      };
    }

    case "click":
    case "rightclick": {
      const matched =
        request.verb === "click"
          ? env.dom.click(request.argument)
          : env.dom.rightClick(request.argument);
      return matched
        ? { ok: true, verb: request.verb, selector: request.argument, matched: true }
        : {
            ok: false,
            verb: request.verb,
            selector: request.argument,
            matched: false,
            error: `nothing matches ${request.argument}`,
          };
    }

    case "type": {
      const edit = env.dom.insertText(request.argument);
      if (!edit) return nothingHasTheCaret("type");
      return {
        ok: edit.error === undefined,
        verb: "type",
        text: request.argument,
        focused: describeFocus(env.dom.focused()),
        value: edit.value,
        caret: edit.caret,
        ...(edit.error ? { error: edit.error } : {}),
      };
    }

    case "press": {
      const key = parseEditingKey(request.argument, mod);
      if (!key) {
        return {
          ok: false,
          verb: "press",
          key: request.argument,
          error: `press does not know "${request.argument}" — it knows ${editingKeyNames()}, \
with mod/shift/alt in front. A letter is not a press: use type.`,
        };
      }
      const edit = env.dom.pressKey(key);
      if (!edit) return nothingHasTheCaret("press");
      return {
        ok: edit.error === undefined,
        verb: "press",
        key: request.argument,
        // Whether the app claimed the key rather than letting it edit. Not the
        // same question as `key`'s `handled`, which asks the registry directly.
        handled: edit.handled,
        focused: describeFocus(env.dom.focused()),
        value: edit.value,
        caret: edit.caret,
        ...(edit.error ? { error: edit.error } : {}),
      };
    }

    case "ui":
      return { ok: true, verb: "ui", ...describeUi(env.ui(), env.dom) };

    default:
      // Rust refuses anything outside the vocabulary before it gets here, so
      // this is unreachable in practice — and it is still an error rather than
      // a silence, because the day it is reachable is the day it matters.
      return { ok: false, error: `"${request.verb}" is not a verb this window knows` };
  }
}

/**
 * The one failure both editing verbs share, said the same way each time.
 *
 * "Nothing happened" and "there was nothing to happen to" are different
 * findings, and a composer that opened without taking the caret is a real bug
 * this app has shipped — `ui.focused` reads `BODY` and every keystroke meant
 * for the message goes to the thread list.
 */
function nothingHasTheCaret(verb: string): Record<string, unknown> {
  return {
    ok: false,
    verb,
    focused: null,
    error:
      "nothing has the caret — click into a field first, and check `qa ui` for whether " +
      "the surface you opened took focus at all",
  };
}

export function describeUi(ui: QaUiSource, dom: QaDom): QaUiReport {
  return {
    mode: ui.mode,
    mailbox: ui.labelId,
    view: ui.calendarView,
    thread: ui.threadId,
    event: ui.eventId,
    selection: ui.selection.ids.length,
    focus: ui.focus,
    palette: ui.paletteOpen,
    overlays: ui.overlays,
    overlay: dom.overlay(),
    rows: dom.count("[data-thread-id]"),
    focused: describeFocus(dom.focused()),
  };
}

/** `TAG`, or `TAG[accessible name]` when the element carries one. */
export function describeFocus(
  target: (EventTargetLike & { name?: string }) | null,
): string | null {
  if (!target) return null;
  const tag = (target.tagName ?? "?").toUpperCase();
  return target.name ? `${tag}[${target.name}]` : tag;
}

// ===========================================================================
// The real page
// ===========================================================================

/** The event types a click is, in the order a pointing device produces them. */
const CLICK_SEQUENCE = ["pointerdown", "mousedown", "pointerup", "mouseup", "click"] as const;

/**
 * The right-click, in the order macOS produces it.
 *
 * `contextmenu` fires between the down and the up on every engine, and both of
 * this app's menus open from it. `click` is absent because a secondary button
 * does not produce one, and a menu that opened on `contextmenu` and then closed
 * on a stray `click` would be a harness artefact rather than a finding.
 */
const RIGHT_CLICK_SEQUENCE = ["pointerdown", "mousedown", "contextmenu", "pointerup", "mouseup"] as const;

export function browserDom(doc: Document): QaDom {
  return {
    click(selector) {
      const element = doc.querySelector(selector);
      if (!element) return false;
      (element as HTMLElement).focus?.();
      for (const type of CLICK_SEQUENCE) {
        element.dispatchEvent(mouseEvent(type));
      }
      return true;
    },

    rightClick(selector) {
      const element = doc.querySelector(selector);
      if (!element) return false;
      (element as HTMLElement).focus?.();
      // Anchored on the element's own middle rather than at 0,0. Both menus
      // hang off a `VirtualElement` built from the pointer position, so a
      // right-click reported at the origin would open the menu in the window's
      // top-left corner — on top of the traffic lights, and nowhere near the
      // row it acts on.
      const box = element.getBoundingClientRect?.();
      const at = box
        ? { clientX: Math.round(box.left + box.width / 2), clientY: Math.round(box.top + box.height / 2) }
        : undefined;
      for (const type of RIGHT_CLICK_SEQUENCE) {
        element.dispatchEvent(mouseEvent(type, { button: 2, buttons: 2, ...at }));
      }
      return true;
    },

    insertText(text) {
      const element = editableTarget(doc);
      if (!element) return null;
      const error = insertIntoElement(doc, element, text);
      return { ...readBack(element), handled: false, ...(error ? { error } : {}) };
    },

    pressKey(key) {
      const element = editableTarget(doc);
      if (!element) return null;

      // The keydown goes first and goes in full. `useKeymap` listens on
      // `window` in the capture phase, so this reaches the registry exactly as
      // a typed key does — including `isTypingTarget`, which is the whole
      // reason `r` means reply in the list and the letter r in a composer.
      const notPrevented = element.dispatchEvent(keyboardEvent("keydown", key));
      let error: string | undefined;
      if (notPrevented) {
        error = applyEditingKey(doc, element, key);
      }
      element.dispatchEvent(keyboardEvent("keyup", key));

      return {
        ...readBack(element),
        handled: !notPrevented,
        ...(error ? { error } : {}),
      };
    },

    count(selector) {
      return doc.querySelectorAll(selector).length;
    },

    overlay() {
      // The last one in document order is the topmost: dialogs render in mount
      // order and a dialog opened from a dialog mounts after it.
      const dialogs = doc.querySelectorAll('[role="dialog"]');
      const top = dialogs[dialogs.length - 1];
      if (!top) return null;
      const labelledBy = top.getAttribute("aria-labelledby");
      const named = labelledBy ? doc.getElementById(labelledBy) : null;
      const name = named?.textContent ?? top.getAttribute("aria-label") ?? "";
      return name.trim() || "dialog";
    },

    focused() {
      const element = doc.activeElement as HTMLElement | null;
      // No focus at all reads as `<body>` in every engine. Reporting that as a
      // target would be a lie of a different shape — `isTypingTarget` would say
      // "not typing", which is true, but the caret being nowhere is the finding.
      if (!element || element === doc.body) return null;
      return {
        tagName: element.tagName,
        isContentEditable: element.isContentEditable,
        name: element.getAttribute("aria-label") ?? undefined,
      };
    },
  };
}

function mouseEvent(type: string, options: MouseEventInit = {}): Event {
  const init = { bubbles: true, cancelable: true, composed: true, detail: 1, ...options };
  // WKWebView has PointerEvent; a test double or an older engine might not, and
  // a MouseEvent is close enough for anything listening for the pointer pair.
  if (type.startsWith("pointer") && typeof PointerEvent !== "undefined") {
    return new PointerEvent(type, { ...init, pointerType: "mouse", isPrimary: true });
  }
  return new MouseEvent(type, { button: 0, ...init });
}

function keyboardEvent(type: "keydown" | "keyup", key: EditingKey): KeyboardEvent {
  return new KeyboardEvent(type, {
    key: key.key,
    code: key.code,
    metaKey: key.meta,
    ctrlKey: key.ctrl,
    shiftKey: key.shift,
    altKey: key.alt,
    bubbles: true,
    cancelable: true,
    composed: true,
  });
}

// ===========================================================================
// Performing the edit
// ===========================================================================
//
// A dispatched `KeyboardEvent` has no default action — `isTrusted` is false, so
// the browser observes it and edits nothing. CDP sidesteps that by going in
// underneath the DOM (`Input.insertText`); there is no CDP inside a WKWebView,
// so the edit is performed with the editing APIs instead.
//
// `document.execCommand` is deprecated and is still the only thing that runs
// the browser's own editing pipeline from script: `beforeinput`, the change,
// `input`, and the native value write that React's change tracker is watching
// for. Setting `.value` and firing a synthetic `input` is the usual workaround
// and it is worse — React sees its tracked value unchanged and drops the event,
// which is exactly the class of "the harness said it typed and nothing
// happened" this is here to end. It is the fallback, not the first choice, and
// it goes through the prototype setter so the tracker is bypassed correctly.

type Editable = HTMLElement;

/** Whatever has the caret, or null. `<body>` is nothing having the caret. */
function editableTarget(doc: Document): Editable | null {
  const element = doc.activeElement as HTMLElement | null;
  if (!element || element === doc.body) return null;
  return element;
}

function isField(element: Editable): element is HTMLInputElement | HTMLTextAreaElement {
  const tag = element.tagName;
  return tag === "INPUT" || tag === "TEXTAREA";
}

/** What the element holds now, in whichever way it can say. */
function readBack(element: Editable): { value: string | null; caret: number | null } {
  if (isField(element)) {
    return { value: element.value, caret: element.selectionStart ?? null };
  }
  return { value: element.textContent, caret: null };
}

function insertIntoElement(doc: Document, element: Editable, text: string): string | undefined {
  if (execCommand(doc, "insertText", text)) return undefined;
  if (!isField(element)) {
    return "execCommand('insertText') declined and there is no fallback for a contenteditable";
  }
  const start = element.selectionStart ?? element.value.length;
  const end = element.selectionEnd ?? start;
  setFieldValue(element, element.value.slice(0, start) + text + element.value.slice(end));
  element.setSelectionRange(start + text.length, start + text.length);
  return undefined;
}

/**
 * The default action the browser would have taken, taken by hand.
 *
 * Only the keys that have one. Tab and Escape are here so `press` accepts them
 * — the app answers both itself, and moving focus or closing an overlay is not
 * this function's business.
 */
function applyEditingKey(doc: Document, element: Editable, key: EditingKey): string | undefined {
  const word = key.alt;
  const line = key.meta || key.ctrl;

  switch (key.key) {
    case "Backspace":
      if (line) return deleteTo(doc, element, "backward", "lineboundary");
      if (word) return deleteTo(doc, element, "backward", "word");
      return deleteOne(doc, element, "backward");

    case "Delete":
      if (word) return deleteTo(doc, element, "forward", "word");
      return deleteOne(doc, element, "forward");

    case "Enter":
      // A single-line field's Enter submits a form; there is nothing to insert
      // and inserting a newline would be a lie about what a keyboard does.
      if (element.tagName === "INPUT") return undefined;
      if (execCommand(doc, key.shift ? "insertLineBreak" : "insertParagraph")) return undefined;
      return insertIntoElement(doc, element, "\n");

    case " ":
      return insertIntoElement(doc, element, " ");

    case "ArrowLeft":
      return moveCaret(doc, element, "backward", word ? "word" : "character", key.shift);
    case "ArrowRight":
      return moveCaret(doc, element, "forward", word ? "word" : "character", key.shift);
    case "ArrowUp":
      return moveCaret(doc, element, "backward", "line", key.shift);
    case "ArrowDown":
      return moveCaret(doc, element, "forward", "line", key.shift);
    case "Home":
      return moveCaret(doc, element, "backward", "lineboundary", key.shift);
    case "End":
      return moveCaret(doc, element, "forward", "lineboundary", key.shift);

    default:
      // Tab, Escape: dispatched above, and whatever the app does with them is
      // the answer. Nothing to perform.
      return undefined;
  }
}

function deleteOne(doc: Document, element: Editable, direction: "backward" | "forward"): string | undefined {
  if (execCommand(doc, direction === "backward" ? "delete" : "forwardDelete")) return undefined;
  if (!isField(element)) return "execCommand('delete') declined on a contenteditable";

  const start = element.selectionStart ?? element.value.length;
  const end = element.selectionEnd ?? start;
  if (start !== end) {
    setFieldValue(element, element.value.slice(0, start) + element.value.slice(end));
    element.setSelectionRange(start, start);
    return undefined;
  }
  if (direction === "backward") {
    if (start === 0) return undefined;
    setFieldValue(element, element.value.slice(0, start - 1) + element.value.slice(start));
    element.setSelectionRange(start - 1, start - 1);
  } else {
    setFieldValue(element, element.value.slice(0, start) + element.value.slice(start + 1));
    element.setSelectionRange(start, start);
  }
  return undefined;
}

/** ⌘⌫ and ⌥⌫: extend the selection, then delete what it covers. */
function deleteTo(
  doc: Document,
  element: Editable,
  direction: "backward" | "forward",
  granularity: "word" | "lineboundary",
): string | undefined {
  if (isField(element)) {
    const caret = element.selectionStart ?? element.value.length;
    const end = element.selectionEnd ?? caret;
    // A live selection is deleted as it stands, whatever the modifier said —
    // the same thing a real ⌘⌫ does.
    if (caret !== end) return deleteOne(doc, element, direction);
    const edge = fieldEdge(element.value, caret, direction, granularity);
    const [from, to] = direction === "backward" ? [edge, caret] : [caret, edge];
    setFieldValue(element, element.value.slice(0, from) + element.value.slice(to));
    element.setSelectionRange(from, from);
    return undefined;
  }

  const selection = doc.getSelection?.();
  if (typeof selection?.modify !== "function") {
    return "this engine has no Selection.modify; cannot delete by line or word";
  }
  selection.modify("extend", direction, granularity);
  if (execCommand(doc, "delete")) return undefined;
  return "execCommand('delete') declined";
}

/** Where a word or line boundary is, for a plain field. */
function fieldEdge(
  value: string,
  caret: number,
  direction: "backward" | "forward",
  granularity: "word" | "lineboundary",
): number {
  if (direction === "backward") {
    const before = value.slice(0, caret);
    if (granularity === "lineboundary") return before.lastIndexOf("\n") + 1;
    return before.replace(/\S+\s*$/, "").length;
  }
  const after = value.slice(caret);
  if (granularity === "lineboundary") {
    const newline = after.indexOf("\n");
    return newline === -1 ? value.length : caret + newline;
  }
  return caret + (after.match(/^\s*\S+/)?.[0].length ?? 0);
}

function moveCaret(
  doc: Document,
  element: Editable,
  direction: "backward" | "forward",
  granularity: "character" | "word" | "line" | "lineboundary",
  extend: boolean,
): string | undefined {
  if (isField(element)) {
    const anchor = element.selectionStart ?? 0;
    const head = element.selectionEnd ?? anchor;
    const from = direction === "backward" ? anchor : head;
    const to =
      granularity === "character"
        ? Math.max(0, Math.min(element.value.length, from + (direction === "backward" ? -1 : 1)))
        : fieldEdge(element.value, from, direction, granularity === "word" ? "word" : "lineboundary");
    element.setSelectionRange(extend ? Math.min(anchor, to) : to, extend ? Math.max(head, to) : to);
    return undefined;
  }

  const selection = doc.getSelection?.();
  if (typeof selection?.modify !== "function") {
    return "this engine has no Selection.modify; cannot move the caret";
  }
  selection.modify(extend ? "extend" : "move", direction, granularity);
  return undefined;
}

/** `false` when the engine declined, which is the signal to fall back. */
function execCommand(doc: Document, command: string, value?: string): boolean {
  const run = (doc as Document & { execCommand?(c: string, ui: boolean, v?: string): boolean })
    .execCommand;
  if (typeof run !== "function") return false;
  try {
    return run.call(doc, command, false, value) === true;
  } catch {
    return false;
  }
}

/**
 * Write a field's value the way the browser would, so React notices.
 *
 * React keeps a shadow copy of the last value it saw on the DOM node and drops
 * an `input` event whose value matches it. Assigning `element.value` updates
 * that shadow copy on the way past, so the event React finally receives looks
 * like nothing changed and `onChange` never fires. Going through the prototype
 * setter writes the DOM without touching the tracker, which is the whole trick.
 */
function setFieldValue(element: HTMLInputElement | HTMLTextAreaElement, next: string): void {
  const prototype = element.tagName === "TEXTAREA" ? HTMLTextAreaElement : HTMLInputElement;
  const setter = Object.getOwnPropertyDescriptor(prototype.prototype, "value")?.set;
  if (setter) setter.call(element, next);
  else element.value = next;
  element.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
}

// ===========================================================================
// The transport
// ===========================================================================

/**
 * Wire the bridge until the returned function is called.
 *
 * Inert outside a Tauri development build: `bun run dev` in a browser tab, the
 * tests, and every release build get a no-op, because the only thing that can
 * speak to this is a Rust module that a release build does not contain.
 */
export function connectQaBridge(options: QaBridgeOptions): () => void {
  const injected = options.subscribe !== undefined;
  if (!injected && !(import.meta.env.DEV && isTauri())) return () => {};

  const subscribe = options.subscribe ?? defaultSubscribe;
  const respond = options.respond ?? defaultRespond;
  const dom =
    options.dom ?? (typeof document === "undefined" ? emptyDom() : browserDom(document));

  let cancelled = false;
  let unsubscribe: (() => void) | null = null;

  void subscribe((request) => {
    let payload: Record<string, unknown>;
    try {
      payload = runVerb(request, {
        keymap: options.keymap,
        ui: options.ui,
        dom,
        mod: options.mod,
      });
    } catch (error) {
      payload = { ok: false, error: String(error) };
    }
    respond({ id: request.id, ...payload });
  }).then((off) => {
    if (cancelled) off();
    else unsubscribe = off;
  });

  return () => {
    cancelled = true;
    unsubscribe?.();
  };
}

function emptyDom(): QaDom {
  return {
    click: () => false,
    rightClick: () => false,
    count: () => 0,
    overlay: () => null,
    focused: () => null,
    insertText: () => null,
    pressKey: () => null,
  };
}

async function defaultSubscribe(
  handler: (request: QaRequest) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  const off = await listen<QaRequest>(QA_REQUEST_EVENT, (event) => handler(event.payload));
  return () => void off();
}

function defaultRespond(payload: Record<string, unknown>): void {
  void import("@tauri-apps/api/event").then(({ emit }) => emit(QA_RESPONSE_EVENT, payload));
}
