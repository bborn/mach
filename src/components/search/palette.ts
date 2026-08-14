/**
 * ⌘K's route into the search view.
 *
 * The palette resolves in layers (see `lib/palette/resolver.ts`), and its own
 * comment reserves layer 1 for "operator queries — `from:tawny has:attachment`".
 * This is that layer, and it is registered through `registerResolver` rather
 * than added to the chain in that file, which is the whole reason the seam
 * exists: the palette component does not import this and does not know it ran.
 *
 * What it offers is one row, not results. The palette is six lines deep and
 * ranked by fuzzy match; an operator query wants a list you can walk, keep and
 * page through, which is the search view. So ⌘K's job here is to hand the
 * sentence over rather than to answer it — and it says so out loud, with the
 * parse read back, so nobody types `from:tawny` into the palette and quietly
 * gets a fuzzy title match instead.
 *
 * Registration is a module side effect because ⌘K is global and the view that
 * imports this is not: `MailMode` unmounts the moment the calendar is on
 * screen, and "search my mail" has to stay reachable from there.
 */

import type { Participant } from "@/types";
import { parseSearchQuery, type ParsedSearch } from "@/lib/search-query";
import {
  registerResolver,
  type PaletteContext,
  type PaletteResolver,
  type PaletteResult,
} from "@/lib/palette/resolver";
// From the leaf module rather than the re-export in `resolver.ts`, for the
// reason `score.ts` was split out in the first place: this file is imported
// *by* the chain, and a cycle back into it is a blank window in WKWebView.
import { fuzzyScore } from "@/lib/palette/score";

/** What `SearchView` listens for. Detail is the query to start from. */
export const SEARCH_EVENT = "mach:search";

export interface SearchEventDetail {
  query: string;
}

/** Ask the search view to open on a query, from anywhere in the app. */
export function openSearch(query: string): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(
    new CustomEvent<SearchEventDetail>(SEARCH_EVENT, { detail: { query } }),
  );
}

/**
 * Ranked above the local layer when the query is operator-shaped and below it
 * otherwise: `from:tawny` is unambiguous and belongs at the top, while `tawny`
 * should still show the six threads ⌘K already knows about first.
 */
const OPERATOR_PRIORITY = 18;

export const searchResolver: PaletteResolver = {
  id: "search",
  priority: OPERATOR_PRIORITY,
  claims: (query) => !query.startsWith(">") && query.trim().length > 0,
  resolve(ctx) {
    const query = ctx.query.trim();
    const address = addressIn(query);
    const person = personRow(ctx, query, address);

    // An address on its own gets *one* row, not two. The full-text search for
    // the characters `john@gmail.com` — which finds the address quoted in a
    // signature or a forwarded header — is a real thing to want and a rare one,
    // and offering it beside the correspondence it is one letter away from is
    // how two rows that both say "search" end up under the same keystroke.
    if (address) return person ? [person] : [];

    const parsed = parseSearchQuery(query, { prefixLastTerm: false });
    if (!parsed.node) return person ? [person] : [];

    const result: PaletteResult = {
      id: "search:open",
      kind: "command",
      title: `Search all mail for ${describe(parsed)}`,
      meta: "⏎",
      // A hair above the fuzzy floor for an implicit command match, so it sits
      // at the top of the commands group when the query is a real query and
      // does not shove `Archive` off screen when it is not.
      score: hasOperators(query) ? 10_000 : 400,
      run: () => openSearch(query),
    };
    return person ? [person, result] : [result];
  },
};

/* -------------------------------------------------------------------------- */
/* Everything with one person                                                  */
/* -------------------------------------------------------------------------- */

/**
 * The other half of ⌘K's answer to a name.
 *
 * Typing a person into the palette offered exactly one thing: write to them.
 * "Show me everything with this person" is the more common want by a distance,
 * and it had no route at all — the search language could say it (`from:` and
 * `to:` are real operators of the real parser) but nothing in the interface
 * ever wrote that sentence for you.
 *
 * # One row, not two per person
 *
 * The obvious shape is a second row beside each matched contact: *write* and
 * *search*, under People, explicit and symmetrical. It is also six extra rows
 * in a list that already mixes commands, labels, mail, calendar and people, and
 * the palette is six lines deep. So this offers **one** row, for the best match
 * only, and it goes at the top rather than into the People group — the same
 * place, and for the same reason, the generic search row already sits: a search
 * is a destination and the groups are ordered destinations first.
 *
 * The compose row stays exactly where it was, under People, now labelled
 * `write` so the two offers cannot be mistaken for each other.
 *
 * Neither of them is a modifier. A modifier on a palette row is a thing you
 * either already know or never discover.
 */
function personRow(
  ctx: PaletteContext,
  query: string,
  typed: string | null,
): PaletteResult | null {
  // An address the store has never seen is still a person. He may be checking
  // whether he has ever heard from them, which is exactly the question this
  // answers — and `ctx.people` is built from mail that exists, so it cannot.
  const known = typed
    ? ctx.people.find((p) => p.email.toLowerCase() === typed)
    : bestMatch(ctx.people, query);
  const email = typed ?? known?.email;
  if (!email) return null;

  const name = known?.name && known.name !== known.email ? known.name : null;
  return {
    id: `search:person:${email.toLowerCase()}`,
    kind: "command",
    title: `Search all mail with ${name ?? email}`,
    subtitle: name ? email : undefined,
    meta: "⏎",
    // An address is as unambiguous as an operator query and is ranked with one.
    // A name is a guess about what was meant, so it ranks below a command whose
    // title the query is a prefix of (`fuzzyScore`'s 1000) and above the
    // full-text row it appears beside.
    score: typed ? 10_000 : 900,
    run: () => openSearch(correspondenceQuery(email)),
  };
}

/**
 * Mail with someone, in the operator language, so the box can show it and he
 * can narrow it afterwards.
 *
 * `cc:` is in because being copied is being written to — a work thread where he
 * is on `To` and she is on `Cc` is the ordinary case, not the exotic one. It
 * costs about 80ms in the worst case (an address with no mail at all, so
 * nothing stops the scan early) and nothing measurable in the usual one.
 *
 * `bcc:` is out. Gmail only reports a Bcc header on messages he sent himself,
 * so it can never find the mail somebody blind-copied *him* on — it is the most
 * expensive of the four and the one that buys the least.
 */
export function correspondenceQuery(email: string): string {
  return `from:${email} OR to:${email} OR cc:${email}`;
}

/**
 * The address in a query that is nothing but an address.
 *
 * `Bruno <b@example.com>` counts, because that is what a paste out of a mail
 * client looks like. `from:b@example.com` does not — `:` is excluded from both
 * halves, so an operator query can never be read as a bare address.
 */
const ADDRESS = /^[^\s@<>,;:"]+@[^\s@<>,;:"]+\.[^\s@<>,;:"]+$/;

export function addressIn(query: string): string | null {
  const angled = /<([^>]+)>/.exec(query);
  const candidate = (angled ? angled[1]! : query).trim().replace(/^"|"$/g, "");
  return ADDRESS.test(candidate) ? candidate.toLowerCase() : null;
}

/**
 * `fuzzyScore`'s floor for a real prefix or substring hit.
 *
 * The same number, for the same reason, as `IMPLICIT_COMMAND_FLOOR`: a
 * scattered subsequence match will find *somebody* for almost any three
 * letters, and offering to search a stranger's mail because their surname
 * happens to contain an `a`, an `r` and a `c` is worse than offering nothing.
 */
const CONFIDENT = 500;

function bestMatch(people: readonly Participant[], query: string): Participant | null {
  let best: Participant | null = null;
  let bestScore = CONFIDENT - 1;
  for (const person of people) {
    const score = Math.max(fuzzyScore(person.name, query), fuzzyScore(person.email, query));
    if (score > bestScore) {
      best = person;
      bestScore = score;
    }
  }
  return best;
}

/** The parse, or the raw words when there was nothing to interpret. */
function describe(parsed: ParsedSearch): string {
  return parsed.chips.length > 0 ? parsed.chips.join(" · ") : `“${parsed.input}”`;
}

const OPERATOR = /(^|\s)(from|to|cc|bcc|subject|label|in|is|has|filename|before|after|older_than|newer_than):/i;

function hasOperators(query: string): boolean {
  return OPERATOR.test(query) || query.includes(" OR ") || /(^|\s)-\S/.test(query);
}

registerResolver(searchResolver);
