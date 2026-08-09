import {
  fuzzyScore,
  type PaletteContext,
  type PaletteResolver,
  type PaletteResult,
} from "@/lib/palette/resolver";
import type { Preferences } from "@/lib/prefs";
import { setPreferenceFromAnywhere } from "./PreferencesProvider";

/**
 * ⌘K's half of the preferences surface.
 *
 * Registered through `registerResolver` rather than added to the chain in
 * `resolver.ts`, which is the seam that file documents: a later layer arrives by
 * registering itself, and the palette component keeps knowing nothing about
 * what is in it.
 *
 * # Why the settings are here and not only in the dialog
 *
 * Because a dialog is a place you go and ⌘K is a thing you say. "Dark" is one
 * word and one keystroke away from being true; making it four — ⌘, tab tab
 * select — for a setting people flip twice a day would be a worse app with more
 * settings in it. The dialog is where you *read* the whole surface; this is
 * where you change the two or three things you already know you want.
 *
 * Only the settings with a small, nameable set of values get an entry. "Sync
 * every 5 minutes" as eight palette rows would push everything the user was
 * actually searching for off the screen, and a number is not a thing you say —
 * it is a thing you pick from a list, which is what the dialog is for.
 */

interface Entry {
  id: string;
  title: string;
  /** Right-aligned in the row: what this does, in a word or two. */
  meta?: string;
  keywords: string;
  run: () => void;
}

/** Opened from ⌘K and from ⌘,, so the dialog does not have to be a route. */
export const PREFERENCES_EVENT = "mach:preferences";

export function openPreferences(): void {
  window.dispatchEvent(new CustomEvent(PREFERENCES_EVENT));
}

function theme(value: Preferences["theme"], title: string): Entry {
  return {
    id: `prefs-theme-${value}`,
    title,
    meta: "theme",
    keywords: `theme appearance colour color scheme ${value} dark light system`,
    run: () => setPreferenceFromAnywhere({ theme: value }),
  };
}

const ENTRIES: Entry[] = [
  {
    id: "prefs-open",
    title: "Preferences…",
    meta: "⌘,",
    keywords: "preferences settings options configure signature theme undo sync calendar",
    run: openPreferences,
  },
  theme("dark", "Change theme: dark"),
  theme("light", "Change theme: light"),
  theme("system", "Change theme: match the system"),
];

/**
 * How confident an implicit match has to be, borrowed from `commandResolver`
 * and for the same reason: outside `>` mode these rows sit above the mail the
 * user is probably searching for, so a scattered fuzzy hit is not enough.
 */
const IMPLICIT_FLOOR = 500;

export const preferencesResolver: PaletteResolver = {
  id: "preferences",
  // Level with the ordinary command layer. Preferences are commands; they have
  // no claim to sit above the ones that act on the mailbox in front of you.
  priority: 20,
  claims: () => true,
  resolve(ctx: PaletteContext): PaletteResult[] {
    const explicit = ctx.query.startsWith(">");
    const q = (explicit ? ctx.query.slice(1) : ctx.query).trim();
    if (!explicit && !q) return [];

    return ENTRIES.map((entry) => ({
      entry,
      score:
        explicit && !q
          ? 1
          : Math.max(fuzzyScore(entry.title, q), fuzzyScore(entry.keywords, q) * 0.8),
    }))
      .filter(({ score }) => score > 0 && (explicit || score >= IMPLICIT_FLOOR))
      .sort((a, b) => b.score - a.score)
      .slice(0, explicit ? ENTRIES.length : 3)
      .map<PaletteResult>(({ entry, score }) => ({
        id: `command:${entry.id}`,
        kind: "command",
        title: entry.title,
        meta: entry.meta,
        score,
        run: entry.run,
      }));
  },
};
