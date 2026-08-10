/**
 * What the mail pane is actually doing, decided once.
 *
 * On first launch there is no Google client id, no account, no mail and no
 * sync. Those are four different situations with four different next actions,
 * and a spinner that means all four is a bug — it tells the user to wait for
 * something that is never going to arrive. This module is the pure function
 * that tells them apart, so the rule lives somewhere testable rather than in a
 * chain of ternaries inside a component.
 */

import { ACTIVE_PHASES, type AccountSyncStatus, type SyncStatus } from "@/types";
import type { MachErrorKind } from "./data";

export interface MailboxError {
  kind: MachErrorKind;
  message: string;
}

export type MailboxState =
  /** The first round trip has not come back yet. */
  | { kind: "loading" }
  /** No OAuth client configured — the fix is an env var, not a retry. */
  | { kind: "notConfigured"; message: string }
  /** Configured, but nobody has authorized an account yet. */
  | { kind: "noAccounts" }
  /** Accounts exist and the first pass is still filling the store. */
  | { kind: "syncing"; progress: SyncProgress }
  /** Nothing to show and something is wrong — sync failed, or IPC did. */
  | { kind: "error"; message: string }
  /** Synced, and this mailbox is genuinely empty. */
  | { kind: "empty" }
  /** There are threads. Render the list. */
  | { kind: "ready" };

export interface MailboxInput {
  /** Has the first load settled, either way? */
  booted: boolean;
  /** A failure from the boot round trip, if any. */
  error: MailboxError | null;
  accountCount: number;
  /** Threads loaded for the *current* query, not the whole store. */
  threadCount: number;
  sync: SyncStatus | null;
  /** True when a label, account or search narrows the query. Changes the copy. */
  filtered?: boolean;
}

export function mailboxState(input: MailboxInput): MailboxState {
  // Missing credentials outrank everything, including "still loading": no
  // amount of waiting produces an account, so say so as soon as it is known.
  // `sync_status` reports it as a flag; an IPC call that needed the client and
  // could not get one reports it as an error kind. Either is authoritative.
  if (input.sync?.configured === false) {
    return {
      kind: "notConfigured",
      message: input.sync.configurationError ?? "No Google OAuth client is configured.",
    };
  }
  if (input.error?.kind === "notConfigured") {
    return { kind: "notConfigured", message: input.error.message };
  }
  if (!input.booted) return { kind: "loading" };
  if (input.error) return { kind: "error", message: input.error.message };
  if (input.accountCount === 0) return { kind: "noAccounts" };
  if (input.threadCount > 0) return { kind: "ready" };

  const progress = syncProgress(input.sync);
  if (progress.active || neverSynced(input.sync)) return { kind: "syncing", progress };
  if (progress.errors.length > 0 && !input.filtered) {
    return { kind: "error", message: progress.errors[0]!.message };
  }
  return { kind: "empty" };
}

/** True until at least one account has finished a pass. */
function neverSynced(status: SyncStatus | null): boolean {
  if (!status || status.accounts.length === 0) return false;
  return status.accounts.every((a) => a.lastSuccessAt === null && a.lastError === null);
}

/* -------------------------------------------------------------------------- */
/* Progress                                                                    */
/* -------------------------------------------------------------------------- */

/**
 * One account that is not syncing, and everything needed to act on it.
 *
 * "Sync failed" in the corner of the window was the whole of what the owner was
 * told, and there was nowhere to go from it. These four fields are what he
 * asked for by name: which account, what Google actually said, when it last
 * worked, and which recovery applies.
 */
export interface SyncFailure {
  email: string;
  /**
   * Google's own text where there is any, verbatim. Paraphrasing it throws away
   * the only thing that distinguishes a password change from a revoked grant
   * from a seven-day token expiry.
   */
  message: string;
  /**
   * Only a fresh sign-in fixes this one. Syncing again cannot produce a refresh
   * token, so the action offered has to be different.
   */
  needsReauthorization: boolean;
  /** When this account last completed a pass. `null` means not in this run. */
  lastSuccessAt: number | null;
}

export interface SyncProgress {
  active: boolean;
  /** 0..1 across every backfilling account, or `null` when no total is known. */
  fraction: number | null;
  done: number;
  total: number;
  /** One line for the status bar: what is happening, and how far along. */
  label: string;
  /** The accounts doing work right now. */
  working: AccountSyncStatus[];
  errors: SyncFailure[];
  /**
   * The addresses among `errors` that need signing in again.
   *
   * Kept as its own list because the status bar's route depends on it and not
   * on the count of failures: no number of syncs produces a refresh token.
   */
  reauthorize: string[];
}

const PHASE_RANK: Record<string, number> = {
  backfill: 4,
  incremental: 3,
  calendar: 2,
  labels: 1,
};

const PHASE_VERB: Record<string, string> = {
  backfill: "Backfilling",
  incremental: "Catching up",
  calendar: "Syncing calendar",
  labels: "Reading labels",
};

function isActive(account: AccountSyncStatus): boolean {
  return ACTIVE_PHASES.includes(account.phase);
}

/**
 * What is shown when an account needs authorizing and Google never said why.
 *
 * The Keychain-missing case: the entry was deleted, or the store was copied to
 * another Mac. There is no remote error text because no request was made.
 */
const NO_CREDENTIAL = "Not signed in";

export function syncProgress(status: SyncStatus | null): SyncProgress {
  const accounts = status?.accounts ?? [];
  const working = accounts.filter(isActive);

  // Two sources, one list. The engine knows which accounts Google refused
  // during this run; `needsReauthorization` also carries the ones whose
  // Keychain entry was already gone at launch, which no pass can discover
  // because a *missing* credential is never sent anywhere.
  const flagged = new Set(status?.needsReauthorization ?? []);
  const errors: SyncFailure[] = accounts
    .filter((a) => a.lastError || a.needsReauthorization || flagged.has(a.email))
    .map((a) => ({
      email: a.email,
      message: a.lastError ?? NO_CREDENTIAL,
      needsReauthorization: a.needsReauthorization || flagged.has(a.email),
      lastSuccessAt: a.lastSuccessAt,
    }));

  // An address flagged at launch may have no status row at all yet — the first
  // pass has not reached it. It still has to be named.
  for (const email of flagged) {
    if (!errors.some((e) => e.email === email)) {
      errors.push({
        email,
        message: NO_CREDENTIAL,
        needsReauthorization: true,
        lastSuccessAt: null,
      });
    }
  }

  const reauth = errors.filter((e) => e.needsReauthorization).map((e) => e.email);

  const backfilling = working.filter((a) => a.phase === "backfill");
  const done = backfilling.reduce((sum, a) => sum + a.backfillDone, 0);
  const total = backfilling.reduce((sum, a) => sum + a.backfillTotal, 0);
  const fraction = total > 0 ? Math.min(1, done / total) : null;

  return {
    active: working.length > 0,
    fraction,
    done,
    total,
    label: progressLabel(status, working, done, total, errors.length - reauth.length, reauth.length),
    working,
    errors,
    reauthorize: reauth,
  };
}

function progressLabel(
  status: SyncStatus | null,
  working: AccountSyncStatus[],
  done: number,
  total: number,
  errorCount: number,
  reauthCount: number,
): string {
  if (working.length === 0) {
    // Signing in again is stated first when it covers every failure, because it
    // is the only one of the two with something for him to do. A mixture says
    // "Sync failed" — the detail names each account and its own recovery.
    if (reauthCount > 0 && errorCount === 0) {
      return reauthCount === 1
        ? "One account needs signing in again"
        : `${reauthCount} accounts need signing in again`;
    }
    if (errorCount + reauthCount > 0) {
      const n = errorCount + reauthCount;
      return n === 1 ? "Sync failed" : `Sync failed on ${n} accounts`;
    }
    if (status?.lastPassFinishedAt) return "Up to date";
    return status?.running ? "Starting sync" : "Idle";
  }

  const lead = [...working].sort(
    (a, b) => (PHASE_RANK[b.phase] ?? 0) - (PHASE_RANK[a.phase] ?? 0),
  )[0]!;
  const verb = PHASE_VERB[lead.phase] ?? "Syncing";
  const who = working.length > 1 ? `${working.length} accounts` : lead.email || "account";

  if (lead.phase === "backfill" && total > 0) {
    return `${verb} ${who} — ${done.toLocaleString()} of ${total.toLocaleString()}`;
  }
  return `${verb} ${who}`;
}
