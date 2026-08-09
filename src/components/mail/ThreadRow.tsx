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
 * Three lines: who, what, and a taste of it.
 *
 * The row used to be one line, with sender, subject, snippet and date sharing
 * a width none of them could have. What that bought was rows on screen; what it
 * cost was the subject, which is the thing you are actually reading the list
 * for — every one of them truncated mid-word, so scanning the list meant
 * opening conversations to find out what they were about. Splitting the row
 * gives the subject the full width of the list to itself, and that is the whole
 * trade: fewer conversations visible, each of them legible.
 *
 * The height is fixed (`--spacing-row`) rather than grown from the content, so
 * the list scrolls in a regular rhythm and the date column lands on the same
 * pixel on every row. Everything on line one is either fixed-width or
 * truncating for the same reason: nothing a sender puts in a display name can
 * push the time out of alignment.
 *
 * Line one carries the metadata — sender, message count, the star and
 * attachment marks, the date — because those are what you *sort* by eye. They
 * are one cluster pinned to the right edge, and the date inside it shrink-wraps
 * its text rather than sitting in a fixed-width box. It used to sit in one, so
 * that "Yesterday" and "Fri" would share a left edge, and the cost was a hole:
 * on every row with a shorter time the marks were pushed a couple of
 * centimetres clear of the digits and looked like they belonged to nothing.
 * What actually has to line up is the *right* edge of the dates, which the
 * cluster's own alignment gives for free — `tabular-nums` keeps the digits from
 * jittering as the minute changes.
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

      {/* Unread, in the left margin beside all three lines rather than on one
          of them: it is a fact about the conversation, not about its sender. */}
      <span
        className={cn(
          "h-1.5 w-1.5 shrink-0 rounded-full",
          unread ? "bg-accent" : "bg-transparent",
        )}
      />

      <div className="flex min-w-0 flex-1 flex-col gap-px">
        <div className="flex items-center gap-1.5">
          <span
            className={cn(
              "min-w-0 truncate leading-tight",
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

          {context && (
            <span className="shrink-0 rounded-[3px] border border-border px-1 text-micro text-faint-foreground">
              {context}
            </span>
          )}

          <span className="ml-auto flex shrink-0 items-center gap-1 pl-2">
            {thread.starred && (
              <Star size={12} strokeWidth={2} className="shrink-0 fill-warning text-warning" />
            )}
            {thread.hasAttachment && (
              <Paperclip size={12} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
            )}
            <span
              className={cn(
                "whitespace-nowrap font-mono text-micro tabular-nums leading-tight",
                unread ? "text-muted-foreground" : "text-faint-foreground",
              )}
            >
              {listTime(thread.timestamp)}
            </span>
          </span>
        </div>

        <div
          className={cn(
            "truncate leading-tight",
            unread ? "font-medium text-foreground" : "text-foreground/80",
          )}
        >
          {thread.subject}
        </div>

        <div className="truncate leading-tight text-faint-foreground">{thread.snippet}</div>
      </div>
    </div>
  );
});
