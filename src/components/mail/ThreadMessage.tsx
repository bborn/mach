import { Paperclip } from "lucide-react";
import type { Message } from "@/types";
import { clockTime } from "@/lib/time";
import { cn } from "@/lib/utils";
import { MessageBody } from "./MessageBody";

export interface ThreadMessageProps {
  message: Message;
  live: boolean;
  expanded: boolean;
  onToggle: () => void;
}

/**
 * One message in a conversation: a header that is always shown, and a body that
 * is only rendered while expanded.
 *
 * Collapsed messages cost nothing — no frame, no IPC, no sanitizing — which is
 * what keeps a forty-message thread cheap to open.
 */
export function ThreadMessage({ message, live, expanded, onToggle }: ThreadMessageProps) {
  const attachments = message.attachments;

  return (
    <article className="border-t border-border py-3 first:border-t-0">
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={expanded}
        className={cn(
          "-mx-2 flex w-[calc(100%+1rem)] items-baseline gap-2 rounded-[var(--radius)] px-2 py-1 text-left",
          "hover:bg-row-hover",
        )}
      >
        <span className="shrink-0 truncate text-body font-medium text-foreground">
          {message.from.name}
        </span>
        {expanded ? (
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

      {expanded && (
        <>
          <div className="mt-0.5 truncate text-micro text-faint-foreground">
            to {message.to.map((p) => p.name).join(", ") || "—"}
            {message.cc.length > 0 && ` · cc ${message.cc.map((p) => p.name).join(", ")}`}
          </div>

          <MessageBody message={message} live={live} />

          {attachments.length > 0 && (
            <div className="mt-3 flex flex-wrap gap-2">
              {attachments.map((attachment) => (
                <span
                  key={attachment.id}
                  className="inline-flex max-w-full items-center gap-1.5 rounded-[var(--radius)] border border-border bg-surface px-2 py-1"
                >
                  <Paperclip
                    size={12}
                    strokeWidth={1.75}
                    className="shrink-0 text-faint-foreground"
                  />
                  <span className="truncate text-micro text-muted-foreground">
                    {attachment.filename}
                  </span>
                  <span className="shrink-0 font-mono text-micro text-faint-foreground">
                    {formatBytes(attachment.sizeBytes)}
                  </span>
                </span>
              ))}
            </div>
          )}
        </>
      )}
    </article>
  );
}

/** One line of the body for a collapsed row, from text we already have. */
function preview(message: Message): string {
  return message.bodyText.replace(/\s+/g, " ").trim().slice(0, 200);
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
