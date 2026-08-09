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
 * **`click`** dispatches a real event sequence — pointerdown, mousedown,
 * pointerup, mouseup, click — on the first element matching a CSS selector.
 * `element.click()` alone would miss every handler written against mousedown,
 * of which this app has several (the dialog backdrop, the list-width drag, the
 * calendar's drag-to-create).
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
  /** How many elements match. */
  count(selector: string): number;
  /** The topmost open overlay's accessible name, or null. */
  overlay(): string | null;
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
      let handled = false;
      for (const token of tokens) {
        /*
         * Normalise first. The tokens people write — and the ones the menu
         * carries — say "mod", which `keyEventFromToken` does not know: it
         * reads canonical modifier names, so an unnormalised "mod+2" becomes a
         * bare "2" with no ⌘ held and matches nothing.
         */
        handled = env.keymap.handle(keyEventFromToken(normalizeToken(token, mod))) || handled;
      }
      return { ok: true, verb: "key", binding: request.argument, handled };
    }

    case "click": {
      const matched = env.dom.click(request.argument);
      return matched
        ? { ok: true, verb: "click", selector: request.argument, matched: true }
        : {
            ok: false,
            verb: "click",
            selector: request.argument,
            matched: false,
            error: `nothing matches ${request.argument}`,
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
  };
}

// ===========================================================================
// The real page
// ===========================================================================

/** The event types a click is, in the order a pointing device produces them. */
const CLICK_SEQUENCE = ["pointerdown", "mousedown", "pointerup", "mouseup", "click"] as const;

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
  };
}

function mouseEvent(type: string): Event {
  const init = { bubbles: true, cancelable: true, composed: true };
  // WKWebView has PointerEvent; a test double or an older engine might not, and
  // a MouseEvent is close enough for anything listening for the pointer pair.
  if (type.startsWith("pointer") && typeof PointerEvent !== "undefined") {
    return new PointerEvent(type, { ...init, pointerType: "mouse", isPrimary: true });
  }
  return new MouseEvent(type, { ...init, button: 0, detail: 1 });
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
  return { click: () => false, count: () => 0, overlay: () => null };
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
