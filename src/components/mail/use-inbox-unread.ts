/**
 * Unread inbox counts, per account, for the rail's badges.
 *
 * The rail used to count these out of `allThreads`, which is the *visible*
 * stream — already scoped to `ui.accountId` and `ui.labelId` and paged to
 * whatever has been scrolled. That made the badges lie in three separate ways:
 * filtering to one account zeroed every other account's badge, walking into
 * Sent zeroed all of them, and an inbox deeper than one page under-counted.
 * A number in a sidebar is a claim about the whole mailbox, so it has to come
 * from a query about the whole mailbox.
 *
 * So this asks the data source its own question — unread, in an inbox, across
 * every account — and keeps it current off the same `threads-changed` push the
 * list uses, coalesced on the same 600ms window because a backfill emits that
 * event hundreds of times a minute.
 *
 * What it deliberately does *not* do is wait for the backend to agree. An
 * archive-everything gesture hides fifty rows optimistically and the write is
 * still in flight; if the badge only moved when the query came back, the rail
 * would sit there insisting on a number the list had already stopped showing.
 * `countByAccount` takes the same optimistic id sets the list filters on, so
 * the badge falls in the same frame the rows do — and if the write fails and
 * the ids are rolled back, the badge comes back with them.
 */

import { useEffect, useMemo, useState } from "react";
import type { AccountId, LabelId, Thread, ThreadId } from "@/types";
import { getDataSource, type Unsubscribe } from "@/lib/data";

/**
 * How many unread inbox threads are counted before the badge gives up on being
 * exact.
 *
 * Nobody triages a five-figure inbox from a sidebar badge, and asking SQLite
 * for every row of one to render a two-digit number is the wrong trade. Past
 * this the rail says `500+`, which is the honest thing and the thing every
 * other mail client does.
 */
export const UNREAD_LIMIT = 500;

/** Matches the list's coalesce window, for the same reason. */
const REFRESH_COALESCE_MS = 600;

export interface InboxUnread {
  /** Unread inbox threads per account. Absent means zero. */
  byAccount: Map<AccountId, number>;
  total: number;
  /** There were more than {@link UNREAD_LIMIT}; the numbers are a floor. */
  capped: boolean;
}

/**
 * Count unread inbox threads per account, minus anything the UI has already
 * optimistically taken out of the inbox or marked read.
 */
export function countByAccount(
  threads: readonly Thread[],
  suppressed: ReadonlySet<ThreadId>,
): Map<AccountId, number> {
  const counts = new Map<AccountId, number>();
  for (const thread of threads) {
    if (suppressed.has(thread.id)) continue;
    counts.set(thread.accountId, (counts.get(thread.accountId) ?? 0) + 1);
  }
  return counts;
}

/**
 * The unread inbox threads of every account, kept fresh.
 *
 * Exported separately from {@link useInboxUnread} so the counting can be tested
 * as a pure function without a data source.
 */
export function useUnreadInboxThreads(
  labelId: LabelId = "INBOX",
): { threads: Thread[]; capped: boolean } {
  const [state, setState] = useState<{ threads: Thread[]; capped: boolean }>({
    threads: [],
    capped: false,
  });

  useEffect(() => {
    let live = true;
    let timer: number | null = null;
    let off: Unsubscribe | undefined;

    const load = () => {
      void getDataSource()
        .listThreads({
          accountId: null,
          labelId,
          unreadOnly: true,
          limit: UNREAD_LIMIT,
        })
        .then((page) => {
          if (!live) return;
          setState({ threads: page.threads, capped: page.nextCursor !== null });
        })
        .catch(() => {
          // A failed count is not worth a status message: the mailbox itself
          // will have said so already, and a badge that stops updating is a
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
      off?.();
    };
  }, [labelId]);

  return state;
}

/** The counts the rail paints, with the optimistic layer already applied. */
export function useInboxUnread(
  suppressed: ReadonlySet<ThreadId>,
  labelId: LabelId = "INBOX",
): InboxUnread {
  const { threads, capped } = useUnreadInboxThreads(labelId);
  return useMemo(() => {
    const byAccount = countByAccount(threads, suppressed);
    let total = 0;
    for (const n of byAccount.values()) total += n;
    return { byAccount, total, capped };
  }, [threads, suppressed, capped]);
}
