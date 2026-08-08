/**
 * Which address you are typing, and what happens when you pick one.
 *
 * An address field holds a *list* in one string — `ada@x.com, bob@y.com, gr` —
 * and only the last of those is a question. Everything here is about that: find
 * the chunk the caret is inside, and put a chosen contact back in its place
 * without disturbing the ones on either side.
 *
 * It is a separate module from `compose.ts` for the same reason the parsing
 * rules are: the composer and the event modal both hold a list of people in a
 * text field, they disagree about the separators they accept (the event modal
 * takes newlines too), and neither should own the rule the other depends on.
 */

export interface Token {
  /** Index of the first character of the chunk, after any separator. */
  start: number;
  /** Index one past its last character. */
  end: number;
  /** The chunk itself, untrimmed. */
  text: string;
}

/** What ends one address and starts the next. Newlines count: people paste. */
const SEPARATORS = new Set([",", ";", "\n"]);

/**
 * The chunk the caret sits in.
 *
 * Quotes are respected on the way back, so `"Doe, Jane" <j@x.com>` is one
 * address rather than two — the same rule `parseRecipients` follows, because a
 * field that splits differently from the parser would complete into a value the
 * parser then re-splits.
 */
export function activeToken(value: string, caret: number): Token {
  const at = Math.max(0, Math.min(caret, value.length));

  let start = 0;
  let quoted = false;
  for (let i = 0; i < at; i += 1) {
    const ch = value[i];
    if (ch === '"') quoted = !quoted;
    else if (SEPARATORS.has(ch) && !quoted) start = i + 1;
  }

  let end = value.length;
  quoted = false;
  for (let i = at; i < value.length; i += 1) {
    const ch = value[i];
    if (ch === '"') quoted = !quoted;
    else if (SEPARATORS.has(ch) && !quoted) {
      end = i;
      break;
    }
  }

  return { start, end, text: value.slice(start, end) };
}

/** What to match contacts against: the chunk, without its padding. */
export function tokenQuery(value: string, caret: number): string {
  return activeToken(value, caret).text.trim();
}

export interface Replacement {
  value: string;
  caret: number;
}

/**
 * Put `insert` where the caret's chunk was.
 *
 * Accepting the last address in the field leaves `", "` behind it and the caret
 * after that, because the next thing you do is either type another name or stop
 * — and a trailing separator costs nothing to the parser, which drops empty
 * chunks. Accepting an address in the middle leaves the rest of the line alone
 * and puts the caret at the end of what was just inserted.
 */
export function replaceToken(value: string, caret: number, insert: string): Replacement {
  const token = activeToken(value, caret);
  const before = value.slice(0, token.start);
  const after = value.slice(token.end);
  // A chunk that was padded keeps its leading space: `a@x.com, bo` completes to
  // `a@x.com, bob@y.com`, not `a@x.com,bob@y.com`.
  const padding = /^\s*/.exec(token.text)?.[0] ?? "";
  const isLast = after.trim() === "";

  const body = padding + insert + (isLast ? ", " : "");
  return {
    value: before + body + (isLast ? "" : after),
    caret: before.length + body.length,
  };
}

/**
 * Addresses already committed in the field, so they are not offered twice.
 *
 * The chunk under the caret is excluded — it is the question, not an answer.
 */
export function committedAddresses(value: string, caret: number): string[] {
  const token = activeToken(value, caret);
  const out: string[] = [];
  let chunk = "";
  let quoted = false;

  const flush = (from: number) => {
    const text = chunk.trim();
    chunk = "";
    if (!text) return;
    // `from` is where this chunk started; skip the one the caret is in.
    if (from === token.start) return;
    const open = text.lastIndexOf("<");
    const close = text.lastIndexOf(">");
    const email = open !== -1 && close > open ? text.slice(open + 1, close) : text;
    if (email.trim()) out.push(email.trim().toLowerCase());
  };

  let start = 0;
  for (let i = 0; i < value.length; i += 1) {
    const ch = value[i];
    if (ch === '"') {
      quoted = !quoted;
      chunk += ch;
    } else if (SEPARATORS.has(ch) && !quoted) {
      flush(start);
      start = i + 1;
    } else {
      chunk += ch;
    }
  }
  flush(start);
  return out;
}
