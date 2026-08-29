/**
 * The messages that did not go, kept current.
 *
 * # Why a hook and not a piece of app state
 *
 * Two surfaces read this — the rail's row, which is how the owner finds out
 * something is wrong, and the panel that lists them — and they are not in the
 * same subtree. The obvious answer is to hang it off `useMach` with everything
 * else, and that would make every list refetch and every optimistic star drag a
 * query about the outbox along with it. This is a table of four rows that
 * changes when a send fails and when the owner acts on one, which is a
 * different clock from the mailbox's.
 *
 * So: one query, subscribed to twice, and a module-level notifier so that
 * discarding a message in the panel clears the badge in the rail in the same
 * frame. The pattern is `subscribeLinkFailures`', for the same reason — the
 * thing that knows is never the thing that can say so.
 *
 * # Why it is not polled
 *
 * `send-failed` is pushed from Rust the moment a message stops trying, from
 * whichever process ran the flush: the window's own timer, `mach send` from a
 * shell, an agent's reply. The list is re-read on that push and on mount, and
 * mount is the half that matters after a restart — the four failures in the
 * owner's store are eighteen days old, and no event was ever going to arrive
 * for them again.
 */

import { useEffect, useState } from "react";
import { listFailedSends, type FailedSend } from "@/lib/compose";
import { getDataSource, type Unsubscribe } from "@/lib/data";

/** Everything currently listening. Module-level, so a write reaches all of it. */
const listeners = new Set<() => void>();

/**
 * Something acted on the queue; whoever is showing it should look again.
 *
 * Called by the panel after a retry or a discard. Rust is the authority — the
 * refetch is what moves the badge, not an optimistic edit — because both of
 * those operations are compare-and-swaps that can legitimately do nothing, and
 * a badge that decremented anyway would be a second lie about the outbox.
 */
export function unsentChanged(): void {
  for (const listener of listeners) listener();
}

export function useUnsent(): { failed: FailedSend[] } {
  const [failed, setFailed] = useState<FailedSend[]>([]);

  useEffect(() => {
    let live = true;
    let off: Unsubscribe | undefined;

    const load = () => {
      void listFailedSends()
        .then((rows) => {
          if (live) setFailed(rows);
        })
        .catch(() => {
          // A queue that cannot be read is not worth a second message: the send
          // that failed has already said so, and a badge that clears itself to
          // zero would be worse than one that stops moving.
        });
    };

    load();
    listeners.add(load);
    void getDataSource()
      .onSendFailed(load)
      .then((dispose) => {
        if (live) off = dispose;
        else dispose();
      })
      .catch(() => {});

    return () => {
      live = false;
      listeners.delete(load);
      off?.();
    };
  }, []);

  return { failed };
}
