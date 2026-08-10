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

/** Which section to land on. `SectionId` in the dialog; a string on the wire. */
export interface PreferencesRequest {
  section?: string;
}

/**
 * Open Preferences, optionally on a named section.
 *
 * The section matters because the dialog remembers the last one looked at, so a
 * caller sending the user somewhere specific — the status bar saying an account
 * needs signing in again — cannot rely on where it happens to be.
 */
export function openPreferences(section?: string): void {
  window.dispatchEvent(
    new CustomEvent<PreferencesRequest>(PREFERENCES_EVENT, { detail: { section } }),
  );
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
    run: () => openPreferences(),
  },
  {
    // The keyboard route to the accounts list, and the one the status bar's
    // "One account needs signing in again" points at with the mouse. Tab in the
    // main window moves between the rail and the list — it is claimed by the
    // mail keymap — so a footer button is not on its own a way in.
    id: "prefs-accounts",
    title: "Accounts…",
    meta: "⌘,",
    keywords:
      "account accounts sign in again signin authorize authorization reauthorize google add remove connect keychain",
    run: () => openPreferences("accounts"),
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
