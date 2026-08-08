import { Check, Paperclip, Star } from "lucide-react";
import { memo, type MouseEvent } from "react";
import type { Account, Thread } from "@/types";
import { ACCOUNT_BG } from "@/lib/colors";
import { listTime } from "@/lib/time";
import { cn } from "@/lib/utils";

interface ThreadRowProps {
  thread: Thread;
  account: Account | undefined;
  unread: boolean;
  /** The cursor is on this row — what `e`, `r` and the reading pane follow. */
  cursor: boolean;
  /** This row is part of a multi-selection. */
  checked: boolean;
  /** A selection exists somewhere in the list, so the tick column is live. */
  selecting: boolean;
  /**
   * A word about where this row came from — the mailbox it lives in, for a
   * list that is not one mailbox. The search view sets it; the mailbox list
   * never does, because there the answer is the same on every row and the
   * space belongs to the subject.
   */
  context?: string;
  onSelect: (event: MouseEvent) => void;
  onToggle: () => void;
}

/**
 * One line. `--spacing-row` is 2.25rem, which puts 18–20 rows on screen where
 * Spark manages 9 — the whole point of the density target. Everything on the
 * row has a fixed or truncating box so nothing can ever push the time column
 * out of alignment.
 *
 * # Selection
 *
 * The tick column exists only while something is selected. A permanent
 * checkbox column would cost every row ~20px of subject width forever to serve
 * a gesture used a few times a day — Superhuman's answer, and the right one, is
 * that the affordance appears when the mode does.
 *
 * While a selection is live the accent tint means "ticked" rather than "under
 * the cursor", and the cursor falls back to the quieter hover tone. Two states
 * on one row need two weights, and the loud one belongs to the set that is
 * about to be archived.
 */
export const ThreadRow = memo(function ThreadRow({
  thread,
  account,
  unread,
  cursor,
  checked,
  selecting,
  context,
  onSelect,
  onToggle,
}: ThreadRowProps) {
  const sender = thread.participants[0]?.name ?? thread.participants[0]?.email ?? "—";

  return (
    <div
      role="option"
      aria-selected={checked || cursor}
      data-thread-id={thread.id}
      data-checked={checked || undefined}
      onClick={onSelect}
      className={cn(
        "group relative flex h-row cursor-default items-center gap-2 pl-3 pr-2.5",
        "border-b border-border/60 text-list",
        checked
          ? "bg-row-selected"
          : cursor
            ? selecting
              ? "bg-row-hover"
              : "bg-row-selected"
            : "hover:bg-row-hover",
      )}
    >
      {/* Per-account identity. The one place colour is allowed to be decorative. */}
      <span
        className={cn(
          "absolute inset-y-0 left-0 w-[2px]",
          account ? ACCOUNT_BG[account.colorIndex] : "bg-border",
        )}
        title={account?.email}
      />

      {selecting && (
        <button
          type="button"
          role="checkbox"
          aria-checked={checked}
          aria-label={checked ? "Deselect conversation" : "Select conversation"}
          onClick={(event) => {
            // The row underneath opens the conversation; the tick must not.
            event.stopPropagation();
            onToggle();
          }}
          className={cn(
            "flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-[3px] border",
            checked
              ? "border-accent bg-accent text-accent-foreground"
              : "border-border-strong text-transparent",
          )}
        >
          <Check size={10} strokeWidth={3} />
        </button>
      )}

      <span
        className={cn(
          "h-1.5 w-1.5 shrink-0 rounded-full",
          unread ? "bg-accent" : "bg-transparent",
        )}
      />

      <span
        className={cn(
          "w-28 shrink-0 truncate",
          unread ? "font-medium text-foreground" : "text-muted-foreground",
        )}
      >
        {sender}
      </span>

      {thread.messageCount > 1 && (
        <span className="shrink-0 font-mono text-micro tabular-nums text-faint-foreground">
          {thread.messageCount}
        </span>
      )}

      <span className="flex min-w-0 flex-1 items-baseline gap-1.5 overflow-hidden">
        <span
          className={cn(
            "max-w-[66%] shrink-0 truncate",
            unread ? "font-medium text-foreground" : "text-foreground/80",
          )}
        >
          {thread.subject}
        </span>
        <span className="min-w-0 flex-1 truncate text-faint-foreground">{thread.snippet}</span>
      </span>

      {context && (
        <span className="shrink-0 rounded-[3px] border border-border px-1 text-micro text-faint-foreground">
          {context}
        </span>
      )}

      {thread.starred && (
        <Star size={12} strokeWidth={2} className="shrink-0 fill-warning text-warning" />
      )}
      {thread.hasAttachment && (
        <Paperclip size={12} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
      )}

      <span
        className={cn(
          "w-[4.5rem] shrink-0 text-right font-mono text-micro tabular-nums",
          unread ? "text-muted-foreground" : "text-faint-foreground",
        )}
      >
        {listTime(thread.timestamp)}
      </span>
    </div>
  );
});
