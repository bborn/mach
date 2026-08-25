import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronUp, ImageOff, TriangleAlert } from "lucide-react";
import type { Message } from "@/types";
import { errorMessage } from "@/lib/ipc";
import {
  localTextRender,
  renderMessageBody,
  restoreMessageBody,
  shouldAutoExpandQuote,
  BLOCK_ALL_REMOTE_IMAGES,
  type RenderedMessage,
} from "@/lib/message-body";
import {
  correctedScrollTop,
  findScroller,
  needsCorrection,
  shouldApplyUpgrade,
  ANCHOR_HOLD_MS,
} from "@/lib/body-upgrade";
import { applyInlineImages, contentIdsIn, fetchInlineImages } from "@/lib/attachments";
import { Button } from "@/components/ui/button";
import { MessageFrame } from "./MessageFrame";

/** Nothing resolved yet. Hoisted so it is one stable identity, not a new Map per render. */
const NO_INLINE_IMAGES: ReadonlyMap<string, string> = new Map();

/** `findScroller`'s view of a real element. The only DOM in the anchor path. */
const DOM_PROBE = {
  parent: (node: HTMLElement) => node.parentElement,
  overflowY: (node: HTMLElement) => getComputedStyle(node).overflowY,
  scrollHeight: (node: HTMLElement) => node.scrollHeight,
  clientHeight: (node: HTMLElement) => node.clientHeight,
};

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
 *
 * # Inline images are not the same choice
 *
 * A `cid:` image is a part of this message. It was delivered with it, fetching
 * it tells the sender nothing they do not already know, and there is no version
 * of "load images" a reader should have to click to see the chart in the mail
 * they are reading. So inline images resolve automatically for an expanded
 * message, independently of `allowRemoteImages` — which is exactly how the
 * sanitizer already counts them (see `render::sanitize`, which does *not* put
 * them in `blockedRemoteImages`).
 *
 * # An evicted body renders as text and upgrades
 *
 * `body_html` for old mail is dropped to keep the store from growing without
 * bound (`src-tauri/src/evict/`). The first render for one of those messages
 * comes back with `htmlEvicted`, and it is the plain text — on screen
 * immediately, with no spinner and nothing waiting on Google, exactly like every
 * other body. The HTML is fetched behind it and swapped in when it lands, pinned
 * so nothing above the message moves and held while the reader is inside it. See
 * `@/lib/body-upgrade`.
 *
 * A fetch that fails leaves the text where it is and puts the reason above it.
 */
export function MessageBody({ message, live }: MessageBodyProps) {
  const [allowRemoteImages, setAllowRemoteImages] = useState(!BLOCK_ALL_REMOTE_IMAGES);
  const [rendered, setRendered] = useState<RenderedMessage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [quotedOverride, setQuotedOverride] = useState<boolean | null>(null);
  const [inlineImages, setInlineImages] =
    useState<ReadonlyMap<string, string>>(NO_INLINE_IMAGES);
  // The upgraded render, waiting for a moment when applying it will not move
  // the sentence being read. Null once it has been applied.
  const [pending, setPending] = useState<RenderedMessage | null>(null);
  const box = useRef<HTMLDivElement>(null);
  // Whether the reader has scrolled since this message opened. A ref rather
  // than state: every scroll event would otherwise re-render the body.
  const engaged = useRef(false);

  // A different message is a different decision.
  useEffect(() => {
    setAllowRemoteImages(!BLOCK_ALL_REMOTE_IMAGES);
    setQuotedOverride(null);
    setRendered(null);
    setPending(null);
    setInlineImages(NO_INLINE_IMAGES);
    engaged.current = false;
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

  /*
   * The evicted body's second half: ask Gmail for the HTML.
   *
   * Keyed on `htmlEvicted` rather than on the message, so it runs once for a
   * message that has one and never at all for the ones that do not. The result
   * is cached in the store by Rust, so a second open of the same message finds
   * it resident and this effect never fires again.
   */
  const evicted = rendered?.htmlEvicted === true;
  useEffect(() => {
    if (!live || !evicted) return;
    let cancelled = false;
    restoreMessageBody(message.id, allowRemoteImages)
      .then((upgraded) => {
        if (cancelled) return;
        setPending(upgraded);
        setError(null);
      })
      .catch((caught: unknown) => {
        // The text stays. Saying so is the whole of the failure handling here:
        // a body that quietly remained text would be indistinguishable from a
        // message that never had any HTML.
        if (!cancelled) setError(errorMessage(caught));
      });
    return () => {
      cancelled = true;
    };
  }, [live, evicted, message.id, allowRemoteImages]);

  /**
   * Put the upgraded body in without moving anything the reader can see.
   *
   * Two parts, and they are separate problems. The message's own top is pinned
   * across the swap, so growth cannot push the conversation around it — held for
   * a beat, because the frame reports its height over several frames as the
   * images land. And the swap is refused outright while the reader is inside the
   * message, in which case the render stays pending and the next scroll gets
   * another chance at it.
   */
  const applyUpgrade = useCallback((upgraded: RenderedMessage) => {
    const node = box.current;
    if (!node) return false;

    const container = findScroller(node, DOM_PROBE);
    const frameTop = container ? container.getBoundingClientRect().top : 0;
    const rect = node.getBoundingClientRect();
    const anchor = rect.top - frameTop;

    if (!shouldApplyUpgrade(engaged.current, { top: anchor, bottom: rect.bottom - frameTop })) {
      return false;
    }

    setRendered(upgraded);
    setPending(null);

    if (!container) return true;

    // Re-pin every frame until the iframe has finished settling. Reading the
    // box fresh each time is the point: the height arrives in steps.
    const until = Date.now() + ANCHOR_HOLD_MS;
    const pin = () => {
      const moved = node.getBoundingClientRect().top - container.getBoundingClientRect().top;
      if (needsCorrection(anchor, moved)) {
        container.scrollTop = correctedScrollTop(container.scrollTop, anchor, moved);
      }
      if (Date.now() < until) requestAnimationFrame(pin);
    };
    requestAnimationFrame(pin);
    return true;
  }, []);

  // Apply as soon as it arrives, and again on every scroll until it takes.
  useEffect(() => {
    if (!pending) return;
    if (applyUpgrade(pending)) return;
    const onScroll = () => applyUpgrade(pending);
    window.addEventListener("scroll", onScroll, { capture: true, passive: true });
    return () => window.removeEventListener("scroll", onScroll, { capture: true });
  }, [pending, applyUpgrade]);

  // "Has the reader moved since this opened." Capture-phase because the pane
  // that scrolls is an ancestor, and scroll does not bubble.
  useEffect(() => {
    const onScroll = () => {
      engaged.current = true;
    };
    window.addEventListener("scroll", onScroll, { capture: true, passive: true });
    return () => window.removeEventListener("scroll", onScroll, { capture: true });
  }, [message.id]);

  /*
   * Resolve the `cid:` references the sanitizer left behind.
   *
   * Runs only for a message the reader has expanded — this component is not
   * mounted for a collapsed one — and only when the sanitizer actually found
   * references, so an ordinary message makes no call at all. Each reference is
   * settled independently inside `fetchInlineImages`, so one missing part
   * leaves one placeholder rather than dropping the whole set.
   */
  // Only assembled when there is something to look for: a body can be eight
  // megabytes, and concatenating both halves on every render to find nothing
  // would be a copy per keystroke elsewhere in the app.
  const inlineCount = rendered?.inlineCidImages ?? 0;
  const cidHtml = inlineCount > 0 && rendered ? rendered.html + rendered.quotedHtml : "";
  useEffect(() => {
    if (!live || inlineCount === 0) return;
    const contentIds = contentIdsIn(cidHtml);
    if (contentIds.length === 0) return;

    let cancelled = false;
    void fetchInlineImages(message.id, contentIds).then((resolved) => {
      if (!cancelled && resolved.size > 0) setInlineImages(resolved);
    });
    return () => {
      cancelled = true;
    };
  }, [live, message.id, inlineCount, cidHtml]);

  // The body the frame actually renders, with any resolved inline image
  // substituted for its placeholder pixel.
  const withImages = useMemo(() => {
    if (!rendered || inlineImages.size === 0) return rendered;
    return {
      ...rendered,
      html: applyInlineImages(rendered.html, inlineImages),
      quotedHtml: applyInlineImages(rendered.quotedHtml, inlineImages),
    };
  }, [rendered, inlineImages]);

  if (!withImages) {
    // Empty, not a status line. The iframe is a frame or two away.
    return <div ref={box} className="mt-3" />;
  }

  // The box the anchor is measured from. It has to be the whole body, including
  // the quoted history below it, because that is what grows when the HTML lands.
  return (
    <div ref={box}>
      <MessageBodyView
        rendered={withImages}
        subject={message.from.name}
        allowRemoteImages={allowRemoteImages}
        onLoadRemoteImages={() => setAllowRemoteImages(true)}
        quotedOpen={quotedOverride ?? shouldAutoExpandQuote(withImages)}
        onToggleQuoted={(open) => setQuotedOverride(open)}
        error={error}
      />
    </div>
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
          {/*
            The tooltip used to carry the reasoning: that this is a per-message
            choice, and that a remote image is how a sender learns the message
            was opened. Both are true and neither belongs on a button. Someone
            reaching for "Load images" has already decided; two sentences of
            privacy briefing between them and the pictures is a tax on the
            decision they came to make, paid on every message.

            What survives is the scope, which is the one thing that is not
            obvious from the button: this does not remember the sender.
          */}
          <Button
            size="sm"
            variant="subtle"
            onClick={onLoadRemoteImages}
            title="For this message only"
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
          format={rendered.format}
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
                format={rendered.format}
                title={`Quoted history in the message from ${subject}`}
              />
            </div>
          )}
        </>
      )}
    </div>
  );
}
