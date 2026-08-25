import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  containWideContent,
  discloseLinkTargets,
  externalUrl,
  CLAIM_ATTR,
  type AnchorCandidate,
  frameDocument,
  nextFrameSize,
  openExternal,
  readFrameScheme,
  readFrameTokens,
  reportLinkFailure,
  resetFrameSize,
  revealBlockedImages,
  FRAME_SANDBOX,
  frameKeepsKey,
  FRAME_TOKENS,
  INITIAL_FRAME_SIZE,
  MAX_FRAME_HEIGHT,
  SCROLL_ATTR,
  type BodyFormat,
  type FrameScheme,
  type FrameTokens,
  type WideCandidate,
} from "@/lib/message-body";
import { useKeymap } from "@/hooks/useKeymap";

export interface MessageFrameProps {
  /** Sanitizer output. Never raw sender HTML — see `ipc::render`. */
  html: string;
  allowRemoteImages: boolean;
  /** Decides whether the frame follows the app theme; see [`frameGround`]. */
  format: BodyFormat;
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
export function MessageFrame({ html, allowRemoteImages, format, title }: MessageFrameProps) {
  const frameRef = useRef<HTMLIFrameElement>(null);
  // The height, the tallest height applied to the document currently in the
  // frame, and the width it was laid out at — one value, because the decision
  // needs all three and the updater that makes it has to be pure. Keeping the
  // peak in a ref and raising it from inside the updater is what clipped the
  // last line of a message; [`nextFrameSize`] has the measurement.
  const [size, setSize] = useState(INITIAL_FRAME_SIZE);
  const { tokens, scheme } = useAppTheme();

  const srcDoc = useMemo(
    () => frameDocument({ html, allowRemoteImages, format, tokens, scheme }),
    [html, allowRemoteImages, format, tokens, scheme],
  );

  const measure = useCallback(() => {
    const frame = frameRef.current;
    const doc = frame?.contentDocument;
    if (!frame || !doc) return;

    // The frame element's own width, which is 100% of the reading pane and so
    // never a function of the height we set. When it changes, the frame is in a
    // new layout and the heights measured in the old one say nothing about it —
    // `nextFrameSize` resets the peak on exactly that.
    //
    // The pane now keeps its 10px track even for a short message (`lockGutter`),
    // so this used to fire on every tall open and no longer does. A splitter
    // drag is still a real width change.
    const width = frame.clientWidth;

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
    setSize((current) => nextFrameSize(current, measured, width));
  }, []);

  /*
   * Hand the app back its keyboard — in a browser tab, and only there.
   *
   * This is a real iframe, so the instant anything inside it has focus — one
   * click to select a word or to scroll — its keydowns fire in *its* document
   * and never reach the window the keymap listens on. Every shortcut in the app
   * dies at once: not only `r`, but archive, star, snooze and the way back to
   * the list. Clicking the list revives them, which is why it was reported as
   * "the R shortcut isn't working consistently" rather than as a dead keyboard.
   *
   * # This does not run in the app
   *
   * The same measurement `FRAME_SANDBOX` records for the click listener below
   * applies here word for word: WebKit refuses to invoke a listener whose
   * target document has scripting disabled, and the sandbox disables scripting
   * in this document by design. It attaches, it never fires, and nothing says
   * so. Believing otherwise cost a third round of the same bug report — "after
   * I click a link in an email, the E archive keycut doesn't register" — and a
   * test that dispatched a keydown into the frame passed the whole time,
   * because Blink runs it.
   *
   * What answers it in the app is `frame_keyboard`, which reads the key off the
   * `NSEvent` below the engine and hands it to the same keymap. This stays for
   * `bun run dev` and the headless harness, where it is the only path there is.
   *
   * Straight into the one registry rather than re-dispatching a synthetic event
   * upward: `keymap.handle` is what decides what a key means everywhere else,
   * and a second path to it would be a second thing to keep in step. It only
   * calls `preventDefault` when a binding actually consumed the key, so an
   * unbound key is left to the document exactly as it was.
   *
   * `frameKeepsKey` holds back what belongs to the document being read — see
   * there for which, and why ⌘A is the one that would hurt. `lib/frame-keyboard.ts`
   * applies the same rule to what arrives from Rust, so the two paths cannot
   * drift into disagreeing about which keys are the frame's.
   */
  const keymap = useKeymap();
  const forwardKey = useCallback(
    (event: Event) => {
      const key = event as KeyboardEvent;
      if (frameKeepsKey(key)) return;
      keymap.handle({
        key: key.key,
        code: key.code,
        metaKey: key.metaKey,
        ctrlKey: key.ctrlKey,
        altKey: key.altKey,
        shiftKey: key.shiftKey,
        // The element inside the frame. There is nothing typeable in here — the
        // sandbox has no `allow-forms` and the sanitizer drops every input — so
        // this only ever reads as "not typing", which is the truth.
        target: key.target as unknown as { tagName?: string; isContentEditable?: boolean },
        preventDefault: () => key.preventDefault(),
        stopPropagation: () => key.stopPropagation(),
      });
    },
    [keymap],
  );

  // Torn down and rebuilt on every document, because changing `srcdoc` replaces
  // the document these listeners are attached to.
  const teardown = useRef<(() => void) | null>(null);
  const boundDoc = useRef<Document | null>(null);
  useEffect(() => () => teardown.current?.(), []);

  /*
   * Size the frame when the HTML has parsed, not when every remote image has
   * loaded. `iframe.onload` waits for those pictures; Linear and a Honeybadger
   * digest sat at 28px for the better part of a second while they did.
   *
   * Images still grow the frame afterwards: `ResizeObserver` and the 250ms
   * follow-up in `bindDocument` catch them. What this must not do is call
   * `resetFrameSize` on load — that would collapse a parse-time height back
   * toward the floor the moment the PNG landed.
   */
  const settle = useCallback(() => {
    const frame = frameRef.current;
    const inner = frame?.contentDocument;
    const root = inner?.documentElement;
    if (!frame || !inner || !root || frame.clientWidth === 0) return;
    // One property read decides whether the pass is worth running at all.
    // Ordinary mail does not overflow, and the pass costs a forced layout per
    // table; this runs on every resize, so "nothing to do" has to be cheap.
    if (root.scrollWidth > root.clientWidth) {
      containWideContent(wideCandidates(inner), frame.clientWidth);
    }
    measure();
    root.classList.toggle("mach-capped", root.scrollHeight > MAX_FRAME_HEIGHT);
  }, [measure]);

  const bindDocument = useCallback(
    (doc: Document) => {
      if (boundDoc.current === doc) {
        settle();
        return;
      }
      teardown.current?.();
      boundDoc.current = doc;

      // Belt and braces: the authoritative reveal is the re-render with
      // `allowRemoteImages: true`, which also widens the frame CSP. This catches
      // anything a stale render left behind, as a property assignment.
      if (allowRemoteImages) {
        revealBlockedImages({
          querySelectorAll: (selector) => doc.querySelectorAll<HTMLImageElement>(selector),
        });
      }

      // Before anything is measured: a disclosure is an inline box and changes
      // what fits. Once per document rather than per resize, since the answer
      // does not depend on the layout.
      discloseLinkTargets(anchorCandidates(doc));

      doc.addEventListener("click", interceptNavigation, true);
      doc.addEventListener("auxclick", interceptNavigation, true);
      doc.addEventListener("submit", preventDefault, true);
      doc.addEventListener("dragstart", preventDefault, true);
      doc.addEventListener("keydown", forwardKey, true);

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
        if (boundDoc.current === doc) boundDoc.current = null;
        doc.removeEventListener("click", interceptNavigation, true);
        doc.removeEventListener("auxclick", interceptNavigation, true);
        doc.removeEventListener("submit", preventDefault, true);
        doc.removeEventListener("dragstart", preventDefault, true);
        doc.removeEventListener("keydown", forwardKey, true);
        observer?.disconnect();
        cancelAnimationFrame(frameId);
        window.clearTimeout(timer);
        teardown.current = null;
      };
    },
    [allowRemoteImages, settle, forwardKey],
  );

  const onLoad = useCallback(() => {
    const doc = frameRef.current?.contentDocument;
    if (doc) bindDocument(doc);
  }, [bindDocument]);

  const bindRef = useRef(bindDocument);
  bindRef.current = bindDocument;

  useEffect(() => {
    teardown.current?.();
    boundDoc.current = null;
    setSize(resetFrameSize);
    const frame = frameRef.current;
    if (!frame) return;
    let cancelled = false;
    const tick = () => {
      if (cancelled) return;
      const doc = frame.contentDocument;
      if (doc?.body && doc.readyState !== "loading" && frame.clientWidth > 0) {
        bindRef.current(doc);
        return;
      }
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    return () => {
      cancelled = true;
    };
  }, [srcDoc]);

  return (
    <iframe
      ref={frameRef}
      title={title}
      sandbox={FRAME_SANDBOX}
      srcDoc={srcDoc}
      referrerPolicy="no-referrer"
      onLoad={onLoad}
      className="block w-full border-0 bg-transparent"
      style={{ height: size.height }}
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

/**
 * Every link in the document, as something [`discloseLinkTargets`] can judge.
 *
 * The disclosure is a sibling of the anchor and not a child of it: inside, it
 * would be part of what a click hits, and the sender's own CSS — which reaches
 * descendants of their anchor — would be able to style it out of existence.
 * Outside, the only rule that matches it is the frame stylesheet's.
 *
 * Invariant 6: the element is created and inserted, never written as markup.
 * `textContent` on the host, so a host that somehow contained markup would be
 * text either way.
 */
function anchorCandidates(doc: Document): AnchorCandidate[] {
  return Array.from(doc.querySelectorAll("a[href]")).map((element) => ({
    get text() {
      return element.textContent ?? "";
    },
    get href() {
      return element.getAttribute("href");
    },
    get disclosed() {
      return element.nextElementSibling?.hasAttribute(CLAIM_ATTR) ?? false;
    },
    disclose(host: string) {
      const chip = doc.createElement("span");
      chip.setAttribute(CLAIM_ATTR, "");
      chip.textContent = `→ ${host}`;
      element.parentNode?.insertBefore(chip, element.nextSibling);
    },
  }));
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

interface AppTheme {
  tokens: FrameTokens;
  scheme: FrameScheme;
}

/**
 * The app's theme, re-read when it flips: its tokens and the scheme they belong
 * to, together.
 *
 * The frame is a separate document, so it inherits nothing — the values are
 * copied in. Watching `class` and `style` on the root is enough because that is
 * exactly how `useMach` applies the theme: the class chooses the token block
 * and the inline `color-scheme` is the resolved answer [`readFrameScheme`]
 * reads back.
 *
 * One piece of state rather than two, so a frame can never be built from this
 * render's tokens and the last one's scheme. That pairing is the whole point;
 * see [`FrameScheme`].
 */
function useAppTheme(): AppTheme {
  const [theme, setTheme] = useState<AppTheme>(() =>
    typeof document === "undefined" ? EMPTY_THEME : readAppTheme(document.documentElement),
  );

  useEffect(() => {
    const root = document.documentElement;
    // Compared by value, not identity: a new object every time would change
    // `srcdoc` and reload every frame on the page for nothing.
    const reread = () =>
      setTheme((current) => {
        const next = readAppTheme(root);
        return sameTheme(current, next) ? current : next;
      });
    reread();
    const observer = new MutationObserver(reread);
    observer.observe(root, { attributes: true, attributeFilter: ["class", "style"] });
    return () => observer.disconnect();
  }, []);

  return theme;
}

const EMPTY_THEME: AppTheme = { tokens: {}, scheme: "light" };

function readAppTheme(root: Element): AppTheme {
  return { tokens: readFrameTokens(root), scheme: readFrameScheme(root) };
}

function sameTheme(a: AppTheme, b: AppTheme): boolean {
  return a.scheme === b.scheme && FRAME_TOKENS.every((name) => a.tokens[name] === b.tokens[name]);
}
