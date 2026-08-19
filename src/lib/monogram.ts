/**
 * Initials and a colour for whoever sent it.
 *
 * The thread list had no visual key for *who*. Sender, subject and preview are
 * three lines of type separated by size and greyness, and scanning the list
 * meant reading it — there was nothing for the eye to land on and count off.
 * The 2px bar down the left edge answers "which of my accounts", which is a
 * different question and the only one the row could answer at a glance.
 *
 * Every mail client that solves this solves it the same way, and so does this
 * one: a small tile with one or two letters in it, coloured from the sender's
 * address. Gmail's API hands back no avatar image, so the letters are all
 * anybody gets — which is also what the competition draws, initials and no
 * photographs.
 *
 * # Why this is worth 26px of a row that guards its width
 *
 * `ThreadRow` turned down a permanent checkbox column for costing "~20px of
 * subject width forever to serve a gesture used a few times a day", and the
 * reasoning holds. It is a rule about *rarely used* things: a monogram is read
 * on every row every time the list is looked at, which is the opposite end of
 * that trade. It sits beside the unread dot in the left margin, against all
 * three lines, so it takes the space once rather than off the subject alone.
 */

import { FALLBACK_FILLS, hashString } from "./calendar-palette";

/**
 * The letters.
 *
 * Two from a name with two words in it, one from a name with one, and the
 * first letter of the address when there is no name at all. That last case is
 * the machine senders — `receipts@`, `no-reply@` — and one letter is the honest
 * amount of identity they have.
 *
 * Split by code point rather than by `charAt`, so a display name that starts
 * with an emoji or a CJK character produces that character rather than half of
 * its surrogate pair.
 */
export function initialsOf(name?: string, email?: string): string {
  const words = (name ?? "")
    // Leading quotes and the `(via Somewhere)` a sender bolts on are not part
    // of anybody's name.
    .replace(/[("'“”‘’]/g, " ")
    .split(/[\s,]+/)
    .filter((word) => /^[\p{L}\p{N}]/u.test(word));

  if (words.length >= 2) return first(words[0]!) + first(words[words.length - 1]!);
  if (words.length === 1) return first(words[0]!);

  const local = (email ?? "").split("@")[0] ?? "";
  const letters = local.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  return letters.length > 0 ? first(letters[0]!) : "?";
}

function first(word: string): string {
  return (Array.from(word)[0] ?? "").toUpperCase();
}

/**
 * The colour, hashed off the address so it never moves.
 *
 * The address and not the display name: people rename themselves between
 * messages — "Ivy" one week, "Ivy Chen (Northloop)" the next — and a tile that
 * changes colour when they do is worse than no tile, because the thing it is
 * for is recognising them without reading.
 *
 * The ramp is the calendar's eight, so the app has one set of hues rather than
 * two that nearly match.
 */
export function monogramColor(email?: string): string {
  const key = (email ?? "").trim().toLowerCase();
  if (!key) return FALLBACK_FILLS[0]!;
  return FALLBACK_FILLS[hashString(key) % FALLBACK_FILLS.length]!;
}
