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
  errors: { email: string; message: string }[];
  /**
   * Addresses that need signing in again, separated out from `errors`.
   *
   * Same sentence in the status bar, different next action: a sync error is
   * something to retry, and a dead Keychain entry is something to fix in
   * Preferences → Accounts. The indicator branches on this.
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

export function syncProgress(status: SyncStatus | null): SyncProgress {
  const accounts = status?.accounts ?? [];
  const working = accounts.filter(isActive);
  const errors = accounts
    .filter((a) => a.lastError)
    .map((a) => ({ email: a.email, message: a.lastError! }));

  // A dead Keychain entry is not a sync error the engine can retry out of — it
  // needs the account authorizing again — but it belongs in the same list,
  // because from the status bar's point of view it is the same sentence.
  const reauth = (status?.needsReauthorization ?? []).filter(
    (email) => !errors.some((e) => e.email === email),
  );
  for (const email of reauth) {
    errors.push({ email, message: `${email} needs signing in again` });
  }

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
    if (errorCount > 0) return errorCount === 1 ? "Sync failed" : `Sync failed on ${errorCount} accounts`;
    if (reauthCount > 0) {
      return reauthCount === 1
        ? "One account needs signing in again"
        : `${reauthCount} accounts need signing in again`;
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
