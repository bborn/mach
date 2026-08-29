import { MailWarning } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import {
  discardSend,
  flushOutbox,
  formatRecipients,
  retrySend,
  type FailedSend,
} from "@/lib/compose";
import { messageTime } from "@/lib/time";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Overlay } from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import { unsentChanged, useUnsent } from "./use-unsent";

/**
 * The messages that did not go.
 *
 * # Why this exists at all
 *
 * `compose_outbox` has had a `failed` state since the queue was written, and
 * until now nothing on earth rendered it. Four rows accumulated in the owner's
 * store over eighteen days — the newest a reply to a client — and he learned
 * they existed when somebody ran a `SELECT` against his database for him. The
 * standing rule in `CLAUDE.md` is that a write Google refuses has to say so;
 * this is that rule on the one path where it costs the most.
 *
 * # Why a panel and not a mailbox
 *
 * A row in the rail is what tells him. This is what he does about it, and it
 * cannot be a mailbox: these are not conversations, they have no thread to open
 * — a permanent failure takes the optimistic copy back out of the conversation,
 * which is what made the message disappear — and the list is two items long on
 * a bad month. A destination in the rail with its own header, its own cursor
 * and its own empty state would be a mailbox's worth of furniture around four
 * rows that want to be dealt with and gone.
 *
 * # What a row carries, and why each part
 *
 * The subject names the message, except when there is not one — two of the
 * owner's four have no subject at all — so the recipients are on the row too
 * and not behind a disclosure. Google's reason is verbatim: "Invalid To header"
 * and "resource not found" are different problems with different answers, and
 * summarising them into "could not send" would throw away the only part that
 * decides what to do next. The words he wrote are underneath, because a message
 * whose address Google refuses cannot be retried into working and the text is
 * then the only thing left worth having.
 *
 * # Retry, discard, and what is deliberately not here
 *
 * Retry is the owner's gesture. Nothing retries on its own — a row reaches
 * `failed` because Google gave an answer that no amount of waiting changes, so
 * something has to be fixed by a person before another attempt means anything.
 * Discard is here because otherwise a message that can never send is a badge
 * that can never clear, and the rail would end up crying wolf permanently.
 *
 * There is no edit-and-resend. It is the right feature for "Invalid To header"
 * and it is not a button: turning the queued RFC822 back into a draft means
 * reconstructing the attachments as well as the words, and a reopened draft
 * that had quietly dropped a file would be a fresh instance of exactly the
 * failure this surface exists to end.
 */
export function UnsentPanel() {
  const { ui, actions } = useMach();
  const open = ui.unsentOpen;
  const { failed } = useUnsent();
  /** Ids with a decision in flight, so a double press cannot fire twice. */
  const [busy, setBusy] = useState<readonly string[]>([]);
  /*
   * Where the keyboard lands, and what the focus trap returns to.
   *
   * `Overlay` finds its own first stop with a query for fields —
   * `input, textarea, [tabindex]` — and this panel has none of those, only
   * buttons. Without a ref the panel opens with focus still on the mailbox
   * behind it, ⇥ walks straight out of the dialog, and the trap has nothing to
   * pull it back to. Naming the first Retry fixes both halves at once.
   */
  const first = useRef<HTMLButtonElement>(null);

  // Nothing to report is a closed panel, whether it emptied while it was up or
  // the rail row went away underneath it.
  useEffect(() => {
    if (open && failed.length === 0) actions.setUnsent(false);
  }, [open, failed.length, actions]);

  const act = useCallback(
    async (row: FailedSend, what: "retry" | "discard") => {
      if (busy.includes(row.id)) return;
      setBusy((ids) => [...ids, row.id]);
      try {
        const ok = what === "retry" ? await retrySend(row.id) : await discardSend(row.id);
        if (!ok) {
          actions.setStatus("Not in the queue any more", "error");
        } else if (what === "retry") {
          /*
           * Retry has to *send*, not just re-queue.
           *
           * `retrySend` puts the row back to `holding` and nothing else looks
           * at the queue until the next compose call or the next launch —
           * which, from in here, is a button that appears to do nothing and a
           * message that leaves some hours later. So the flush is part of the
           * gesture. The row stays on screen for the length of it, because the
           * list is only re-read afterwards: a row that vanished and came back
           * a second later would be a worse answer than one that waited.
           *
           * Nothing is said when it works. The row leaving the list is the
           * whole message, and Rust says the other thing itself — a second
           * refusal comes back through `send-failed` with Google's reason.
           */
          await flushOutbox().catch(() => undefined);
        }
      } catch {
        actions.setStatus("Could not reach the queue", "error");
      } finally {
        setBusy((ids) => ids.filter((id) => id !== row.id));
        // Every surface showing the queue re-reads, this one included.
        unsentChanged();
      }
    },
    [busy, actions],
  );

  useKeyBindings([
    {
      keys: "g u",
      group: "Go to",
      description: "Unsent",
      // Registered here rather than in `App` so the key and the surface it
      // opens live in one file, and so it is dead while there is nothing to
      // show — a jump key that opens an empty panel teaches the wrong thing.
      when: () => !open && failed.length > 0,
      handler: () => actions.setUnsent(true),
    },
  ]);

  return (
    <Overlay
      open={open}
      onClose={() => actions.setUnsent(false)}
      labelledBy="unsent-title"
      initialFocus={first}
      className="self-start"
    >
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-border px-3">
        <MailWarning size={14} strokeWidth={1.75} className="shrink-0 text-warning" />
        <span id="unsent-title" className="text-list text-foreground">
          Unsent
        </span>
        <span className="ml-auto font-mono text-micro tabular-nums text-faint-foreground">
          {failed.length}
        </span>
      </div>

      <div className="min-h-0 overflow-y-auto">
        {failed.map((row, index) => (
          <UnsentRow
            key={row.id}
            row={row}
            retryRef={index === 0 ? first : undefined}
            busy={busy.includes(row.id)}
            onRetry={() => void act(row, "retry")}
            onDiscard={() => void act(row, "discard")}
          />
        ))}
      </div>

      <footer className="flex h-7 shrink-0 items-center gap-3 border-t border-border px-3">
        <span className="flex items-center gap-1">
          <Kbd keys="tab" />
          <span className="text-micro text-faint-foreground">move</span>
        </span>
        <span className="flex items-center gap-1">
          <Kbd keys="escape" />
          <span className="text-micro text-faint-foreground">close</span>
        </span>
      </footer>
    </Overlay>
  );
}

function UnsentRow({
  row,
  busy,
  retryRef,
  onRetry,
  onDiscard,
}: {
  row: FailedSend;
  busy: boolean;
  retryRef?: React.RefObject<HTMLButtonElement | null>;
  onRetry: () => void;
  onDiscard: () => void;
}) {
  const recipients = [
    { label: "To", list: row.to },
    { label: "Cc", list: row.cc },
    { label: "Bcc", list: row.bcc },
  ].filter((line) => line.list.length > 0);

  return (
    <article
      data-unsent-id={row.id}
      className="border-b border-border px-3 py-2.5 last:border-b-0"
    >
      <div className="flex items-baseline gap-2">
        <span
          className={cn(
            "min-w-0 flex-1 truncate text-list",
            row.subject.trim() === "" ? "text-faint-foreground" : "text-foreground",
          )}
        >
          {/* Gmail's own words for a message with no subject, so the row reads
              the same here as it would anywhere else in a mailbox. */}
          {row.subject.trim() === "" ? "(no subject)" : row.subject}
        </span>
        <span className="shrink-0 font-mono text-micro tabular-nums text-faint-foreground">
          {messageTime(row.createdAt)}
        </span>
      </div>

      {recipients.map((line) => (
        <div key={line.label} className="flex gap-1.5 pt-0.5 text-micro">
          <span className="shrink-0 text-faint-foreground">{line.label}</span>
          <span className="min-w-0 truncate text-muted-foreground">
            {formatRecipients(line.list)}
          </span>
        </div>
      ))}

      <div className="pt-1 text-micro text-warning">{row.error}</div>

      {row.body.trim() !== "" && (
        /* Selectable, and scrolls rather than truncating. This is the copy of
           record for a message that cannot be sent as it stands: `user-select`
           is off across the app's chrome, so it is turned back on here. */
        <p className="mt-1.5 max-h-24 select-text overflow-y-auto whitespace-pre-wrap break-words border-l border-border pl-2 text-micro text-muted-foreground">
          {row.body}
        </p>
      )}

      {row.attachments.length > 0 && (
        <div className="pt-1 text-micro text-faint-foreground">
          {row.attachments.join(", ")}
        </div>
      )}

      <div className="flex gap-1 pt-1.5">
        <Button ref={retryRef} size="sm" variant="subtle" disabled={busy} onClick={onRetry}>
          Retry
        </Button>
        <Button size="sm" variant="danger" disabled={busy} onClick={onDiscard}>
          Discard
        </Button>
      </div>
    </article>
  );
}
