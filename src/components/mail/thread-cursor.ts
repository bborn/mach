/**
 * Which message in the open conversation the keyboard is on.
 *
 * # Why this is the DOM rather than a piece of state
 *
 * `r`, `a` and `f` live in `ComposerDock`; the messages are drawn by
 * `ReadingPane`; neither is the other's parent. The question they have to agree
 * on — *which message is the reply verb aimed at* — is already answered by the
 * browser, because every message header is a real `<button>` and exactly one
 * element in a document has focus. Adding a `messageId` to the shell's store
 * beside that would be a second answer, and the two would disagree the first
 * time something moved focus without going through it.
 *
 * The same reasoning as `keyboardInComposer` in `lib/compose.ts`: where the
 * keyboard is is a fact about the DOM.
 *
 * # What counts as focused
 *
 * The id lives on the message's `<article>`, so *anything* inside it answers —
 * the header button, the sandboxed body frame, an attachment chip, the menu
 * button. A reader who tabbed into the body of message four and pressed `r`
 * means message four.
 */

/** The id of the message this element belongs to. Carried by the `<article>`. */
export const MESSAGE_ROW = "data-mach-message";

/** The focus stop inside one, which is the header. `n` and `p` move between these. */
export const MESSAGE_CURSOR = "data-mach-message-cursor";

/** A draft row: mirrored into the conversation, and not something to answer. */
export const MESSAGE_DRAFT = "data-mach-message-draft";

type Root = Pick<Document, "querySelectorAll" | "querySelector"> & {
  activeElement?: Element | null;
};

function root(doc?: Root): Root | null {
  if (doc) return doc;
  return typeof document === "undefined" ? null : document;
}

/**
 * The message the keyboard is inside, or null.
 *
 * Null is not a failure: it is the ordinary state of a conversation nobody has
 * stepped into, and it means "the newest message" to every caller — the answer
 * the strip under the thread has always given.
 */
export function focusedMessageId(doc?: Root): number | null {
  const scope = root(doc);
  const active = scope?.activeElement;
  if (!(active instanceof Element)) return null;
  const row = active.closest(`[${MESSAGE_ROW}]`);
  return idOf(row);
}

/** Every message row on screen, in the order the conversation is read. */
export function messageRows(doc?: Root): Element[] {
  const scope = root(doc);
  return scope ? [...scope.querySelectorAll(`[${MESSAGE_ROW}]`)] : [];
}

/**
 * Move the cursor `step` messages and put the keyboard there.
 *
 * With nothing focused it starts from the end rather than the beginning: the
 * newest message is the one already expanded and the one every other reply
 * route means, so `p` from cold steps back from it rather than jumping to the
 * top of a conversation of eleven.
 *
 * It does not wrap. Running off either end of a conversation and silently
 * reappearing at the other is how a reader loses their place in a long one.
 */
export function moveMessageCursor(step: number, doc?: Root): number | null {
  const rows = messageRows(doc);
  if (rows.length === 0) return null;
  const current = focusedMessageId(doc);
  const at = current === null ? -1 : rows.findIndex((row) => idOf(row) === current);
  const next = at === -1 ? rows.length - 1 : at + step;
  const row = rows[Math.min(Math.max(next, 0), rows.length - 1)];
  return row ? focusRow(row) : null;
}

/** Put the keyboard on one message by id. */
export function focusMessage(id: number, doc?: Root): number | null {
  const scope = root(doc);
  const row = scope?.querySelector(`[${MESSAGE_ROW}="${id}"]`);
  return row ? focusRow(row) : null;
}

function focusRow(row: Element): number | null {
  const stop = row.querySelector(`[${MESSAGE_CURSOR}]`);
  if (stop instanceof HTMLElement) {
    stop.focus();
    // `nearest` rather than `center`: the header is a line of text and the
    // conversation is already scrolled where the reader left it.
    stop.scrollIntoView?.({ block: "nearest" });
  }
  return idOf(row);
}

/**
 * What a reply verb should answer, given where the keyboard is.
 *
 * The one rule, in one place, so the three keys and the three menu items cannot
 * disagree: the focused message when there is one, and otherwise nothing —
 * which `prepare` reads as the newest message in the thread.
 *
 * A draft row never answers. It is drawn as a message because it is mirrored
 * into the conversation, so it can hold the keyboard like any other row, and
 * `r` there means "reply to this conversation" rather than "reply to my own
 * unsent text". Rust refuses it as well; this is what keeps the refusal from
 * ever being reached.
 */
export function replyTarget(doc?: Root): number | null {
  const scope = root(doc);
  const active = scope?.activeElement;
  if (!(active instanceof Element)) return null;
  const row = active.closest(`[${MESSAGE_ROW}]`);
  if (!row || row.hasAttribute(MESSAGE_DRAFT)) return null;
  return idOf(row);
}

function idOf(row: Element | null | undefined): number | null {
  const raw = row?.getAttribute(MESSAGE_ROW);
  if (raw === null || raw === undefined || raw === "") return null;
  const id = Number(raw);
  return Number.isFinite(id) ? id : null;
}
