/**
 * "Sync now" — the one place it is started from, and the one place its answer
 * is turned into words.
 *
 * # Why this is not just a call
 *
 * Two things have to be true of a key that talks to Google, and neither is a
 * property of the IPC call on its own.
 *
 * **Pressing it twice must not do it twice.** The engine already refuses to run
 * two passes over one account — see `SyncEngine::force_sync` — so a second
 * press is harmless, but it is also *pointless*, and a UI that lets you fire it
 * ten times while nothing visibly changes is a UI that looks broken. The
 * in-flight set here is what lets the palette entry read as busy and what stops
 * the second press ever reaching the wire.
 *
 * **It has to say whether it worked.** A background sync is allowed to be
 * silent; one somebody asked for is not. `forcedSyncMessage` is the pure half
 * of that — outcome in, one line out — so the wording is testable without a
 * window, and so the same sentence appears wherever a forced sync is started
 * from.
 *
 * No React here on purpose: the store is a plain set with subscribers, which is
 * what `useSyncExternalStore` wants and what a node test can drive directly.
 */

import type { AccountId, ForcedSync } from "@/types";

/**
 * What a forced sync is addressed to. `"all"` is the ⇧⌘R / ⌘K case; an account
 * id is the retry that sits beside one failing account.
 */
export type ForcedSyncKey = AccountId | "all";

const inFlight = new Set<ForcedSyncKey>();
const listeners = new Set<() => void>();

function announce(): void {
  for (const listener of listeners) listener();
}

/**
 * Claim the slot. `false` means one is already running for this target and the
 * caller must not issue a second request.
 */
export function beginForcedSync(key: ForcedSyncKey): boolean {
  if (inFlight.has(key)) return false;
  inFlight.add(key);
  announce();
  return true;
}

export function endForcedSync(key: ForcedSyncKey): void {
  if (!inFlight.delete(key)) return;
  announce();
}

/** Is this target syncing? With no argument: is anything? */
export function forcedSyncInFlight(key?: ForcedSyncKey): boolean {
  return key === undefined ? inFlight.size > 0 : inFlight.has(key);
}

export function subscribeForcedSync(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Test seam. Nothing in the app calls this. */
export function resetForcedSync(): void {
  inFlight.clear();
  announce();
}

export interface ForcedSyncReport {
  message: string;
  tone: "info" | "error";
}

/**
 * What to put on the status line when a forced pass comes back.
 *
 * The failure wording names the account and carries Google's own text, rather
 * than saying "Sync failed" and leaving him to go looking. That was the whole
 * complaint that produced `SyncDetail`, and a message that repeats the mistake
 * in a different place is not an improvement — the indicator still holds the
 * panel with the per-account recovery in it, and this is what tells him to go
 * and look, by saying enough that he usually does not have to.
 *
 * A dead credential is worded differently because the recovery is different: no
 * number of syncs produces a refresh token. It is the same split
 * `AccountReporter::credential_rejected` makes in Rust, said out loud.
 */
export function forcedSyncMessage(outcome: ForcedSync): ForcedSyncReport {
  const failures = outcome.accounts.filter((a) => a.error !== null);

  if (failures.length === 1) {
    const failure = failures[0]!;
    return {
      message: failure.needsReauthorization
        ? `${failure.email} needs signing in again`
        : `${failure.email}: ${failure.error}`,
      tone: "error",
    };
  }
  if (failures.length > 1) {
    const reauth = failures.filter((a) => a.needsReauthorization).length;
    return {
      message:
        reauth === failures.length
          ? `${reauth} accounts need signing in again`
          : `Sync failed on ${failures.length} accounts`,
      tone: "error",
    };
  }

  // Nothing failed. Either a pass ran, or everything asked for was already
  // mid-pass — which is not a failure and is not "up to date" either, because
  // the answer is still on its way.
  if (!outcome.started) return { message: "Already syncing", tone: "info" };
  return { message: "Up to date", tone: "info" };
}
