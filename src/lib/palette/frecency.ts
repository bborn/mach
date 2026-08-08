/**
 * The palette remembers what you actually use.
 *
 * A ranked list that ignores history makes you retype the same six characters
 * forever. The fix people expect is "frecency" — Mozilla's coinage — which
 * beats either half on its own:
 *
 *   * **Frequency alone** ossifies. Something you hammered for a week in March
 *     outranks what you have used all month.
 *   * **Recency alone** thrashes. One stray pick reorders the list, and the
 *     thing you use twenty times a day drops below it.
 *
 * So each use is worth a point that decays with age, and the score is their
 * sum. Twenty uses last week still beat one use an hour ago, but a command you
 * abandoned in March fades out on its own without any pruning pass.
 *
 * Applied as a *boost* to the resolver's relevance score rather than replacing
 * it: typing an exact match must always win, or the palette stops being
 * predictable. History breaks ties and fills the empty query — it does not
 * overrule what you typed.
 */

/** Uses decay to half their value over this long. */
const HALF_LIFE_MS = 14 * 24 * 60 * 60 * 1000; // two weeks

/**
 * How much a well-worn entry can lift a result.
 *
 * Deliberately modest. Resolver scores run to a few dozen for a strong textual
 * match, so this reorders near-ties and does not float a stale favourite above
 * something you literally just typed.
 */
const MAX_BOOST = 12;

/** Uses beyond this add nothing, so a runaway count cannot dominate. */
const SATURATION = 8;

/** Entries older than this are dropped on write; two half-lives is ~0.25 weight. */
const FORGET_AFTER_MS = 90 * 24 * 60 * 60 * 1000;

const STORAGE_KEY = "mach.palette.frecency.v1";

/** Timestamps of recent uses, newest last. */
export type FrecencyStore = Record<string, number[]>;

/** Only the last N uses of any entry are kept — older ones cannot matter. */
const MAX_SAMPLES = 16;

export function decayedScore(
  timestamps: readonly number[],
  now: number,
): number {
  let sum = 0;
  for (const t of timestamps) {
    const age = Math.max(0, now - t);
    sum += Math.pow(0.5, age / HALF_LIFE_MS);
  }
  return sum;
}

/**
 * The boost for one entry, in the same units as the resolver's `score`.
 *
 * Saturating rather than linear: the gap between one use and five should be
 * large, between twenty and forty almost nothing.
 */
export function boostFor(
  store: FrecencyStore,
  id: string,
  now: number,
): number {
  const uses = store[id];
  if (!uses || uses.length === 0) return 0;
  const raw = decayedScore(uses, now);
  return MAX_BOOST * (1 - Math.exp(-raw / (SATURATION / 2)));
}

/** Records a use, keeping the store bounded. */
export function record(
  store: FrecencyStore,
  id: string,
  now: number,
): FrecencyStore {
  const next: FrecencyStore = {};
  for (const [key, uses] of Object.entries(store)) {
    const kept = uses.filter((t) => now - t < FORGET_AFTER_MS);
    if (kept.length > 0) next[key] = kept;
  }
  const existing = next[id] ?? [];
  next[id] = [...existing, now].slice(-MAX_SAMPLES);
  return next;
}

/**
 * Applies the boost, preserving the resolver's grouping.
 *
 * Results keep their `kind`; only `score` moves. The palette groups by kind
 * before displaying, so this reorders within a group rather than mixing
 * commands into the middle of mail results.
 */
export function applyFrecency<T extends { id: string; score?: number }>(
  results: readonly T[],
  store: FrecencyStore,
  now: number,
): T[] {
  return results.map((r) => {
    const boost = boostFor(store, r.id, now);
    return boost === 0 ? r : { ...r, score: (r.score ?? 0) + boost };
  });
}

/**
 * The entries to offer for an empty query, most-used first.
 *
 * An empty palette showing nothing wastes the most common interaction there
 * is: open it, and the thing you were reaching for is already at the top.
 */
export function topEntries(
  store: FrecencyStore,
  now: number,
  limit = 6,
): { id: string; score: number }[] {
  return Object.entries(store)
    .map(([id, uses]) => ({ id, score: decayedScore(uses, now) }))
    .filter((e) => e.score > 0.05)
    .sort((a, b) => b.score - a.score)
    .slice(0, limit);
}

/* -------------------------------------------------------------------------- */
/* Persistence                                                                */
/* -------------------------------------------------------------------------- */

/**
 * Kept in `localStorage` rather than SQLite on purpose: this is UI preference,
 * not mail. It should not be in the store that syncs, it should not survive
 * into a QA instance seeded from a copy of the real database, and losing it
 * costs the user nothing but a few days of re-learning.
 */
export function load(storage: Storage | undefined = safeStorage()): FrecencyStore {
  if (!storage) return {};
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return {};
    const out: FrecencyStore = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (Array.isArray(v) && v.every((n) => typeof n === "number")) out[k] = v;
    }
    return out;
  } catch {
    // Corrupt or unavailable storage must never break the palette.
    return {};
  }
}

export function save(
  store: FrecencyStore,
  storage: Storage | undefined = safeStorage(),
): void {
  if (!storage) return;
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(store));
  } catch {
    /* quota or private mode — the palette still works, it just forgets */
  }
}

function safeStorage(): Storage | undefined {
  try {
    return typeof localStorage === "undefined" ? undefined : localStorage;
  } catch {
    return undefined;
  }
}
