import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  containWideContent,
  externalUrl,
  frameDocument,
  nextFrameHeight,
  openExternal,
  readFrameTokens,
  reportLinkFailure,
  revealBlockedImages,
  FRAME_SANDBOX,
  FRAME_TOKENS,
  MAX_FRAME_HEIGHT,
  MIN_FRAME_HEIGHT,
  SCROLL_ATTR,
  type FrameTokens,
  type WideCandidate,
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
 * 1. `sandbox="allow-same-origin allow-popups"` — no `allow-scripts`, no
 *    `allow-top-navigation`. Nothing in the frame can run. `allow-popups` is
 *    what lets a link reach the navigation hook at all; see [`FRAME_SANDBOX`].
 * 2. A per-frame CSP, in a `<meta>` because `srcdoc` has no headers.
 * 3. Navigation is intercepted. In a browser tab that happens here, in the
 *    parent; in the app it happens in `ipc::render::link_guard`, because a
 *    listener attached to a scripting-disabled document never runs in WebKit.
 *    Either way the URL goes to the system browser and the WebView does not
 *    navigate.
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

  // The tallest height applied to the document currently in the frame, and the
  // width it was laid out at. `nextFrameHeight` refuses anything shorter than
  // the peak, which is what makes the measure→resize→measure cycle terminate;
  // a change of width is the one thing that may take it back down, and it can
  // only come from the reader. Both are reset when the document is replaced.
  const peak = useRef(MIN_FRAME_HEIGHT);
  const laidOutAt = useRef(0);

  const measure = useCallback(() => {
    const frame = frameRef.current;
    const doc = frame?.contentDocument;
    if (!frame || !doc) return;

    // The frame element's own width, which is 100% of the reading pane and so
    // never a function of the height we set. When it changes, the frame is in a
    // new layout and the heights measured in the old one say nothing about it.
    const width = frame.clientWidth;
    if (width !== laidOutAt.current) {
      laidOutAt.current = width;
      peak.current = MIN_FRAME_HEIGHT;
    }

    /*
     * Three readings, because each one misses something the others catch.
     *
     * `body.scrollHeight` is the obvious one and is short by the last child's
     * bottom margin, which collapses through the body and is not counted — 30px
     * of a real message, clipped, on the first case this was tested against.
     *
     * The root's *box* has it, because the root shrink-wraps the body's margin
     * box. Its `scrollHeight` would not: that one is `max(content, viewport)`,
     * and the viewport is the height we just set, so it can only ever ratchet
     * upward. The rect is independent of what we set — checked at 28, 386, 416
     * and 900px against the same content, which reported 416 every time.
     */
    const body = doc.body;
    const root = doc.documentElement;
    const measured = Math.max(
      body?.scrollHeight ?? 0,
      Math.ceil(body?.getBoundingClientRect().height ?? 0),
      Math.ceil(root?.getBoundingClientRect().height ?? 0),
    );
    setHeight((current) => {
      const next = nextFrameHeight(current, measured, peak.current);
      if (next !== current) peak.current = Math.max(peak.current, next);
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
    peak.current = MIN_FRAME_HEIGHT;
    laidOutAt.current = 0;

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

    /*
     * Let the frame scroll itself once it is clamped.
     *
     * Below the cap the frame is exactly as tall as its content and has nothing
     * to scroll. At the cap the height has stopped tracking the content, so
     * without a scrollbar everything past it would be unreachable — which is a
     * bug this code has already had once.
     */
    const syncCapped = () => {
      const root = frameRef.current?.contentDocument?.documentElement;
      if (!root) return;
      root.classList.toggle("mach-capped", root.scrollHeight > MAX_FRAME_HEIGHT);
    };

    // Contain, then measure, then decide about the cap — in that order, because
    // each answer depends on the one before it.
    const settle = () => {
      const frame = frameRef.current;
      const inner = frame?.contentDocument;
      const root = inner?.documentElement;
      // One property read decides whether the pass is worth running at all.
      // Ordinary mail does not overflow, and the pass costs a forced layout per
      // table; this runs on every resize, so "nothing to do" has to be cheap.
      if (frame && inner && root && root.scrollWidth > root.clientWidth) {
        containWideContent(wideCandidates(inner), frame.clientWidth);
      }
      measure();
      syncCapped();
    };

    const observer =
      typeof ResizeObserver === "function" ? new ResizeObserver(() => settle()) : null;
    // The body again, for the same reason, and because observing the root would
    // re-fire on every height we set and risk a resize loop.
    const observed = doc.body ?? doc.documentElement;
    if (observer && observed) observer.observe(observed);

    // Inline `data:` images and web fonts settle a frame or two after load, and
    // an image is exactly the thing that turns a table that fitted into one
    // that does not.
    const frameId = requestAnimationFrame(settle);
    const timer = window.setTimeout(settle, 250);
    settle();

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
 * The elements [`containWideContent`] is allowed to consider, deepest first.
 *
 * Tables are the whole of the problem in practice — mail is tables — and the
 * body's own children are the backstop for the rest: a `<div style="width:
 * 900px">` at the top level is rare but does happen, and there is no reason for
 * it to drag the document sideways either.
 *
 * Deliberately *not* a walk of every element. `scrollWidth` forces layout on
 * each read, and a thousand-paragraph message would pay for a thousand of them
 * to find nothing.
 */
function wideCandidates(doc: Document): WideCandidate[] {
  const tables = Array.from(doc.querySelectorAll("table")).reverse();
  const topLevel = doc.body ? Array.from(doc.body.children) : [];
  return [...tables, ...topLevel].map((element) => candidate(element, doc));
}

function candidate(element: Element, doc: Document): WideCandidate {
  return {
    // Getters, not values: every wrap changes the layout the next answer
    // depends on.
    get scrollWidth() {
      return element.scrollWidth;
    },
    get parentTagName() {
      return element.parentElement?.tagName ?? "";
    },
    get handled() {
      if (element.parentElement?.hasAttribute(SCROLL_ATTR)) return true;
      // An element that already scrolls sideways on its own — a `<pre>`, or a
      // sender's div that kept its overflow — reports its *content* width from
      // `scrollWidth`, so it would look permanently too wide and be wrapped in
      // a second scroller for nothing.
      const view = doc.defaultView;
      if (!view) return false;
      return view.getComputedStyle(element).overflowX !== "visible";
    },
    wrap() {
      const parent = element.parentElement;
      if (!parent) return;
      // Created and moved, never written as markup: no sender string is
      // re-parsed by this.
      const box = doc.createElement("div");
      box.setAttribute(SCROLL_ATTR, "");
      parent.insertBefore(box, element);
      box.appendChild(element);
    },
  };
}

/**
 * Every click inside the frame, captured before the document sees it.
 *
 * A link becomes an `openUrl` call; anything else that could navigate is simply
 * cancelled. The `href` is re-validated even though the sanitizer restricted it
 * to four schemes, because at this point it is a DOM property and this is the
 * last check before the system browser.
 *
 * # This does not run in the app
 *
 * It runs in `bun run dev`, in a browser tab, and it is the only thing that
 * opens a link there. Inside Mach it is dead code that looks alive: WebKit
 * refuses to invoke a listener whose target document has scripting disabled,
 * and the sandbox disables scripting in this document by design. Attaching
 * succeeds; firing never happens; nothing says so. That cost this project two
 * rounds of the same bug report — see [`FRAME_SANDBOX`] for the measurement and
 * for what opens links instead.
 *
 * It stays because the browser path is real and because it is the stricter of
 * the two: where it does run, it cancels the navigation outright rather than
 * relying on a later hook to do it.
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
  if (url) {
    void openExternal(url);
    return;
  }
  // A link whose href survived the sanitizer but not the URL parser. Rare, and
  // exactly the shape of failure that used to end here in silence.
  reportLinkFailure("That link does not go anywhere Mach can open");
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
