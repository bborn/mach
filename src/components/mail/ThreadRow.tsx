import { Check, Paperclip, Star } from "lucide-react";
import { memo, type MouseEvent } from "react";
import type { Thread, ThreadId } from "@/types";
import { Monogram } from "@/components/ui/monogram";
import { listTime } from "@/lib/time";
import { cn } from "@/lib/utils";

interface ThreadRowProps {
  thread: Thread;
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
  /**
   * Mark the preview as your own unsent text.
   *
   * Defaults to "does this conversation carry Gmail's `DRAFT` label", which is
   * the fact; the Drafts mailbox passes `false`, because there the answer is
   * the same on every row and the mark would be furniture.
   */
  draft?: boolean;
  /**
   * The row was clicked, with whichever modifiers were down.
   *
   * It carries its own id and reads the modifiers off the event itself, so that
   * the list can hand *every* row the same function. The obvious spelling —
   * `onSelect={(e) => actions.clickThread(thread.id, …)}` written in the list —
   * builds a new closure per row per render, and a new closure fails the shallow
   * prop compare, so the `memo` around this component never once held. Moving
   * the cursor one row re-rendered every row on screen.
   */
  onSelect: (id: ThreadId, modifiers: { extend: boolean; toggle: boolean }) => void;
}

/** What a click means, given what was held down. */
function modifiersOf(event: MouseEvent): { extend: boolean; toggle: boolean } {
  return { extend: event.shiftKey, toggle: event.metaKey || event.ctrlKey };
}

/** The label Gmail files an unsent message under. Not a local invention. */
export const GMAIL_DRAFT = "DRAFT";

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
 *
 * # The preview line can be your own words
 *
 * A conversation whose most recent message is your unsent draft shows that
 * draft as the preview, because it *is* the last thing on the conversation and
 * hiding it would put stale text on the row. What was missing is whose words
 * they are: an unsent reply read exactly like a received one. So the line is
 * prefixed with **Draft**, in words rather than in colour, which is the same
 * mark the message carries inside the thread.
 *
 * # Why the `memo` is worth keeping honest
 *
 * A mailbox is up to three hundred of these, and `j`/`k` changes the `cursor`
 * prop on exactly two of them. Everything else the row is given is already
 * identity-stable across a cursor move — `thread` survives `reconcile`,
 * `unread` comes from a `useCallback` — so the `memo` should mean
 * two renders per keystroke and does.
 *
 * It did not, for as long as the list built the click handler inline per row.
 * That is what `onSelect` taking an id is for; see the prop.
 */
export const ThreadRow = memo(function ThreadRow({
  thread,
  unread,
  cursor,
  checked,
  selecting,
  context,
  draft = thread.labelIds.includes(GMAIL_DRAFT),
  onSelect,
}: ThreadRowProps) {
  const from = thread.participants[0];
  const sender = from?.name ?? from?.email ?? "—";

  return (
    <div
      role="option"
      aria-selected={checked || cursor}
      data-thread-id={thread.id}
      data-checked={checked || undefined}
      onClick={(event) => onSelect(thread.id, modifiersOf(event))}
      className={cn(
        "group relative flex h-row cursor-default items-center pl-4 pr-3",
        "border-b border-border/60 text-list",
        // The tint under the cursor and under a tick are the same two
        // colours the calendar's blocks cross-fade between; this row was
        // the only surface still snapping to them.
        "transition-colors duration-100 ease-out motion-reduce:transition-none",
        checked
          ? "bg-row-selected"
          : cursor
            ? selecting
              ? "bg-row-hover"
              : "bg-row-selected"
            : "hover:bg-row-hover",
      )}
    >
      {/*
        The tick column, opening and closing rather than appearing.

        It is drawn on every row now and given a width of zero when there is no
        selection, which is the same bargain the old conditional made — §the row
        pays nothing for it until the mode exists — reached by a route that can
        be animated. Entering selection used to shove the sender, the subject
        and the preview 22px to the right between one frame and the next, on
        every visible row at once.

        One property animates: the wrapper's width. The 8px that separates the
        tick from the tile is `mr-2` *inside* the wrapper, so it is carried by
        the same transition instead of being a flex gap that snaps. `overflow-
        hidden` is what lets the button keep its 14px while the column around it
        is still narrower than that.

        Out of the accessibility tree and off the tab ring while it is shut: a
        checkbox nobody can see is not a checkbox anybody should reach.
      */}
      <span
        aria-hidden={!selecting}
        className={cn(
          "flex shrink-0 items-center overflow-hidden",
          "transition-[width] duration-150 ease-out motion-reduce:transition-none",
          selecting ? "w-[22px]" : "w-0",
        )}
      >
        <button
          type="button"
          role="checkbox"
          aria-checked={checked}
          aria-label={checked ? "Deselect conversation" : "Select conversation"}
          tabIndex={selecting ? undefined : -1}
          onClick={(event) => {
            // The row underneath opens the conversation; the tick must not.
            event.stopPropagation();
            onSelect(thread.id, { extend: false, toggle: true });
          }}
          className={cn(
            "mr-2 flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-[3px] border",
            "transition-colors duration-100 ease-out motion-reduce:transition-none",
            checked
              ? "border-accent bg-accent text-accent-foreground"
              : "border-border-strong text-transparent",
          )}
        >
          {selecting && <Check size={10} strokeWidth={3} />}
        </button>
      </span>

      {/*
        Unread, in the left margin beside all three lines rather than on one of
        them: it is a fact about the conversation, not about its sender.

        Absolutely placed, because it used to sit in the row's flex flow and
        *reserve* its 6px whether or not it was drawn. On a read row that is
        6px of nothing plus the gap after it, so the monogram carried a hole
        on its left and looked shoved against the text it belongs beside. Out
        of flow it costs the row nothing when it is invisible, which is most
        rows.
      */}
      <span
        className={cn(
          "absolute left-2 h-1.5 w-1.5 rounded-full",
          unread ? "bg-accent" : "bg-transparent",
        )}
      />

      <Monogram name={from?.name} email={from?.email} className="mr-4" />

      {/*
        4px between the three lines. It was 1px, and the row read "a little
        tight" in a report from inside the app.

        The three lines sat 1px apart inside a row that kept ~8.75px clear above
        the sender and below the preview. The space around the group was nine
        times the space within it, which by proximity makes the group one
        paragraph: sender, subject and preview fused into a single block, and
        the type ramp was left carrying the hierarchy alone. 4px puts the two
        figures within about 2:1 of each other, and the three lines separate.

        The leading was left alone. Each of these is a single truncated line, so
        `leading-tight` only sets how much symmetric padding the line box
        carries — it would be this same gap under another name, spelled where
        nobody reading the row's rhythm would look for it.

        Adding up the line boxes: 16.25 + 4 + 17.5 + 4 + 13.75 = 55.5px, which
        is what `--spacing-row` is sized against.
      */}
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex items-center gap-1">
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

        {/*
          Three lines used to be three 13px lines separated only by weight and by
          how grey they were, so the row read as a texture. The subject, which is
          what anyone reads a list of mail for, sits one step up at `text-body`;
          the preview sits one step down at `text-micro`. That puts three pixels
          between the line you scan for and the line you glance at.

          It cost no height: 14px and 11px at `leading-tight` come to the same
          47.5px the three 13px lines did.

          The preview goes darker as it goes smaller, `muted` rather than
          `faint`. The size is already carrying the demotion, and 11px at the
          faint step would be paying for it twice.
        */}
        <div
          className={cn(
            "truncate text-body leading-tight",
            unread ? "font-medium text-foreground" : "text-foreground/80",
          )}
        >
          {thread.subject}
        </div>

        <div className="truncate text-micro leading-tight text-muted-foreground">
          {draft && <span className="font-medium text-danger">Draft</span>}
          {draft && <span className="text-faint-foreground"> · </span>}
          {thread.snippet}
        </div>
      </div>
    </div>
  );
});
