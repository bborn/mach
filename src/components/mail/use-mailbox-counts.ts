/**
 * How many drafts, and how many snoozed. Kept current.
 *
 * # Why these two and nothing else
 *
 * Drafts is a total rather than an unread count, because a draft has no read
 * state — the owner's Drafts mailbox is 1 thread and 0 unread, so an unread
 * count there would show nothing, ever. Snoozed is a total for a different
 * reason: it is a queue of conversations coming back, and the number is small
 * and prompts something.
 *
 * Everything else on the rail is left alone. Gmail shows a Spam count and it is
 * noise — 354 of the owner's 384 spam threads are unread, because nobody reads
 * spam — and Sent, Starred, All, Archive and Promotions are archives of
 * finished things whose numbers only grow.
 *
 * # Where the numbers come from
 *
 * Local SQLite, over one `mailbox_counts` call, asked with the same predicate
 * the mailboxes themselves are listed by — see `db::queries::count_mailbox`.
 * The `labels` table carries no counts and nothing is asked of Google.
 *
 * # When they are re-read
 *
 * Never on a render or a keystroke. Twice on the same clocks the rest of the
 * rail runs on: `threads-changed`, which covers a snooze, a wake, a sync pass
 * and every command; and {@link mailboxCountsChanged}, which `useMach`'s
 * `reload()` fires and which is what covers the composer — saving a draft and
 * discarding one both write the Drafts mailbox from a path with no
 * `threads-changed` behind it.
 *
 * The subscription is coalesced on the same 600ms window as `use-inbox-unread`,
 * for the same reason: a backfill emits `threads-changed` hundreds of times a
 * minute.
 */

import { useEffect, useState } from "react";
import { getDataSource, type MailboxCounts, type Unsubscribe } from "@/lib/data";

/** Matches the list's coalesce window, and the unread badge's. */
const REFRESH_COALESCE_MS = 600;

const EMPTY: MailboxCounts = { drafts: 0, snoozed: 0 };

/** Everything currently listening. Module-level, so a write reaches all of it. */
const listeners = new Set<() => void>();

/**
 * Something wrote a draft, or threw one away. Whoever is showing the counts
 * should look again.
 *
 * The composer's writes do not emit `threads-changed` — an autosave is not a
 * mailbox event and making it one would refetch every list on every keystroke —
 * so this rides `useMach`'s `reload()`, which is already the signal the
 * composer sends when the Drafts mailbox has changed under it.
 */
export function mailboxCountsChanged(): void {
  for (const listener of listeners) listener();
}

export function useMailboxCounts(): MailboxCounts {
  const [counts, setCounts] = useState<MailboxCounts>(EMPTY);

  useEffect(() => {
    let live = true;
    let timer: number | null = null;
    let off: Unsubscribe | undefined;

    const load = () => {
      void getDataSource()
        .mailboxCounts()
        .then((next) => {
          if (!live) return;
          // Same object identity when nothing moved, so the rail's memo does
          // not rebuild every row on every coalesced refresh.
          setCounts((current) =>
            current.drafts === next.drafts && current.snoozed === next.snoozed ? current : next,
          );
        })
        .catch(() => {
          // A count that cannot be read is not worth a message. The mailbox
          // itself will have said so, and a number that stops moving is a
          // quieter wrong than one that clears itself to zero.
        });
    };

    const schedule = () => {
      if (timer !== null) return;
      timer = window.setTimeout(() => {
        timer = null;
        load();
      }, REFRESH_COALESCE_MS);
    };

    load();
    listeners.add(load);
    void getDataSource()
      .onThreadsChanged(schedule)
      .then((dispose) => {
        if (live) off = dispose;
        else dispose();
      })
      .catch(() => {});

    return () => {
      live = false;
      if (timer !== null) window.clearTimeout(timer);
      listeners.delete(load);
      off?.();
    };
  }, []);

  return counts;
}
