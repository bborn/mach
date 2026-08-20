import { Check, CircleAlert, LoaderCircle, ShieldQuestion, X } from "lucide-react";
import { cn } from "@/lib/utils";
import type { AgentSession } from "@/lib/agent";

/**
 * One session in the bottom bar.
 *
 * Titled by its task, not by "Agent" — several run at once and "which one is
 * the Tuesday reply" has to be answerable at a glance. The state icon carries
 * the only thing that changes the owner's behaviour: whether it needs him.
 */
export function AgentPill({
  session,
  active,
  onOpen,
  onClose,
}: {
  session: AgentSession;
  active: boolean;
  onOpen: () => void;
  onClose: () => void;
}) {
  const waiting = session.status === "awaitingApproval";
  const failed = session.status === "failed";

  return (
    <span
      className={cn(
        "group flex h-6 shrink-0 items-center gap-1.5 rounded-[var(--radius)] pl-2 pr-1 text-micro",
        active ? "bg-surface-raised text-foreground" : "text-muted-foreground hover:text-foreground",
        waiting && "bg-warning/15 text-warning",
        failed && "text-danger",
      )}
    >
      <button
        type="button"
        onClick={onOpen}
        title={session.title}
        className="flex items-center gap-1.5 whitespace-nowrap"
      >
        <StatusIcon session={session} />
        <span className="max-w-[16rem] truncate">{session.title}</span>
      </button>
      <button
        type="button"
        onClick={onClose}
        aria-label={`Close ${session.title}`}
        className="rounded-[var(--radius)] p-0.5 text-faint-foreground opacity-0 transition-opacity hover:text-foreground group-hover:opacity-100"
      >
        <X size={11} strokeWidth={2} />
      </button>
    </span>
  );
}

function StatusIcon({ session }: { session: AgentSession }) {
  switch (session.status) {
    case "running":
      return (
        <LoaderCircle
          size={11}
          strokeWidth={2}
          className="animate-spin text-accent"
          aria-label="Working"
        />
      );
    case "awaitingApproval":
      return <ShieldQuestion size={11} strokeWidth={2} aria-label="Waiting for you" />;
    case "failed":
      return <CircleAlert size={11} strokeWidth={2} aria-label="Failed" />;
    default:
      return <Check size={11} strokeWidth={2} className="text-success" aria-label="Done" />;
  }
}
