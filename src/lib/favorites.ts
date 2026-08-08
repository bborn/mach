/**
 * Favorites — the things you chose to keep in the sidebar.
 *
 * The rail no longer lists every label, so the only things in it are the ones
 * you navigate to constantly plus the ones you asked for. A favorite is a
 * mailbox *as you were looking at it* (label plus account scope, so "Inbox ·
 * Personal" and "Inbox" are two different favorites) or a single conversation
 * you want to keep one click away.
 *
 * This module is pure: a list in, a list out, plus a defensive parse of
 * whatever `localStorage` hands back. Nothing here touches React, so the rules
 * are testable and the persistence layer is a two-line adapter at the bottom.
 */

import type { AccountId, LabelId, ThreadId } from "@/types";

export type Favorite =
  | { kind: "mailbox"; labelId: LabelId; accountId: AccountId | null; name: string }
  | { kind: "thread"; threadId: ThreadId; accountId: AccountId; name: string };

export const FAVORITES_STORAGE_KEY = "mach.favorites.v1";

/**
 * Identity of a favorite. A mailbox is identified by label *and* account scope;
 * a conversation by its thread id, whatever it happens to be called today.
 */
export function favoriteKey(favorite: Favorite): string {
  return favorite.kind === "mailbox"
    ? `mailbox:${favorite.accountId ?? "all"}:${favorite.labelId}`
    : `thread:${favorite.threadId}`;
}

export function isFavorited(favorites: readonly Favorite[], key: string): boolean {
  return favorites.some((favorite) => favoriteKey(favorite) === key);
}

/** Adds to the end — favorites keep the order they were pinned in. */
export function addFavorite(favorites: readonly Favorite[], favorite: Favorite): Favorite[] {
  const key = favoriteKey(favorite);
  // Re-pinning something already pinned refreshes its name rather than
  // duplicating the row: a renamed label should not appear twice.
  if (isFavorited(favorites, key)) {
    return favorites.map((existing) => (favoriteKey(existing) === key ? favorite : existing));
  }
  return [...favorites, favorite];
}

export function removeFavorite(favorites: readonly Favorite[], key: string): Favorite[] {
  return favorites.filter((favorite) => favoriteKey(favorite) !== key);
}

export function toggleFavorite(favorites: readonly Favorite[], favorite: Favorite): Favorite[] {
  const key = favoriteKey(favorite);
  return isFavorited(favorites, key)
    ? removeFavorite(favorites, key)
    : addFavorite(favorites, favorite);
}

/* -------------------------------------------------------------------------- */
/* Persistence                                                                 */
/* -------------------------------------------------------------------------- */

/** The slice of `Storage` this needs, so a test can pass a `Map`-backed fake. */
export interface FavoriteStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/**
 * Parse whatever is in storage, discarding anything that is not a favorite.
 *
 * This runs against data written by an older build of the app, so a wrong shape
 * is an ordinary event, not an exception: bad entries are dropped and the good
 * ones still load.
 */
export function parseFavorites(raw: string | null | undefined): Favorite[] {
  if (!raw) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];

  const out: Favorite[] = [];
  const seen = new Set<string>();
  for (const entry of parsed) {
    const favorite = toFavorite(entry);
    if (!favorite) continue;
    const key = favoriteKey(favorite);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(favorite);
  }
  return out;
}

function toFavorite(value: unknown): Favorite | null {
  if (typeof value !== "object" || value === null) return null;
  const row = value as Record<string, unknown>;
  const name = typeof row.name === "string" ? row.name : null;
  if (!name) return null;

  if (row.kind === "mailbox") {
    if (typeof row.labelId !== "string" || !row.labelId) return null;
    const accountId =
      typeof row.accountId === "number" ? row.accountId : row.accountId === null ? null : undefined;
    if (accountId === undefined) return null;
    return { kind: "mailbox", labelId: row.labelId, accountId, name };
  }

  if (row.kind === "thread") {
    if (typeof row.threadId !== "number" || typeof row.accountId !== "number") return null;
    return { kind: "thread", threadId: row.threadId, accountId: row.accountId, name };
  }

  return null;
}

export function serializeFavorites(favorites: readonly Favorite[]): string {
  return JSON.stringify(favorites);
}

/** `localStorage`, or nothing at all — a private window is not an error. */
function browserStore(): FavoriteStore | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

export function loadFavorites(store: FavoriteStore | null = browserStore()): Favorite[] {
  if (!store) return [];
  try {
    return parseFavorites(store.getItem(FAVORITES_STORAGE_KEY));
  } catch {
    return [];
  }
}

export function saveFavorites(
  favorites: readonly Favorite[],
  store: FavoriteStore | null = browserStore(),
): void {
  if (!store) return;
  try {
    store.setItem(FAVORITES_STORAGE_KEY, serializeFavorites(favorites));
  } catch {
    // A full or blocked store must not take the window down with it.
  }
}
