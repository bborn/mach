import { useMach } from "@/hooks/useMach";
import type { SyncProgress } from "@/lib/mailbox-state";
import { cn } from "@/lib/utils";

/**
 * The sync indicator.
 *
 * The initial backfill is around thirteen minutes for a large account, which is
 * long enough that "spinner" is not an acceptable answer — it has to say what
 * it is doing and how far through it is. Progress comes from the `sync-status`
 * event; nothing here polls.
 */
export function SyncIndicator() {
  const { progress, sync, actions } = useMach();

  if (!progress.active && progress.errors.length === 0) return null;

  const title = sync
    ? sync.accounts
        .map((a) => {
          const detail = a.lastError
            ? `error: ${a.lastError}`
            : a.phase === "backfill" && a.backfillTotal > 0
              ? `${a.backfillDone.toLocaleString()} / ${a.backfillTotal.toLocaleString()}`
              : a.phase;
          return `${a.email} — ${detail}`;
        })
        .join("\n")
    : undefined;

  return (
    <button
      type="button"
      title={title}
      onClick={actions.syncNow}
      className="flex min-w-0 shrink items-center gap-2 text-micro text-faint-foreground hover:text-foreground"
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
    </button>
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
