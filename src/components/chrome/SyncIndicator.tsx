import { useEffect, useState, useSyncExternalStore } from "react";
import { useMach } from "@/hooks/useMach";
import { forcedSyncInFlight, subscribeForcedSync } from "@/lib/force-sync";
import type { SyncFailure, SyncProgress } from "@/lib/mailbox-state";
import { openPreferences } from "@/components/prefs/palette";
import { registerResolver } from "@/lib/palette/resolver";
import { listTime } from "@/lib/time";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { SYNC_DETAIL_EVENT, syncDetailResolver } from "./sync-detail";

/**
 * The sync indicator.
 *
 * The initial backfill is around thirteen minutes for a large account, which is
 * long enough that "spinner" is not an acceptable answer — it has to say what
 * it is doing and how far through it is. Progress comes from the `sync-status`
 * event; nothing here polls.
 *
 * # Where a failure goes
 *
 * "Sync failed" in eleven pixels of the bottom rail was the entire report on a
 * mailbox that had stopped updating: no account named, no reason, and nothing
 * to press. The indicator opens [`SyncDetail`] instead, which names each
 * account, quotes Google, says when that account last worked, and puts the
 * recovery for *that* failure next to it.
 *
 * Two recoveries, because there are two kinds of failure and only one of them
 * is retriable. A rate limit or a 503 wants "Sync now". A credential Google has
 * refused wants a person: no number of syncs produces a refresh token, so that
 * row offers "Sign in again" and lands in Preferences → Accounts, where
 * `73bc4af` put the button.
 *
 * Tab in the main window belongs to the mail keymap, so this button is not a
 * keyboard route on its own — ⌘K → "Sync status…" is, and it opens the same
 * panel.
 */
export function SyncIndicator() {
  const { progress, sync, actions } = useMach();
  const [open, setOpen] = useState(false);
  const syncing = useSyncExternalStore(
    subscribeForcedSync,
    () => forcedSyncInFlight(),
    () => false,
  );

  // Registered while the indicator is mounted rather than at module scope, so
  // ⌘K offers the panel exactly when there is a window for it to open in.
  useEffect(() => registerResolver(syncDetailResolver), []);

  // The keyboard route. ⌘K dispatches; the panel is the same one the click
  // opens, so there is one surface and not two that can disagree.
  useEffect(() => {
    const show = () => setOpen(true);
    window.addEventListener(SYNC_DETAIL_EVENT, show);
    return () => window.removeEventListener(SYNC_DETAIL_EVENT, show);
  }, []);

  const failures = progress.errors;

  if (!progress.active && failures.length === 0) return null;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        render={
          <button
            type="button"
            // Named for what pressing it does, which is not the same as what the
            // label says. The label is the state.
            aria-label={failures.length > 0 ? `${progress.label} — details` : undefined}
            onClick={failures.length > 0 ? undefined : () => actions.syncNow()}
            className="flex min-w-0 shrink items-center gap-2 text-micro text-faint-foreground hover:text-foreground"
          />
        }
      >
        {progress.active ? (
          <SyncBar progress={progress} className="w-16" />
        ) : (
          <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-danger" />
        )}
        <span className="truncate font-mono tabular-nums">{progress.label}</span>
        {progress.fraction !== null && (
          <span className="shrink-0 font-mono tabular-nums">
            {Math.round(progress.fraction * 100)}%
          </span>
        )}
      </PopoverTrigger>

      {failures.length > 0 && (
        <PopoverContent side="top" align="end" className="w-[26rem] max-w-[90vw] p-3">
          <SyncDetail
            failures={failures}
            lastPassFinishedAt={sync?.lastPassFinishedAt ?? null}
            // The button says "Sync <this address> again", so it syncs that
            // address. It used to sync all five, which was a small lie with a
            // real cost: retrying one rate-limited account also spent the
            // quota of the four that were fine.
            onRetry={(email) => {
              setOpen(false);
              actions.syncNow(sync?.accounts.find((a) => a.email === email)?.accountId);
            }}
            syncing={syncing}
            onSignIn={() => {
              setOpen(false);
              openPreferences("accounts");
            }}
          />
        </PopoverContent>
      )}
    </Popover>
  );
}

/**
 * One block per account that is not syncing.
 *
 * The address, Google's own words, when that account last finished a pass, and
 * the one action that applies to it. Google's text is rendered verbatim and
 * never paraphrased: it is the only thing that distinguishes a password change
 * from a withdrawn grant from the seven-day expiry an unverified OAuth app puts
 * on every token it issues, and the owner is the one who can tell them apart.
 */
export function SyncDetail({
  failures,
  lastPassFinishedAt,
  onRetry,
  onSignIn,
  now = Date.now(),
  syncing = false,
}: {
  failures: readonly SyncFailure[];
  lastPassFinishedAt: number | null;
  /** Retry *this* account. The button names it, so it may not sync the rest. */
  onRetry: (email: string) => void;
  onSignIn: () => void;
  now?: number;
  /** A forced pass is already running; the retries have nothing to add. */
  syncing?: boolean;
}) {
  return (
    <div className="flex flex-col gap-3">
      {failures.map((failure) => (
        <div key={failure.email} className="flex min-w-0 flex-col gap-1">
          <div className="flex min-w-0 items-baseline gap-2">
            <span className="min-w-0 flex-1 truncate text-body text-foreground">
              {failure.email}
            </span>
            <span className="shrink-0 font-mono text-micro tabular-nums text-faint-foreground">
              {failure.lastSuccessAt === null
                ? "Never synced"
                : `Last synced ${listTime(failure.lastSuccessAt, now)}`}
            </span>
          </div>

          {/* `break-words` because Google's descriptions run long and a reason
              that overflows its own panel is a reason nobody can read. */}
          <p className="break-words text-micro leading-snug text-danger">{failure.message}</p>

          <div>
            {failure.needsReauthorization ? (
              <Button
                size="sm"
                variant="subtle"
                aria-label={`Sign in again as ${failure.email}`}
                onClick={onSignIn}
              >
                Sign in again
              </Button>
            ) : (
              <Button
                size="sm"
                variant="subtle"
                aria-label={`Sync ${failure.email} again`}
                disabled={syncing}
                onClick={() => onRetry(failure.email)}
              >
                {syncing ? "Syncing" : "Sync now"}
              </Button>
            )}
          </div>
        </div>
      ))}

      {lastPassFinishedAt !== null && (
        <p className="font-mono text-micro tabular-nums text-faint-foreground">
          Last pass {listTime(lastPassFinishedAt, now)}
        </p>
      )}
    </div>
  );
}

/**
 * The bar itself. Determinate whenever the backfill has enumerated its queue;
 * before that there is no honest number, so it shows motion without claiming a
 * position.
 */
export function SyncBar({
  progress,
  className,
}: {
  progress: SyncProgress;
  className?: string;
}) {
  const determinate = progress.fraction !== null;
  return (
    <span
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={determinate ? 100 : undefined}
      aria-valuenow={determinate ? Math.round(progress.fraction! * 100) : undefined}
      aria-label={progress.label}
      className={cn(
        "block h-[3px] w-full shrink-0 overflow-hidden rounded-full bg-surface-raised",
        className,
      )}
    >
      <span
        className={cn("block h-full rounded-full bg-accent", !determinate && "animate-pulse")}
        style={{ width: determinate ? `${Math.max(progress.fraction! * 100, 2)}%` : "35%" }}
      />
    </span>
  );
}
