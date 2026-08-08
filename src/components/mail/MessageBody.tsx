import { useEffect, useState } from "react";
import { ChevronDown, ChevronUp, ImageOff, TriangleAlert } from "lucide-react";
import type { Message } from "@/types";
import { errorMessage } from "@/lib/ipc";
import {
  localTextRender,
  renderMessageBody,
  shouldAutoExpandQuote,
  BLOCK_ALL_REMOTE_IMAGES,
  type RenderedMessage,
} from "@/lib/message-body";
import { Button } from "@/components/ui/button";
import { MessageFrame } from "./MessageFrame";

export interface MessageBodyProps {
  message: Message;
  /** True when the Tauri backend is behind the data source. */
  live: boolean;
}

/**
 * A message body: sanitize in Rust, render in a sandboxed frame, and offer the
 * two choices the reader owns — remote images, and quoted history.
 *
 * # Images load; trackers do not
 *
 * The old default turned every message into a wall of grey boxes with a button
 * on top, which is not what a mail client is for — half of real mail *is* the
 * pictures, and a reader who clicks "load images" on everything has bought the
 * inconvenience without the privacy. So remote images load, and the sanitizer
 * drops the ones that were never pictures: 1×1 pixels, images hidden with CSS,
 * `/open`-shaped URLs with no dimensions. That is where nearly all of the
 * read-receipt value actually lives.
 *
 * `BLOCK_ALL_REMOTE_IMAGES` restores the strict behaviour for a reader who
 * wants a guarantee rather than a heuristic.
 *
 * # The remote-images choice is per message, not per sender
 *
 * Deliberate. "Trust this sender forever" is a decision made from one message
 * about every future one, and the payoff for a tracker is exactly that: one
 * click buys a read receipt on everything that address sends afterwards.
 * Clicking again costs a click. The choice also resets when the message is
 * closed, because nothing persists it.
 */
export function MessageBody({ message, live }: MessageBodyProps) {
  const [allowRemoteImages, setAllowRemoteImages] = useState(!BLOCK_ALL_REMOTE_IMAGES);
  const [rendered, setRendered] = useState<RenderedMessage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [quotedOverride, setQuotedOverride] = useState<boolean | null>(null);

  // A different message is a different decision.
  useEffect(() => {
    setAllowRemoteImages(!BLOCK_ALL_REMOTE_IMAGES);
    setQuotedOverride(null);
    setRendered(null);
  }, [message.id]);

  useEffect(() => {
    if (!live) {
      // No Rust process to sanitize with; the fixture source is text-only.
      setRendered(localTextRender(message.id, message.bodyText));
      return;
    }
    let cancelled = false;
    renderMessageBody(message.id, allowRemoteImages)
      .then((next) => {
        if (cancelled) return;
        setRendered(next);
        setError(null);
      })
      .catch((caught: unknown) => {
        if (cancelled) return;
        setError(errorMessage(caught));
        setRendered(localTextRender(message.id, message.bodyText));
      });
    return () => {
      cancelled = true;
    };
  }, [message.id, message.bodyText, allowRemoteImages, live]);

  if (!rendered) {
    return <div className="mt-3 text-list text-faint-foreground">Rendering…</div>;
  }

  return (
    <MessageBodyView
      rendered={rendered}
      subject={message.from.name}
      allowRemoteImages={allowRemoteImages}
      onLoadRemoteImages={() => setAllowRemoteImages(true)}
      quotedOpen={quotedOverride ?? shouldAutoExpandQuote(rendered)}
      onToggleQuoted={(open) => setQuotedOverride(open)}
      error={error}
    />
  );
}

export interface MessageBodyViewProps {
  rendered: RenderedMessage;
  /** Used to name the frames for screen readers. */
  subject: string;
  allowRemoteImages: boolean;
  onLoadRemoteImages: () => void;
  quotedOpen: boolean;
  onToggleQuoted: (open: boolean) => void;
  error?: string | null;
}

/**
 * The presentational half, with no data fetching in it.
 *
 * Split out so `MessageBody.test.tsx` can render the security-relevant markup —
 * the frame's `sandbox` and CSP, the blocked-images bar — without a WebView, a
 * backend, or a DOM.
 */
export function MessageBodyView({
  rendered,
  subject,
  allowRemoteImages,
  onLoadRemoteImages,
  quotedOpen,
  onToggleQuoted,
  error,
}: MessageBodyViewProps) {
  const blocked = rendered.blockedRemoteImages;
  const empty = rendered.html.length === 0;

  return (
    <div className="selectable mt-3">
      {error && (
        <div className="mb-2 flex items-center gap-1.5 text-micro text-danger">
          <TriangleAlert size={12} strokeWidth={1.75} className="shrink-0" />
          <span className="truncate">{error}</span>
        </div>
      )}

      {/*
        Trackers are dropped silently. There is nothing to click and nothing
        to decide, so a bar saying so is just noise on every marketing email.
        The count still comes back on `rendered.blockedTrackers` if a
        diagnostics surface ever wants it.

        `blocked` is different: it only happens under BLOCK_ALL_REMOTE_IMAGES,
        it is a deferral rather than a decision, and it keeps its button.
      */}
      {blocked > 0 && !allowRemoteImages && (
        <div className="mb-2 flex items-center gap-2 rounded-[var(--radius)] border border-border bg-surface px-2 py-1">
          <ImageOff size={12} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
          <span className="min-w-0 flex-1 truncate text-micro text-muted-foreground">
            {blocked} remote image{blocked === 1 ? "" : "s"} blocked
          </span>
          <Button
            size="sm"
            variant="subtle"
            onClick={onLoadRemoteImages}
            title="Loads them for this message only. Remote images tell the sender you opened it."
          >
            Load images
          </Button>
        </div>
      )}

      {empty ? (
        <div className="text-list text-faint-foreground">No message body</div>
      ) : (
        <MessageFrame
          html={rendered.html}
          allowRemoteImages={allowRemoteImages}
          title={`Message from ${subject}`}
        />
      )}

      {rendered.hasQuoted && (
        <>
          <div className="mt-1">
            <Button
              size="sm"
              variant="subtle"
              className="gap-1"
              aria-expanded={quotedOpen}
              onClick={() => onToggleQuoted(!quotedOpen)}
            >
              {quotedOpen ? (
                <ChevronUp size={12} strokeWidth={1.75} />
              ) : (
                <ChevronDown size={12} strokeWidth={1.75} />
              )}
              <span className="text-micro">{quotedOpen ? "Hide quoted text" : "Quoted text"}</span>
            </Button>
          </div>
          {quotedOpen && (
            <div className="mt-1 border-l-2 border-border pl-3">
              <MessageFrame
                html={rendered.quotedHtml}
                allowRemoteImages={allowRemoteImages}
                title={`Quoted history in the message from ${subject}`}
              />
            </div>
          )}
        </>
      )}
    </div>
  );
}
