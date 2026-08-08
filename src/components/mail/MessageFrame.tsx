import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  externalUrl,
  frameDocument,
  nextFrameHeight,
  openExternal,
  readFrameTokens,
  revealBlockedImages,
  FRAME_SANDBOX,
  FRAME_TOKENS,
  MAX_FRAME_HEIGHT,
  MIN_FRAME_HEIGHT,
  type FrameTokens,
} from "@/lib/message-body";

export interface MessageFrameProps {
  /** Sanitizer output. Never raw sender HTML — see `ipc::render`. */
  html: string;
  allowRemoteImages: boolean;
  /** Screen-reader name; iframes without one are announced as "frame". */
  title: string;
}

/**
 * One message body, in a sandboxed iframe.
 *
 * This component *is* the WebView half of the security contract in
 * `docs/message-rendering-invariants.md`:
 *
 * 1. `sandbox="allow-same-origin"` — no `allow-scripts`, no `allow-popups`, no
 *    `allow-top-navigation`. Nothing in the frame can run.
 * 2. A per-frame CSP, in a `<meta>` because `srcdoc` has no headers.
 * 3. Navigation is intercepted here, in the parent, and links go to the system
 *    browser. Rust cannot stop the WebView from navigating; this can.
 * 4. `data-mach-blocked-src` is read as a DOM property, never concatenated.
 *
 * The frame is sized to its content so the *pane* scrolls rather than the
 * message, and the measurement is clamped so a hostile body cannot claim a
 * height that breaks the layout.
 */
export function MessageFrame({ html, allowRemoteImages, title }: MessageFrameProps) {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(MIN_FRAME_HEIGHT);
  const tokens = useThemeTokens();

  const srcDoc = useMemo(
    () => frameDocument({ html, allowRemoteImages, tokens }),
    [html, allowRemoteImages, tokens],
  );

  // Every height we have applied to the document currently in the frame.
  // `nextFrameHeight` uses it to tell a real content change from a
  // measure→resize→measure cycle. Cleared when the document is replaced.
  const applied = useRef<Set<number>>(new Set());

  const measure = useCallback(() => {
    const doc = frameRef.current?.contentDocument;
    if (!doc) return;
    // Measured on the body, not on `documentElement`: the root's `scrollHeight`
    // is clamped to the frame's own viewport, which is the height we just set,
    // so a frame measured that way can grow and never shrink again.
    const body = doc.body;
    const measured = body
      ? Math.max(body.scrollHeight, Math.ceil(body.getBoundingClientRect().height))
      : (doc.documentElement?.scrollHeight ?? 0);
    setHeight((current) => {
      const next = nextFrameHeight(current, measured, applied.current);
      if (next !== current) applied.current.add(next);
      return next;
    });
  }, []);

  // Torn down and rebuilt on every load, because changing `srcdoc` replaces the
  // document these listeners are attached to.
  const teardown = useRef<(() => void) | null>(null);
  useEffect(() => () => teardown.current?.(), []);

  const onLoad = useCallback(() => {
    teardown.current?.();
    const doc = frameRef.current?.contentDocument;
    if (!doc) return;

    // A new document is new content, so the old heights say nothing about it.
    applied.current = new Set();

    // Belt and braces: the authoritative reveal is the re-render with
    // `allowRemoteImages: true`, which also widens the frame CSP. This catches
    // anything a stale render left behind, as a property assignment.
    if (allowRemoteImages) {
      revealBlockedImages({
        querySelectorAll: (selector) => doc.querySelectorAll<HTMLImageElement>(selector),
      });
    }

    doc.addEventListener("click", interceptNavigation, true);
    doc.addEventListener("auxclick", interceptNavigation, true);
    doc.addEventListener("submit", preventDefault, true);
    doc.addEventListener("dragstart", preventDefault, true);

    const observer =
      typeof ResizeObserver === "function" ? new ResizeObserver(() => measure()) : null;
    // The body again, for the same reason, and because observing the root would
    // re-fire on every height we set and risk a resize loop.
    const observed = doc.body ?? doc.documentElement;
    if (observer && observed) observer.observe(observed);

    /*
     * Let the frame scroll itself once it is clamped.
     *
     * Below the cap the frame is exactly as tall as its content and must never
     * grow a scrollbar — that feedback loop is what made the pane jitter. At
     * the cap the height is fixed, so the loop cannot start, and without a
     * scrollbar everything past 12000px was unreachable.
     */
    const syncCapped = () => {
      const root = frameRef.current?.contentDocument?.documentElement;
      if (!root) return;
      root.classList.toggle("mach-capped", root.scrollHeight > MAX_FRAME_HEIGHT);
    };
    syncCapped();

    // Inline `data:` images and web fonts settle a frame or two after load.
    const frameId = requestAnimationFrame(() => {
      measure();
      syncCapped();
    });
    const timer = window.setTimeout(() => {
      measure();
      syncCapped();
    }, 250);
    measure();

    teardown.current = () => {
      doc.removeEventListener("click", interceptNavigation, true);
      doc.removeEventListener("auxclick", interceptNavigation, true);
      doc.removeEventListener("submit", preventDefault, true);
      doc.removeEventListener("dragstart", preventDefault, true);
      observer?.disconnect();
      cancelAnimationFrame(frameId);
      window.clearTimeout(timer);
      teardown.current = null;
    };
  }, [allowRemoteImages, measure]);

  return (
    <iframe
      ref={frameRef}
      title={title}
      sandbox={FRAME_SANDBOX}
      srcDoc={srcDoc}
      referrerPolicy="no-referrer"
      onLoad={onLoad}
      className="block w-full border-0 bg-transparent"
      style={{ height }}
    />
  );
}

/**
 * Every click inside the frame, captured before the document sees it.
 *
 * A link becomes an `openUrl` call; anything else that could navigate is simply
 * cancelled. The `href` is re-validated even though the sanitizer restricted it
 * to four schemes, because at this point it is a DOM property and this is the
 * last check before the system browser.
 */
function interceptNavigation(event: Event): void {
  const node = event.target as Node | null;
  // `instanceof Element` is a cross-realm trap here: the frame has its own
  // constructors. `nodeType` is the same number in every realm.
  const start = node?.nodeType === 1 ? (node as Element) : (node?.parentElement ?? null);
  const anchor = start?.closest("a[href]") ?? null;

  if (!anchor) {
    // A middle click on a non-link can still open something in some engines.
    if (event.type === "auxclick") event.preventDefault();
    return;
  }

  event.preventDefault();
  event.stopPropagation();

  const url = externalUrl(anchor.getAttribute("href"));
  if (url) void openExternal(url);
}

function preventDefault(event: Event): void {
  event.preventDefault();
}

/**
 * The app's tokens, re-read when the theme flips.
 *
 * The frame is a separate document, so it inherits nothing: the values are
 * copied in. Watching the `class` attribute is enough because that is exactly
 * how `useMach` applies the theme.
 */
function useThemeTokens(): FrameTokens {
  const [tokens, setTokens] = useState<FrameTokens>(() =>
    typeof document === "undefined" ? {} : readFrameTokens(document.documentElement),
  );

  useEffect(() => {
    const root = document.documentElement;
    // Compared by value, not identity: a new object every time would change
    // `srcdoc` and reload every frame on the page for nothing.
    const reread = () =>
      setTokens((current) => {
        const next = readFrameTokens(root);
        return sameTokens(current, next) ? current : next;
      });
    reread();
    const observer = new MutationObserver(reread);
    observer.observe(root, { attributes: true, attributeFilter: ["class", "style"] });
    return () => observer.disconnect();
  }, []);

  return tokens;
}

function sameTokens(a: FrameTokens, b: FrameTokens): boolean {
  return FRAME_TOKENS.every((name) => a[name] === b[name]);
}
