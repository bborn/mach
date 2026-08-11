import { useMemo, useState } from "react";
import {
  Download,
  File,
  FileArchive,
  FileAudio,
  FileCode,
  FileImage,
  FileSpreadsheet,
  FileText,
  FileVideo,
  LoaderCircle,
  Paperclip,
  ShieldAlert,
  TriangleAlert,
} from "lucide-react";
import type { Attachment, AttachmentId, Message } from "@/types";
import { clockTime } from "@/lib/time";
import { cn } from "@/lib/utils";
import {
  attachmentKind,
  attachmentLabel,
  disambiguateNames,
  displayFilename,
  formatBytes,
  isExecutable,
  openAttachment,
  saveAttachment,
  type AttachmentKind,
} from "@/lib/attachments";
import { errorMessage } from "@/lib/ipc";
import { MessageBody } from "./MessageBody";

export interface ThreadMessageProps {
  message: Message;
  live: boolean;
  expanded: boolean;
  onToggle: () => void;
  /**
   * Reopen this message in the composer. Only ever called for a draft; the
   * reading pane resolves the row back to its editable copy.
   */
  onOpenDraft?: () => void;
}

/**
 * One message in a conversation: a header that is always shown, and a body that
 * is only rendered while expanded.
 *
 * Collapsed messages cost nothing — no frame, no IPC, no sanitizing — which is
 * what keeps a forty-message thread cheap to open. Attachments follow the same
 * rule and take it further: expanding a message renders the chips, and nothing
 * is downloaded until one of them is activated.
 *
 * # A draft is not a message you sent
 *
 * A draft is mirrored into the conversation it answers, which is what puts it
 * in the Drafts mailbox and on the phone. It used to render here identically to
 * a sent reply: no mark, nothing to activate, and the agent meanwhile telling
 * the owner the thread carried a DRAFT label. Somebody was going to close that
 * thread believing they had answered it.
 *
 * So a draft row says **Draft**, in words. The colour is reinforcement and
 * never the signal — the pill is bordered and labelled, so it survives being
 * read in greyscale or by a screen reader. And the row *does* something:
 * activating it opens the draft in the composer rather than unfolding a
 * read-only copy of text nobody has finished writing. There is no expand state
 * for a draft, because editing it is the only thing anyone wants from it.
 */
export function ThreadMessage({
  message,
  live,
  expanded,
  onToggle,
  onOpenDraft,
}: ThreadMessageProps) {
  const attachments = message.attachments;
  const draft = message.isDraft;

  return (
    <article
      data-draft={draft || undefined}
      className={cn(
        "border-t border-border py-3 first:border-t-0",
        // A second, redundant mark, for the reader scanning down the left edge
        // rather than reading the row. Redundancy is the point: neither this
        // nor the pill is the only thing saying "unsent".
        draft && "-ml-3 border-l-2 border-l-danger pl-3",
      )}
    >
      <button
        type="button"
        onClick={draft ? onOpenDraft : onToggle}
        aria-expanded={draft ? undefined : expanded}
        title={draft ? "Edit draft" : undefined}
        className={cn(
          "-mx-2 flex w-[calc(100%+1rem)] items-baseline gap-2 rounded-[var(--radius)] px-2 py-1 text-left",
          "hover:bg-row-hover",
        )}
      >
        {draft && (
          <span className="shrink-0 rounded-[3px] border border-danger px-1 font-medium text-micro uppercase tracking-wide text-danger">
            Draft
          </span>
        )}
        <span className="shrink-0 truncate text-body font-medium text-foreground">
          {message.from.name}
        </span>
        {expanded && !draft ? (
          <span className="min-w-0 flex-1 truncate text-micro text-faint-foreground">
            {message.from.email}
          </span>
        ) : (
          <span className="min-w-0 flex-1 truncate text-list text-muted-foreground">
            {preview(message)}
          </span>
        )}
        {attachments.length > 0 && (
          <Paperclip size={12} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
        )}
        <span className="shrink-0 font-mono text-micro tabular-nums text-faint-foreground">
          {clockTime(message.timestamp)}
        </span>
      </button>

      {expanded && !draft && (
        <>
          <div className="mt-1 truncate text-micro text-faint-foreground">
            to {message.to.map((p) => p.name).join(", ") || "—"}
            {message.cc.length > 0 && ` · cc ${message.cc.map((p) => p.name).join(", ")}`}
          </div>

          <MessageBody message={message} live={live} />

          {attachments.length > 0 && <AttachmentRow attachments={attachments} live={live} />}
        </>
      )}
    </article>
  );
}

/* -------------------------------------------------------------------------- */
/* Attachments                                                                 */
/* -------------------------------------------------------------------------- */

export interface AttachmentRowProps {
  attachments: Attachment[];
  /** False in `bun run dev` against fixtures, where there is nothing to fetch. */
  live: boolean;
}

/**
 * The chip row under an expanded message.
 *
 * # Everything works from the keyboard
 *
 * Each attachment is two real `<button>`s, so they are in the tab order for
 * free, Enter and Space activate them for free, and the focus ring is the app's
 * own. No `tabIndex`, no `onKeyDown`, no `role="button"` on a div — every one
 * of those is a way to get *most* of a keyboard affordance and lose the part
 * that only a screen reader would have told you about.
 *
 * The first button opens; the second saves. Opening is the common case and gets
 * the whole chip; saving is the deliberate one and gets an icon beside it.
 *
 * # Failures are shown, not swallowed
 *
 * A refusal from Rust — an executable, an attachment too large, an expired
 * grant — is a sentence the reader needs. It lands under the row rather than in
 * a console nobody has open.
 */
export function AttachmentRow({ attachments, live }: AttachmentRowProps) {
  const [busy, setBusy] = useState<AttachmentId | null>(null);
  const [status, setStatus] = useState<{ error: boolean; message: string } | null>(null);

  // The names as the row will show them: never blank, and never two the same.
  // Both cases are ordinary in real mail — a part with no filename at all, and
  // three copies of one name in a single message.
  const names = useMemo(
    () => disambiguateNames(attachments.map((a) => displayFilename(a.filename))),
    [attachments],
  );

  async function run(id: AttachmentId, action: () => Promise<string | null>) {
    setBusy(id);
    setStatus(null);
    try {
      const message = await action();
      setStatus(message ? { error: false, message } : null);
    } catch (caught: unknown) {
      setStatus({ error: true, message: errorMessage(caught) });
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="mt-3">
      <ul aria-label="Attachments" className="flex list-none flex-wrap gap-2 p-0">
        {attachments.map((attachment, index) => {
          const kind = attachmentKind(attachment.mimeType, attachment.filename);
          const Icon = ICONS[kind];
          const name = names[index] ?? displayFilename(attachment.filename);
          const label = attachmentLabel(name, attachment.mimeType, attachment.sizeBytes);
          const working = busy === attachment.id;
          const program = isExecutable(attachment.filename, attachment.mimeType);

          return (
            <li key={attachment.id} className="max-w-full">
              <div className="flex items-stretch overflow-hidden rounded-[var(--radius)] border border-border bg-surface">
                <button
                  type="button"
                  disabled={!live || working}
                  aria-label={`Open ${label}`}
                  /*
                   * Both of the unhappy tooltips are labels, not explanations.
                   *
                   * The disabled one is a `bun run dev` state: there is no Rust
                   * process behind the fixtures, so there are no bytes to fetch.
                   * Nobody in that browser tab needs to be told what the fixture
                   * preview is — they are the one running it.
                   *
                   * The program one is a warning that has to fit on a chip. What
                   * it is warning about: Rust refuses to hand an executable to
                   * LaunchServices (`attachments::names::is_dangerous`), so the
                   * click will fail. `isExecutable` here is only the hint that
                   * lets the chip say so first. Saving is the way through, and
                   * the Save button is already the next control along, so the
                   * tooltip does not have to spell out the recovery.
                   */
                  title={
                    !live
                      ? "Needs the app"
                      : program
                        ? "Mach does not open programs"
                        : `Open ${name}`
                  }
                  onClick={() =>
                    run(attachment.id, async () => {
                      await openAttachment(attachment.id);
                      return null;
                    })
                  }
                  className={cn(
                    "flex min-w-0 items-center gap-1 px-2 py-1 text-left",
                    "hover:bg-row-hover disabled:opacity-60",
                  )}
                >
                  {working ? (
                    <LoaderCircle
                      size={12}
                      strokeWidth={1.75}
                      className="shrink-0 animate-spin text-faint-foreground"
                    />
                  ) : (
                    <Icon
                      size={12}
                      strokeWidth={1.75}
                      className={cn(
                        "shrink-0",
                        program ? "text-danger" : "text-faint-foreground",
                      )}
                    />
                  )}
                  <span className="truncate text-micro text-muted-foreground">{name}</span>
                  <span className="shrink-0 font-mono text-micro text-faint-foreground">
                    {formatBytes(attachment.sizeBytes)}
                  </span>
                </button>

                <button
                  type="button"
                  disabled={!live || working}
                  aria-label={`Save ${name}`}
                  title={`Save ${name}…`}
                  onClick={() =>
                    run(attachment.id, async () => {
                      const saved = await saveAttachment(attachment.id);
                      return saved.path ? `Saved ${saved.filename}` : null;
                    })
                  }
                  className={cn(
                    "flex shrink-0 items-center border-l border-border px-2",
                    "hover:bg-row-hover disabled:opacity-60",
                  )}
                >
                  <Download size={12} strokeWidth={1.75} className="text-faint-foreground" />
                </button>
              </div>
            </li>
          );
        })}
      </ul>

      {status && (
        <div
          role="status"
          className={cn(
            "mt-2 flex items-center gap-1 text-micro",
            status.error ? "text-danger" : "text-muted-foreground",
          )}
        >
          {status.error && <TriangleAlert size={12} strokeWidth={1.75} className="shrink-0" />}
          <span className="truncate">{status.message}</span>
        </div>
      )}
    </div>
  );
}

const ICONS: Record<AttachmentKind, typeof File> = {
  image: FileImage,
  audio: FileAudio,
  video: FileVideo,
  archive: FileArchive,
  spreadsheet: FileSpreadsheet,
  code: FileCode,
  document: FileText,
  executable: ShieldAlert,
  file: File,
};

/**
 * One line of the body for a collapsed row, from text we already have.
 *
 * Gmail's snippet first, and `bodyText` only when there is none. This used to
 * read `bodyText` alone, and a collapsed thread from a mailer that dumps its
 * `<style>` block into the `text/plain` alternative showed four rows of
 * `body { margin: 0; padding: 0; -webkit-text-size-adjust: 100%…` where the
 * message should have been. The prose in that particular one began 554
 * characters in, well past anything a preview would ever slice to.
 *
 * The snippet is Gmail's summary of the *rendered* message, so it is the right
 * source for a summary on its own merits, and it was already being fetched.
 * `bodyText` stays as the fallback for a draft Mach mirrored itself, which has
 * no snippet until it has been sent and synced back.
 */
export function preview(message: Message): string {
  const line = message.snippet.trim() || message.bodyText;
  return line.replace(/\s+/g, " ").trim().slice(0, 200);
}

/**
 * Re-exported so the existing import site keeps working. The implementation
 * moved to `src/lib/attachments.ts`, where it can be tested beside the rest of
 * the attachment display decisions.
 */
export { formatBytes };
