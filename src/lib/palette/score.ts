/**
 * Ranking, on its own, because two resolvers need it and one of them is not in
 * this directory.
 *
 * It lived in `resolver.ts` until the plugin resolver arrived: `resolver.ts`
 * imports `pluginResolver` to put it in the chain, and `plugins/palette.ts`
 * needs `fuzzyScore` to rank with — a cycle that a bundler forgives and a
 * browser's live module graph does not. It surfaced in WKWebView as
 * "Cannot access uninitialized variable" *at the top of the app*, which is a
 * bad way to find out. One leaf module with no imports of its own cannot be
 * part of a cycle.
 */

/**
 * Subsequence match with a bias towards prefixes and word starts. Deliberately
 * small: local ranking has to feel instant on every keystroke, and FTS5 does
 * the real work once the Rust side is wired up.
 */
export function fuzzyScore(haystack: string, needle: string): number {
  if (!needle) return 1;
  const hay = haystack.toLowerCase();
  const pin = needle.toLowerCase();

  const direct = hay.indexOf(pin);
  if (direct === 0) return 1000;
  if (direct > 0) return 700 - Math.min(direct, 200) + (hay[direct - 1] === " " ? 100 : 0);

  let score = 0;
  let cursor = 0;
  for (const char of pin) {
    const found = hay.indexOf(char, cursor);
    if (found === -1) return 0;
    score += found === 0 || hay[found - 1] === " " ? 8 : 2;
    cursor = found + 1;
  }
  return score;
}
