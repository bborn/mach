/**
 * The paged thread list.
 *
 * `list_threads` is keyset-paginated, and the reason matters: the sync engine
 * is inserting at the top of this list the whole time you are scrolling it, so
 * an offset would duplicate and skip rows. Pages are appended, the cursor comes
 * from the backend, and nothing here ever asks for "everything".
 *
 * A refresh — a sync pass landed, or a command changed something — refetches
 * *as many rows as are already loaded* rather than collapsing back to the first
 * page, so a scroll position two hundred rows deep survives a `threads-changed`
 * event.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import type { AccountId, LabelId, Thread, ThreadCursor } from "@/types";
import { getDataSource } from "@/lib/data";
import { toMachError } from "@/lib/ipc";
import type { MailboxError } from "@/lib/mailbox-state";

export const PAGE_SIZE = 60;
/** A refresh never refetches more than this, however deep the scroll went. */
const MAX_REFRESH = 300;

export interface ThreadStream {
  threads: Thread[];
  /** The first page is in flight and there is nothing to show yet. */
  loading: boolean;
  loadingMore: boolean;
  hasMore: boolean;
  error: MailboxError | null;
  loadMore: () => void;
  /** Refetch what is loaded, keeping depth. Safe to call on every push event. */
  refresh: () => void;
}

/**
 * Anything thrown by a data source, in the two terms the UI branches on.
 *
 * `toMachError` already classifies a missing OAuth client, and it has to run
 * here too: a source that is not the IPC one — or an error raised before the
 * IPC wrapper — must still reach the "configure me" screen rather than the
 * generic failure one.
 */
export function toMailboxError(error: unknown): MailboxError {
  const wrapped = toMachError(error);
  return { kind: wrapped.kind, message: wrapped.message };
}

export function useThreadStream(accountId: AccountId | null, labelId: LabelId): ThreadStream {
  const [threads, setThreads] = useState<Thread[]>([]);
  const [cursor, setCursor] = useState<ThreadCursor | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<MailboxError | null>(null);

  // Every fetch carries the generation it started in; a switch of account or
  // label bumps it, and late answers from the old query are dropped rather than
  // painted over the new one.
  const generation = useRef(0);
  const loadedCount = useRef(0);

  const fetchPage = useCallback(
    async (after: ThreadCursor | null, limit: number, generationAtStart: number) => {
      if (after) setLoadingMore(true);
      try {
        const page = await getDataSource().listThreads({
          accountId,
          labelId,
          limit,
          after,
        });
        if (generationAtStart !== generation.current) return;
        setThreads((previous) => {
          const next = after ? merge(previous, page.threads) : page.threads;
          loadedCount.current = next.length;
          return next;
        });
        setCursor(page.nextCursor);
        setError(null);
      } catch (caught) {
        if (generationAtStart !== generation.current) return;
        setError(toMailboxError(caught));
      } finally {
        if (generationAtStart === generation.current) {
          setLoading(false);
          setLoadingMore(false);
        }
      }
    },
    [accountId, labelId],
  );

  // A new query is a new list: reset, then fetch page one.
  useEffect(() => {
    generation.current += 1;
    loadedCount.current = 0;
    setThreads([]);
    setCursor(null);
    setLoading(true);
    void fetchPage(null, PAGE_SIZE, generation.current);
  }, [fetchPage]);

  const loadMore = useCallback(() => {
    if (!cursor || loading || loadingMore) return;
    void fetchPage(cursor, PAGE_SIZE, generation.current);
  }, [cursor, loading, loadingMore, fetchPage]);

  const refresh = useCallback(() => {
    const limit = Math.min(Math.max(loadedCount.current, PAGE_SIZE), MAX_REFRESH);
    void fetchPage(null, limit, generation.current);
  }, [fetchPage]);

  return {
    threads,
    loading,
    loadingMore,
    hasMore: cursor !== null,
    error,
    loadMore,
    refresh,
  };
}

/** Append, dropping any row the previous page already carried. */
function merge(previous: Thread[], incoming: Thread[]): Thread[] {
  if (incoming.length === 0) return previous;
  const seen = new Set(previous.map((t) => t.id));
  return [...previous, ...incoming.filter((t) => !seen.has(t.id))];
}
