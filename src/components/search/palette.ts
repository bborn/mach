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

import { parseSearchQuery, type ParsedSearch } from "@/lib/search-query";
import { registerResolver, type PaletteResolver, type PaletteResult } from "@/lib/palette/resolver";

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
    const parsed = parseSearchQuery(query, { prefixLastTerm: false });
    if (!parsed.node) return [];

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
    return [result];
  },
};

/** The parse, or the raw words when there was nothing to interpret. */
function describe(parsed: ParsedSearch): string {
  return parsed.chips.length > 0 ? parsed.chips.join(" · ") : `“${parsed.input}”`;
}

const OPERATOR = /(^|\s)(from|to|cc|bcc|subject|label|in|is|has|filename|before|after|older_than|newer_than):/i;

function hasOperators(query: string): boolean {
  return OPERATOR.test(query) || query.includes(" OR ") || /(^|\s)-\S/.test(query);
}

registerResolver(searchResolver);
