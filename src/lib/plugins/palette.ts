/**
 * Plugin actions in ⌘K — **one** host-owned resolver, not one per plugin.
 *
 * The design cuts `registerResolver` for plugins on purpose: every resolver runs
 * on every keystroke, so one slow or careless plugin makes ⌘K feel broken and
 * the user blames Mach. What survives is the bounded version — the host does the
 * matching, over static manifest text (`title`, `keywords`), and no plugin code
 * runs until an entry is chosen.
 *
 * So this file holds a plain array that the plugin provider keeps up to date,
 * and a resolver that scores it with the same `fuzzyScore` everything else uses.
 * A plugin cannot make it slow because a plugin never gets to run inside it.
 *
 * Priority sits just below the core command resolver: a plugin's "Archive
 * everything" must never outrank the real Archive.
 */

// Types only from `resolver`, so this module can be imported *by* it without
// forming a cycle; the scoring function lives on its own for the same reason.
import type { PaletteResolver, PaletteResult } from "@/lib/palette/resolver";
import { fuzzyScore } from "@/lib/palette/score";
import type { PluginAction, InstalledPlugin } from "./types";

export interface PaletteAction {
  plugin: InstalledPlugin;
  action: PluginAction;
}

/** How a chosen entry actually runs. Set by the provider; absent means inert. */
export type PaletteRunner = (pluginId: string, actionId: string) => void;

let entries: PaletteAction[] = [];
let runner: PaletteRunner | null = null;

/** Replace the offered set. Called whenever the plugin list changes. */
export function setPaletteActions(next: PaletteAction[], run: PaletteRunner): void {
  entries = next;
  runner = run;
}

export function paletteActions(): PaletteAction[] {
  return entries;
}

export function clearPaletteActions(): void {
  entries = [];
  runner = null;
}

/**
 * Same floor as the core command resolver: outside `>` mode a scattered
 * subsequence match is not enough to put a plugin above the mail you were
 * obviously reaching for.
 */
const IMPLICIT_FLOOR = 500;

const LIMIT = 5;

export const pluginResolver: PaletteResolver = {
  id: "plugin",
  // Below `command` (20), above mailboxes (15).
  priority: 18,
  claims: () => true,
  resolve(ctx) {
    if (entries.length === 0) return [];
    const explicit = ctx.query.startsWith(">");
    const q = (explicit ? ctx.query.slice(1) : ctx.query).trim();
    if (!explicit && !q) return [];

    return entries
      .map((entry) => ({
        entry,
        score: explicit && !q
          ? 1
          : Math.max(
              fuzzyScore(entry.action.title, q),
              fuzzyScore(entry.action.keywords ?? "", q) * 0.8,
              fuzzyScore(entry.plugin.manifest.name, q) * 0.7,
            ),
      }))
      .filter((scored) => scored.score > 0 && (explicit || scored.score >= IMPLICIT_FLOOR))
      .sort((a, b) => b.score - a.score)
      .slice(0, LIMIT)
      .map<PaletteResult>(({ entry, score }) => ({
        id: `plugin:${entry.plugin.id}:${entry.action.id}`,
        kind: "command",
        title: entry.action.title,
        // Attribution in the palette for the same reason as in the agent
        // transcript: the user is about to hand a selection to a stranger's
        // code, and ought to be able to see whose.
        subtitle: entry.plugin.manifest.name,
        meta: entry.plugin.manifest.name,
        score,
        run: () => runner?.(entry.plugin.id, entry.action.id),
      }));
  },
};
