import {
  fuzzyScore,
  type PaletteContext,
  type PaletteResolver,
  type PaletteResult,
} from "@/lib/palette/resolver";

/**
 * ⌘K's route to the sync detail.
 *
 * The status bar sits under the mail keymap, which owns Tab, so a button in the
 * footer is reachable with a mouse and with nothing else. Every recovery in this
 * app has to be reachable from the keyboard, and the palette is the seam the
 * rest of the window already uses for it — `prefs/palette.ts` puts "Accounts…"
 * there for exactly the same reason.
 *
 * Registered by `SyncIndicator` while it is mounted, which is only while a sync
 * is running or has failed. There is nothing to say otherwise.
 */

export const SYNC_DETAIL_EVENT = "mach:sync-detail";

/** Open the sync detail panel on the status bar's indicator. */
export function openSyncDetail(): void {
  window.dispatchEvent(new CustomEvent(SYNC_DETAIL_EVENT));
}

const ENTRY = {
  id: "sync-detail",
  title: "Sync status…",
  keywords:
    "sync status failed failure error retry again sign in signin authorize reauthorize account accounts google stalled stuck",
};

/**
 * The same floor `preferencesResolver` uses, and for the same reason: outside
 * `>` mode this row sits above the mail being searched for, so a scattered
 * fuzzy hit is not enough to earn the space.
 */
const IMPLICIT_FLOOR = 500;

export const syncDetailResolver: PaletteResolver = {
  id: "sync-detail",
  priority: 20,
  claims: () => true,
  resolve(ctx: PaletteContext): PaletteResult[] {
    const explicit = ctx.query.startsWith(">");
    const q = (explicit ? ctx.query.slice(1) : ctx.query).trim();
    if (!explicit && !q) return [];

    const score =
      explicit && !q
        ? 1
        : Math.max(fuzzyScore(ENTRY.title, q), fuzzyScore(ENTRY.keywords, q) * 0.8);
    if (score <= 0 || (!explicit && score < IMPLICIT_FLOOR)) return [];

    return [
      {
        id: `command:${ENTRY.id}`,
        kind: "command",
        title: ENTRY.title,
        score,
        run: openSyncDetail,
      },
    ];
  },
};
