/**
 * The WebView half of message rendering.
 *
 * `docs/message-rendering-invariants.md` is the contract this file implements.
 * The Rust sanitizer (`src-tauri/src/render/`) produces HTML that is safe to
 * *parse*; it cannot sandbox anything and cannot set a CSP. Those two live
 * here, plus the small pure decisions the reading pane makes about what to
 * show.
 *
 * Stopping the WebView from navigating used to be on that list and is not: it
 * turned out to be the one thing this layer cannot do on macOS, because a
 * listener attached to a scripting-disabled frame never runs in WebKit. That
 * job belongs to `ipc::render::link_guard`; see [`FRAME_SANDBOX`].
 *
 * Everything in this module is a pure function or an injectable call, so
 * `src/components/mail/MessageBody.test.tsx` can assert on the security
 * controls directly rather than on a screenshot.
 */

import type { MessageId } from "@/types";
import { isTauri } from "@/lib/ipc";

/* -------------------------------------------------------------------------- */
/* The payload                                                                 */
/* -------------------------------------------------------------------------- */

export type BodyFormat = "html" | "text" | "snippet" | "empty";

/** What `render_message_body` returns. Mirrors `ipc::render::RenderedMessage`. */
export interface RenderedMessage {
  messageId: MessageId;
  format: BodyFormat;
  /** Echoed from the request, so a stale render is visible rather than silent. */
  remoteImagesAllowed: boolean;
  /**
   * This is the text of a message whose HTML was evicted to reclaim disk, and
   * [`restoreMessageBody`] will upgrade it. Distinct from `format: "text"`,
   * which is also what a message that never had an HTML part renders as — that
   * one has nothing to fetch.
   */
  htmlEvicted: boolean;
  /** Sanitized HTML for the new content. */
  html: string;
  /** Sanitized HTML for the quoted history; empty when there is none. */
  quotedHtml: string;
  hasQuoted: boolean;
  /**
   * Remote images deferred behind "load images". Only ever non-zero under
   * [`BLOCK_ALL_REMOTE_IMAGES`].
   */
  blockedRemoteImages: number;
  /** Images the sanitizer judged to be tracking pixels and dropped outright. */
  blockedTrackers: number;
  inlineCidImages: number;
  inlineDataImages: number;
}

/**
 * The preference, ahead of there being a preferences window to hang it off.
 *
 * `false` — the default — is "show me my mail": remote images load, and the
 * sanitizer drops the subset of them that are tracking pixels rather than
 * pictures (see `render::sanitize::block_trackers`). `true` restores the older,
 * stricter behaviour where *every* remote image waits behind a click, which
 * tells the sender nothing at all at the cost of a mailbox full of grey boxes.
 *
 * The per-message state in `MessageBody` already exists, so turning this into a
 * real preference is a matter of changing what seeds it.
 */
export const BLOCK_ALL_REMOTE_IMAGES = false;

type Nullable<T> = T | null | undefined;

interface WireRenderedMessage {
  messageId?: Nullable<number>;
  format?: Nullable<string>;
  remoteImagesAllowed?: Nullable<boolean>;
  htmlEvicted?: Nullable<boolean>;
  html?: Nullable<string>;
  quotedHtml?: Nullable<string>;
  hasQuoted?: Nullable<boolean>;
  blockedRemoteImages?: Nullable<number>;
  blockedTrackers?: Nullable<number>;
  inlineCidImages?: Nullable<number>;
  inlineDataImages?: Nullable<number>;
}

const FORMATS = new Set<BodyFormat>(["html", "text", "snippet", "empty"]);

function count(value: Nullable<number>): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.floor(value) : 0;
}

/**
 * The wire value, with every field forced to something renderable.
 *
 * A missing `html` must become `""` and never `undefined`: it ends up inside a
 * document string, and `"undefined"` in the middle of a message body is the
 * kind of bug that survives a demo.
 */
export function mapRenderedMessage(wire: Nullable<WireRenderedMessage>, messageId: MessageId): RenderedMessage {
  const format = wire?.format as BodyFormat | undefined;
  return {
    messageId: typeof wire?.messageId === "number" ? wire.messageId : messageId,
    format: format && FORMATS.has(format) ? format : "empty",
    remoteImagesAllowed: wire?.remoteImagesAllowed === true,
    htmlEvicted: wire?.htmlEvicted === true,
    html: typeof wire?.html === "string" ? wire.html : "",
    quotedHtml: typeof wire?.quotedHtml === "string" ? wire.quotedHtml : "",
    hasQuoted: wire?.hasQuoted === true,
    blockedRemoteImages: count(wire?.blockedRemoteImages),
    blockedTrackers: count(wire?.blockedTrackers),
    inlineCidImages: count(wire?.inlineCidImages),
    inlineDataImages: count(wire?.inlineDataImages),
  };
}

/* -------------------------------------------------------------------------- */
/* The call                                                                    */
/* -------------------------------------------------------------------------- */

export type InvokeFn = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

let invoker: InvokeFn | null = null;

/** Test seam. `src/lib/ipc.ts` owns the app's transport; this owns one command. */
export function setRenderInvoker(fn: InvokeFn | null): void {
  invoker = fn;
}

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (invoker) return invoker<T>(command, args);
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  return tauriInvoke<T>(command, args);
}

/** Render one message body. Rendering itself happens on a Rust worker thread. */
export async function renderMessageBody(
  messageId: MessageId,
  allowRemoteImages: boolean,
): Promise<RenderedMessage> {
  const wire = await invoke<WireRenderedMessage>("render_message_body", {
    messageId,
    allowRemoteImages,
  });
  return mapRenderedMessage(wire, messageId);
}

/**
 * Fetch an evicted body from Gmail and render it.
 *
 * Only called for a render that came back with `htmlEvicted`. Rejects when the
 * message is no longer in Gmail or the network refused, and the caller keeps the
 * text it already has and shows the sentence — a body must never go blank
 * because a fetch failed.
 */
export async function restoreMessageBody(
  messageId: MessageId,
  allowRemoteImages: boolean,
): Promise<RenderedMessage> {
  const wire = await invoke<WireRenderedMessage>("restore_message_body", {
    messageId,
    allowRemoteImages,
  });
  return mapRenderedMessage(wire, messageId);
}

/**
 * The fallback for `bun run dev` against the fixture source, where there is no
 * Rust process to sanitize anything.
 *
 * It renders the *plain text* body and nothing else. Rendering fixture HTML
 * would mean sanitizing in TypeScript, which would be a second sanitizer with
 * none of the first one's tests — so the browser-only mode is deliberately
 * poorer than the app.
 */
export function localTextRender(messageId: MessageId, text: string): RenderedMessage {
  return {
    messageId,
    format: text.trim() ? "text" : "empty",
    remoteImagesAllowed: false,
    htmlEvicted: false,
    html: text.trim() ? `<div style="white-space:pre-wrap">${escapeHtml(text)}</div>` : "",
    quotedHtml: "",
    hasQuoted: false,
    blockedRemoteImages: 0,
    blockedTrackers: 0,
    inlineCidImages: 0,
    inlineDataImages: 0,
  };
}

const ESCAPES: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;",
};

export function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/g, (c) => ESCAPES[c] ?? c);
}

/* -------------------------------------------------------------------------- */
/* The frame                                                                   */
/* -------------------------------------------------------------------------- */

/**
 * Invariant 1. **No `allow-scripts`, ever**, and therefore nothing in the frame
 * can execute — not the sender's markup, not a future ammonia CVE's output.
 *
 * `allow-same-origin` on its own is not the dangerous half of that pair: the
 * pair is dangerous because a *script* in a same-origin frame can reach the
 * embedder, and there are no scripts here. What it buys is reading
 * `contentDocument` to size the frame and to restructure it (invariant 6).
 *
 * # Why `allow-popups` is here
 *
 * Because without it a link in a message could not be opened at all, and for
 * months was not.
 *
 * The interception in `MessageFrame` attaches a capture-phase click listener to
 * the frame's document from the parent. In Blink that works. In WebKit — which
 * is the engine every macOS WebView is — it does not: `JSEventListener` refuses
 * to run a listener whose target's context has scripting disabled, and a frame
 * sandboxed without `allow-scripts` has scripting disabled by definition. So
 * the listener attaches, and never fires, and there is no error anywhere. That
 * was measured directly against WKWebView, on this message: with
 * `allow-same-origin` alone the parent's listener recorded nothing at all for a
 * real click on a real link.
 *
 * With no listener, the click is whatever the markup says it is, and every
 * anchor the sanitizer emits carries `target="_blank"`:
 *
 * - Without `allow-popups` the `_blank` navigation is refused inside WebKit
 *   before anything outside the engine is consulted. Dead click. This is what
 *   the reader was looking at.
 * - Stripping `target` instead makes it a same-frame navigation, and that is
 *   refused too — by the *app's* `frame-src` policy, again before anything can
 *   see it. Also a dead click, also measured.
 * - With `allow-popups` the navigation reaches
 *   `decidePolicyForNavigationAction`, which is a hook outside the web engine
 *   entirely. `ipc::render::link_guard` cancels it there and hands the URL to
 *   the system browser. Nothing is opened inside Mach: cancelling the policy
 *   happens *before* WebKit asks for a window, and nothing in the app answers
 *   that request either, so the failure mode if the guard ever stops running is
 *   the dead click we started with rather than the sender's page in a window.
 *
 * `allow-popups` grants nothing else here. A frame that cannot run scripts has
 * no `window.open`, so the only way to reach it is a person clicking a link,
 * which is the case being served. It does not carry
 * `allow-popups-to-escape-sandbox`, so anything it did open would still be
 * sandboxed.
 *
 * Absent, and deliberately so: `allow-scripts`, `allow-top-navigation`,
 * `allow-forms`, `allow-modals`, `allow-downloads`.
 */
export const FRAME_SANDBOX = "allow-same-origin allow-popups";

/** Tokens copied from the app document into the frame, so mail matches chrome. */
export const FRAME_TOKENS = [
  "--foreground",
  "--muted-foreground",
  "--faint-foreground",
  "--background",
  "--border",
  "--accent",
] as const;

export type FrameTokens = Partial<Record<(typeof FRAME_TOKENS)[number], string>>;

/* -------------------------------------------------------------------------- */
/* Ground                                                                      */
/* -------------------------------------------------------------------------- */

/**
 * What the frame's own colours come from.
 *
 * - `theme` — the app's tokens, on a transparent background. The document is
 *   ours: plain text we turned into HTML ourselves, with no colour of its own.
 *   It should follow the app the way the rest of the window does.
 * - `light` — a fixed light palette, on an opaque light background. The
 *   document is the sender's.
 */
export type FrameGround = "light" | "theme";

/**
 * Remote HTML is not ours to re-theme.
 *
 * A sender writes their mail against a white page: they set `color` on the
 * things they mean to stand out and leave the rest to inherit. Copying the
 * app's `--foreground` into that document changes only half of the pair —
 * every element without a colour of its own turns near-white, while the
 * backgrounds stay whatever the sender's CSS says, which is white. That is
 * exactly what a GitHub Actions notification looked like in dark mode: the
 * heading, the "All jobs have failed" line and the table header were invisible,
 * and the footer and links — the parts the sender coloured explicitly — were
 * fine.
 *
 * The fix is to stop injecting half a theme. An HTML body gets a light ground
 * and `color-scheme: light`, so its own CSS lands on the page it was written
 * for, and the engine's own defaults for anything it does not style are dark
 * ink rather than pale. Nothing in the sender's content is restyled, and no
 * selector is special-cased; the only lever is what the document inherits.
 *
 * Plain text keeps the app theme, because there the document *is* ours.
 */
export function frameGround(format: BodyFormat): FrameGround {
  return format === "html" ? "light" : "theme";
}

/* -------------------------------------------------------------------------- */
/* Measure                                                                     */
/* -------------------------------------------------------------------------- */

/**
 * The column a `text/plain` body is rendered in.
 *
 * # Why plain text needs one and HTML does not
 *
 * A `text/plain` body arrives with its column already chosen. The sender's
 * generator wrapped it — 72, 76, 78 or 80 characters, and RFC 5322 asks for no
 * more than 78 — and those wraps are real newlines in the body, which
 * [`text_to_html`] preserves. So the message is not text to be laid out; it is
 * text that has already been laid out, and the only question left is how wide a
 * box to put it in.
 *
 * Get that box **wider** than the sender's column and every line lands intact.
 * The right edge is ragged, because the sender's breaks are not the box's
 * breaks, and that is what plain mail has looked like since before there were
 * windows to show it in.
 *
 * Get it **narrower** and each line spills its last word or two onto a line of
 * its own, at column zero, and it does that on every line in the message. That
 * is what the reader reported: an automated bookkeeping digest, wrapped at
 * about ninety columns, alternating a full line with a one-word orphan.
 *
 * Narrowing cannot fix that — it only changes which word is orphaned. The box
 * has to be at least as wide as the column the sender used.
 *
 * # The number
 *
 * 40rem is 640px, which in the frame's own font is about ninety characters of
 * ordinary mail and sixty-nine of the widest case, all digits. That holds the
 * 78 columns RFC 5322 recommends with room to spare, and it holds the
 * ninety-column digests that real bookkeeping software emits.
 *
 * It is also a ceiling: a reading pane dragged out to fill a wide window would
 * otherwise stretch a body wrapped at 72 columns across the whole of it, which
 * puts the sender's breaks nowhere near the box's and makes the ragged edge far
 * more obvious than it needs to be.
 *
 * `ReadingPane` sizes its column from this, so the frame can actually reach it;
 * `message-body.measure.test.ts` holds the two together.
 */
export const TEXT_MEASURE = "40rem";

/** The horizontal padding `ReadingPane` puts around the message column. */
export const MESSAGE_COLUMN_PADDING = "2.5rem";

/**
 * What `ReadingPane`'s message column is capped at: the measure, plus its own
 * padding, so the body inside lands on exactly [`TEXT_MEASURE`].
 */
export const MESSAGE_COLUMN_MAX = `calc(${TEXT_MEASURE} + ${MESSAGE_COLUMN_PADDING})`;

/**
 * The light theme's own values, as literals.
 *
 * They cannot be read off the app: when the window is dark, `.dark` has already
 * replaced them, and the light values are not in the cascade to be found. These
 * are the `:root` block of `src/styles/globals.css`, so a message in dark mode
 * is rendered by the same palette a message in light mode is.
 */
const LIGHT_GROUND: FrameTokens = {
  "--foreground": "oklch(0.145 0 0)",
  "--muted-foreground": "oklch(0.556 0 0)",
  "--faint-foreground": "oklch(0.708 0 0)",
  "--background": "oklch(1 0 0)",
  "--border": "oklch(0.922 0 0)",
  "--accent": "oklch(0.55 0.18 255)",
};

/**
 * Invariant 2, the message-frame CSP. Stricter than the app-level policy in
 * `tauri.conf.json` — this one starts from `'none'` and adds back two things.
 *
 * `style-src 'unsafe-inline'` is unavoidable (email *is* inline styles), which
 * is exactly why the CSS scrubber exists in the sanitizer. `img-src` widens to
 * `https:` only while the user has opted in for this message; there is no
 * attachment scheme to add yet, because nothing serves `cid:` parts.
 */
export function frameCsp(allowRemoteImages: boolean): string {
  return [
    "default-src 'none'",
    "script-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
    "frame-src 'none'",
    "object-src 'none'",
    "style-src 'unsafe-inline'",
    `img-src ${allowRemoteImages ? "data: https:" : "data:"}`,
  ].join("; ");
}

/**
 * CSS values are copied from our own stylesheet, so this is defence in depth
 * rather than a boundary — but a token is still a string being written into a
 * stylesheet, and `}` in one would end the rule.
 */
function cssValue(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed || trimmed.length > 64) return null;
  return /^[a-zA-Z0-9 .,%#()/_-]+$/.test(trimmed) ? trimmed : null;
}

function tokenBlock(tokens: FrameTokens): string {
  const declarations = FRAME_TOKENS.map((name) => {
    const value = tokens[name];
    const safe = value ? cssValue(value) : null;
    return safe ? `${name}:${safe}` : null;
  }).filter((d): d is string => d !== null);
  return declarations.join(";");
}

/**
 * Base styles for the frame. Every colour comes from a token with a `currentColor`
 * or `transparent` fallback, so a missing token degrades to the browser default
 * rather than to an invented hex.
 *
 * # `overflow-wrap: break-word`, and not `word-break: break-word`
 *
 * They read as the same rule in two strengths. They are two properties with two
 * consequences, and the difference is a column of mail rendered one letter per
 * line.
 *
 * `overflow-wrap` breaks a word that would otherwise overflow its line: it is
 * about a box that is already too narrow, and it leaves the box's *minimum*
 * width alone. `word-break: break-word` is the legacy spelling of
 * `overflow-wrap: anywhere`, which additionally tells every layout algorithm
 * that the narrowest this content can ever be is one character.
 *
 * Mail is tables, and a table column is sized from exactly that number. A
 * GitHub Actions notification asks for a 24px status column whose header
 * happens to be the word "Status". Auto table layout normally refuses:
 * "Status" is 45px at its narrowest, so the column is 45px and the 24px is
 * ignored. Under the stronger rule its narrowest is one letter, the 24px is
 * granted, and the header renders vertically — measured at 25.9px wide and
 * 176px tall on that message, which is what the reader was looking at. Spark
 * renders the same mail correctly.
 *
 * What the stronger rule was there for — a 400-character URL dragging the
 * document sideways — is already covered, because `overflow-wrap` still breaks
 * that URL. Where it leaves a table genuinely too wide, [`containWideContent`]
 * gives that table its own scroller, which is the behaviour we want anyway.
 */
function frameStyles(tokens: FrameTokens, format: BodyFormat): string {
  const ground = frameGround(format);
  /*
   * One stylesheet, two palettes. Everything below reads its colours through a
   * token, so choosing the token block chooses the ground — see [frameGround]
   * for why the sender's HTML never gets the app's.
   *
   * `color-scheme: light` on a sender's document is the other half of it: it
   * fixes the engine's own colours — form controls, scrollbars, and the initial
   * `color` for anything a missing token would fall through to — to the page
   * the mail was written for. The plain-text ground keeps `light dark`, because
   * there the tokens are the app's and the app is what it should follow.
   */
  const light = ground === "light";
  return `:root{${tokenBlock(light ? LIGHT_GROUND : tokens)};color-scheme:${light ? "light" : "light dark"}}
html,body{margin:0;padding:0;background:${light ? "var(--background,#fff)" : "transparent"}}
/* Nothing nests. A message grows to its natural height and the reading pane is
   the only thing that scrolls, so the frame's own viewport must never have
   anything to scroll:

   - Vertically it cannot, because the frame is sized to the content. Saying so
     with overflow-y:hidden matters anyway: a horizontal scrollbar takes a
     centimetre off the bottom of the viewport, which pushes the content past
     the height we just set, which grows a *vertical* scrollbar as well. That
     pair is what the reader was looking at, from a single over-wide table.
   - Horizontally it should not, because content too wide for the pane is given
     its own scroller by [containWideContent]. overflow-x stays scrollable as a
     last resort so anything that escapes that pass is reachable rather than
     silently cut off.

   Hiding the bars is belt and braces on top of that, and is what keeps the
   frame from re-entering the old feedback loop: a frame a pixel too short grows
   a scrollbar, the scrollbar narrows the body, a narrower body wraps taller,
   and the height measured to fix it is the height that caused it.

   Past MAX_FRAME_HEIGHT none of this applies: the height stops tracking the
   content, so the frame has to scroll itself or the rest of the message would
   be unreachable. MessageFrame sets mach-capped when it clamps. */
html:not(.mach-capped){overflow-x:auto;overflow-y:hidden;scrollbar-width:none}
html:not(.mach-capped)::-webkit-scrollbar{width:0;height:0;display:none}
/* On the wrapping rule below, and the one that is deliberately not here, see
   the note above frameStyles. */
body{color:var(--foreground,currentColor);font:0.9375rem/1.6 ui-sans-serif,-apple-system,BlinkMacSystemFont,"Inter","Segoe UI",system-ui,sans-serif;overflow-wrap:break-word;-webkit-font-smoothing:antialiased;-webkit-user-select:text;user-select:text}
img{max-width:100%;height:auto;border:0}
/* A tracking pixel has no business occupying a box. The sanitizer already took
   its URL away, so this is about layout, not about privacy. */
img[data-mach-tracker]{display:none!important}
table{max-width:100%}
a{color:var(--accent,currentColor)}
blockquote{margin:0.5rem 0;padding-left:0.75rem;border-left:2px solid var(--border,currentColor)}
/* A log or a code block keeps its lines: reflowing a stack trace to the width
   of the pane is not preserving it. It scrolls inside itself instead, which is
   the same bargain the wrapper below makes for a wide table — the paragraphs
   on either side stay where they are.
   overflow-y is pinned shut because leaving it at the initial value would make
   it compute to auto the moment overflow-x does not, and then the horizontal
   scrollbar could grow a vertical one inside a block that has nothing to
   scroll vertically. */
pre{white-space:pre;max-width:100%;overflow-x:auto;overflow-y:hidden}
/* The scroller [containWideContent] puts around content too wide for the pane.
   The element inside is released from the 100% cap that made it overflow the
   document in the first place — inside the scroller, being wide is the point. */
[data-mach-scroll]{max-width:100%;overflow-x:auto;overflow-y:hidden}
[data-mach-scroll]>table{max-width:none}
hr{border:0;border-top:1px solid var(--border,currentColor)}${
    ground === "light" ? "" : `\n/* See [TEXT_MEASURE]. Only a body we turned into HTML ourselves gets a
   column chosen for it; a sender's own HTML already has one. */
body{max-width:${TEXT_MEASURE}}`
  }`;
}

export interface FrameDocumentOptions {
  /** Sanitizer output. Never raw sender HTML. */
  html: string;
  allowRemoteImages: boolean;
  /** Whose document this is, and therefore whose colours it gets. */
  format: BodyFormat;
  /** Only consulted for a `theme` ground; see [`frameGround`]. */
  tokens?: FrameTokens;
}

/**
 * The whole document that goes into `srcdoc`.
 *
 * The CSP lands as a `<meta http-equiv>` because a `srcdoc` frame has no
 * response headers of its own.
 *
 * # Known gap: the inherited policy
 *
 * An `about:srcdoc` document inherits its creator's policy container, so the
 * app-level CSP in `tauri.conf.json` applies *as well as* this one and the
 * effective policy is the intersection. That is the right direction for every
 * directive here except one: the app policy's `img-src` is
 * `'self' asset: http://asset.localhost data: blob:`, which does not include
 * `https:`, so "load remote images" will render nothing in the packaged app
 * until `https:` is added to that list. `tauri.conf.json` is outside this
 * unit's ownership, so the change is not made here.
 */
export function frameDocument({
  html,
  allowRemoteImages,
  format,
  tokens = {},
}: FrameDocumentOptions): string {
  return `<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="${frameCsp(allowRemoteImages)}">
<meta name="referrer" content="no-referrer">
<style>${frameStyles(tokens, format)}</style>
</head><body>${html}</body></html>`;
}

/** Read the app's own tokens so the frame inherits the theme, not a copy of it. */
export function readFrameTokens(root: Element | null | undefined): FrameTokens {
  if (!root || typeof getComputedStyle !== "function") return {};
  const computed = getComputedStyle(root);
  const tokens: FrameTokens = {};
  for (const name of FRAME_TOKENS) {
    const value = computed.getPropertyValue(name);
    if (value && value.trim()) tokens[name] = value.trim();
  }
  return tokens;
}

/* -------------------------------------------------------------------------- */
/* Height                                                                      */
/* -------------------------------------------------------------------------- */

/** Below this a one-line message looks like a rendering failure. */
export const MIN_FRAME_HEIGHT = 28;

/**
 * Invariant: the frame must not be able to lie about its height.
 *
 * A body of a million empty divs would otherwise hand us a height no layout can
 * survive. Past the cap the frame keeps its own scrollbar, because a height that
 * has stopped tracking the content would otherwise put the rest of the message
 * out of reach — that was a real bug, and the cap caused it.
 *
 * It used to be 12 000px, which is about eight screens: long newsletters and
 * forwarded chains reach that, and every one of them grew a scrollbar inside
 * itself for no better reason than the number being small. 120 000px is far
 * past anything a person sends and still nowhere near what a browser will lay
 * out, so in practice the cap is now only ever hit by something pathological —
 * which is the only thing it was ever for.
 */
export const MAX_FRAME_HEIGHT = 120_000;

export function clampFrameHeight(measured: number): number {
  if (!Number.isFinite(measured) || measured <= 0) return MIN_FRAME_HEIGHT;
  return Math.min(MAX_FRAME_HEIGHT, Math.max(MIN_FRAME_HEIGHT, Math.ceil(measured)));
}

/**
 * Sub-pixel churn we refuse to act on: applying a one-pixel change costs a
 * relayout and buys nothing a reader can see.
 */
export const FRAME_HEIGHT_EPSILON = 1;

/**
 * The height to apply next, given what we just measured and the tallest height
 * already applied since the frame was last laid out at this width.
 *
 * # Why this is not just `clampFrameHeight(measured)`
 *
 * Setting the frame's height changes the frame's layout, and the observer that
 * asked for the measurement is watching that layout. That is a loop, and it
 * only fails to spin if the measured height is independent of the height we
 * set. It is not, quite: a frame shorter than its content grows a scrollbar,
 * the scrollbar narrows the body, and a narrower body wraps taller. So the
 * frame oscillates between "tall enough" and "tall enough, plus a scrollbar's
 * worth of rewrapping", forever — measured on a real newsletter as 2403 ⇄ 2436.
 *
 * # Why it cannot oscillate
 *
 * At a fixed width the height only ever goes up: a measurement that is not
 * taller than the tallest we have applied is ignored. So the heights applied to
 * one document form a strictly increasing sequence bounded by
 * [`MAX_FRAME_HEIGHT`], which must stop, and in practice stops in two or three
 * steps — first paint, then the images.
 *
 * The one thing that may take it back down is a change of *width*, which resets
 * the peak (see `MessageFrame`). That cannot start a loop either, because the
 * frame's width is `100%` of the reading pane and is therefore never a function
 * of the height we set: a width change comes from the reader dragging the
 * splitter or resizing the window, never from us. Growth is driven by the
 * content; shrinking is driven by the reader; neither is driven by the
 * measurement, which is what a loop would require.
 *
 * Refusing to shrink is also the safe direction on its own terms: too tall
 * leaves a band of empty space, too short clips mail.
 */
export function nextFrameHeight(current: number, measured: number, peak: number): number {
  const next = clampFrameHeight(measured);
  if (Math.abs(next - current) <= FRAME_HEIGHT_EPSILON) return current;
  return next > peak ? next : current;
}

/* -------------------------------------------------------------------------- */
/* Width                                                                       */
/* -------------------------------------------------------------------------- */

/** Marks a scroller this pass added, so the CSS can style it and we can skip it. */
export const SCROLL_ATTR = "data-mach-scroll";

/**
 * One element the pass may put in its own sideways scroller.
 *
 * Structural rather than a DOM type, for the same reason [`BlockedImage`] is:
 * the decision is worth testing without a WebView. `scrollWidth` and `handled`
 * are read *fresh* on every access — wrapping one element re-lays-out every
 * ancestor of it, and that is the entire point of doing the deepest first.
 */
export interface WideCandidate {
  /** Live: what this element measures now, not when the list was built. */
  readonly scrollWidth: number;
  /** The tag it hangs off, so a wrapper is never put somewhere illegal. */
  readonly parentTagName: string;
  /** Already wrapped, or already a scroller in its own right. */
  readonly handled: boolean;
  /** Put this element inside its own horizontal scroller. */
  wrap(): void;
}

/**
 * Where a `<div>` is not a legal child. The parser would move a wrapper out of
 * one of these and take the element with it, rearranging the message.
 */
const TABLE_INTERNAL = new Set(["TABLE", "THEAD", "TBODY", "TFOOT", "TR"]);

/** Sub-pixel slack, so a table that measures 595.4 against a 595 pane is "fits". */
const WIDTH_SLACK = 1;

/**
 * Give content too wide for the frame its own sideways scroller.
 *
 * # Why the scrolling has to live here rather than on the document
 *
 * A twelve-column table genuinely does not fit in a reading pane, and something
 * has to give. Letting the *document* scroll sideways — which is what happens
 * with no intervention — is the worst of the options: the paragraphs above and
 * below the table are only as wide as the pane, so scrolling right to read the
 * last column slides the entire message off the screen and leaves a field of
 * white beside it. It also costs a horizontal scrollbar across the whole
 * message, and a horizontal scrollbar eats enough of the frame's viewport to
 * summon a vertical one beside it. That is the pair the reader was looking at.
 *
 * A scroller around the wide element alone is what Gmail does and what the
 * reader asked for: the table scrolls, everything else stays put.
 *
 * The senders who care already do this — the message that prompted the work
 * ships its table inside `<div style="overflow-x:auto">` — but `overflow-x` is
 * not in the sanitizer's CSS allowlist, so their container arrives as a plain
 * div. We are not going to widen that allowlist for a layout problem; we
 * re-establish the container ourselves.
 *
 * # Why a DOM pass and not CSS
 *
 * "Too wide" is a measurement, and CSS has no selector for it. The obvious
 * hack — `table{display:block;overflow:auto}` — would apply to every table,
 * and in mail most tables are the *layout*, not the data.
 *
 * Nothing here writes markup: elements are created and moved, never
 * interpolated, so no sender string is re-parsed. The frame gains no
 * permission, and `docs/message-rendering-invariants.md` is untouched by it.
 *
 * Candidates must arrive **deepest first**. Wrapping the inner table usually
 * takes its ancestors back under the width, so they are never wrapped at all —
 * which is what keeps the surrounding layout in place.
 */
export function containWideContent(
  candidates: Iterable<WideCandidate>,
  frameWidth: number,
): number {
  if (!Number.isFinite(frameWidth) || frameWidth <= 0) return 0;
  let wrapped = 0;
  for (const candidate of candidates) {
    if (candidate.handled) continue;
    if (TABLE_INTERNAL.has(candidate.parentTagName)) continue;
    if (candidate.scrollWidth <= frameWidth + WIDTH_SLACK) continue;
    candidate.wrap();
    wrapped += 1;
  }
  return wrapped;
}

/* -------------------------------------------------------------------------- */
/* Behaviour                                                                   */
/* -------------------------------------------------------------------------- */

/** Tags that carry content on their own, so a body containing one is not empty. */
const CONTENTFUL = /<(img|table|hr|figure|video|audio|iframe|object)\b/i;

/** Is there anything above the quote? Text, an image, a table — anything. */
export function isEffectivelyEmpty(html: string): boolean {
  if (!html) return true;
  if (CONTENTFUL.test(html)) return false;
  const text = html
    .replace(/<[^>]*>/g, " ")
    .replace(/&nbsp;|&#160;| /g, " ")
    .trim();
  return text.length === 0;
}

/**
 * Invariant 7. A body that is *entirely* quoted is a legitimate bare forward —
 * and is also how a sender hides their whole message behind the collapse. So
 * when there is nothing above the quote, the quote is the message: show it.
 */
export function shouldAutoExpandQuote(rendered: Pick<RenderedMessage, "html" | "hasQuoted">): boolean {
  return rendered.hasQuoted && isEffectivelyEmpty(rendered.html);
}

/* -------------------------------------------------------------------------- */
/* Links                                                                       */
/* -------------------------------------------------------------------------- */

const EXTERNAL_SCHEMES = new Set(["http:", "https:", "mailto:", "tel:"]);

/**
 * The URL to hand the system browser, or `null` for anything we will not open.
 *
 * The sanitizer already restricts `href` to these four schemes; this is the
 * second check, because by the time a click arrives the value is a DOM property
 * and this function is the last thing between it and `openUrl`.
 */
export function externalUrl(href: string | null | undefined): string | null {
  if (!href) return null;
  let parsed: URL;
  try {
    parsed = new URL(href);
  } catch {
    return null;
  }
  return EXTERNAL_SCHEMES.has(parsed.protocol) ? parsed.href : null;
}

/* -------------------------------------------------------------------------- */
/* Failed links say so                                                         */
/* -------------------------------------------------------------------------- */

/**
 * The Tauri event `ipc::render` emits when it could not open a link.
 *
 * The navigation guard runs in Rust, so a failure there has no promise to
 * reject and nothing in the window would otherwise hear about it.
 */
export const LINK_FAILED_EVENT = "link-failed";

export type LinkFailureListener = (message: string) => void;

/**
 * Everyone currently willing to say a link failed. One in practice — `App`
 * mounts `LinkFailures` once — and a set because the alternative is a single
 * slot that a second subscriber silently steals.
 */
const linkFailureListeners = new Set<LinkFailureListener>();

/**
 * Say that a link could not be opened.
 *
 * This exists because the opposite was the rule for the life of this feature: a
 * click that could not be turned into an open produced nothing anywhere — no
 * toast, no log a reader could see, in one case not even a rejected promise —
 * and the same bug was reported twice before anyone could tell whether it was
 * the sanitizer, the URL, or the WebView. Whatever else changes, a click that
 * cannot open a link now ends on screen.
 *
 * A plain listener set rather than a `window` event: the producers and the one
 * consumer are all in this module graph, so there is nothing a global event
 * buys, and this half of the mechanism can then be tested without a DOM.
 */
export function reportLinkFailure(message: string): void {
  for (const listener of [...linkFailureListeners]) listener(message);
}

/** How `subscribeLinkFailures` reaches Tauri. Injected so tests need no app. */
export type ListenFn = (
  event: string,
  handler: (payload: { message?: string } | null) => void,
) => Promise<() => void>;

/**
 * Every way a link can fail to open, as one subscription.
 *
 * Two sources because there are two paths and either can be the one that fails:
 * the in-window path ([`openExternal`], and the click that produced no URL at
 * all), and the WebView's own navigation, which only Rust sees.
 */
export function subscribeLinkFailures(
  handler: LinkFailureListener,
  listen?: ListenFn,
): () => void {
  linkFailureListeners.add(handler);

  let unlisten: (() => void) | null = null;
  let cancelled = false;
  const subscribe = listen ?? (isTauri() ? tauriListen : null);
  if (subscribe) {
    void subscribe(LINK_FAILED_EVENT, (payload) => {
      if (payload?.message) handler(payload.message);
    })
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
      })
      // A subscription that could not be made is not something to report
      // *through the subscription*; the in-window half still works.
      .catch(() => {});
  }

  return () => {
    cancelled = true;
    linkFailureListeners.delete(handler);
    unlisten?.();
  };
}

const tauriListen: ListenFn = async (event, handler) => {
  const { listen } = await import("@tauri-apps/api/event");
  const off = await listen(event, (e) => handler(e.payload as { message?: string } | null));
  return () => void off();
};

/**
 * Hand a link to the system browser. Never navigates the WebView.
 *
 * # This is no longer the path that opens links in the app
 *
 * It cannot be. It is called from `interceptNavigation`, and that listener does
 * not run in WebKit — see [`FRAME_SANDBOX`] for the measurement. Inside Mach a
 * message link is opened by `ipc::render::link_guard`, at the navigation layer,
 * where no script is involved.
 *
 * What is left here still matters: `bun run dev` renders the same frontend in a
 * plain browser tab, where the listener *does* run and there is no Rust to fall
 * through to. And every other caller — a calendar link, a conference URL — is
 * an ordinary click in the app document and reaches this normally.
 *
 * There is deliberately no `window.open` fallback inside the app: a WebView
 * that cannot reach the opener plugin would answer `window.open` by rendering
 * the sender's page *inside Mach*, which is the exact outcome the interception
 * exists to prevent. It is a reported failure now rather than a silent one.
 */
export async function openExternal(url: string): Promise<void> {
  if (!isTauri()) {
    if (typeof window !== "undefined") window.open(url, "_blank", "noopener,noreferrer");
    return;
  }
  /*
   * Routed through Rust rather than the opener plugin's JS binding.
   *
   * The JS path swallowed failures in a console.warn, so a link that did
   * nothing produced no signal anywhere and three separate theories got
   * checked before the real cause could be seen. Rust logs the refusal and
   * re-validates the scheme, and the frontend surfaces the error instead of
   * dropping it.
   */
  const { invoke } = await import("@tauri-apps/api/core");
  try {
    await invoke("open_external", { url });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.warn("could not open", url, error);
    reportLinkFailure(`Could not open that link: ${message}`);
  }
}

/* -------------------------------------------------------------------------- */
/* Blocked images                                                              */
/* -------------------------------------------------------------------------- */

/** The minimum an element needs for [`revealBlockedImages`]. */
export interface BlockedImage {
  dataset: { machBlockedSrc?: string };
  src: string;
  removeAttribute(name: string): void;
}

export interface BlockedImageRoot {
  querySelectorAll(selector: string): Iterable<BlockedImage>;
}

/**
 * Invariant 4. `data-mach-blocked-src` is consumed as a **DOM property** —
 * `img.src = img.dataset.machBlockedSrc` — and never concatenated into an HTML
 * string. The stored value is the URL parser's own normalized serialization, so
 * even string concatenation would be inert; that is not a property worth
 * depending on when the alternative is one assignment.
 *
 * The authoritative path for loading images is a re-render with
 * `allowRemoteImages: true`, which also widens the frame CSP. This runs after
 * that render for anything left over, and returns how many it revealed.
 */
export function revealBlockedImages(root: BlockedImageRoot | null | undefined): number {
  if (!root) return 0;
  let revealed = 0;
  for (const img of root.querySelectorAll("img[data-mach-blocked-src]")) {
    const url = externalUrl(img.dataset.machBlockedSrc);
    if (!url) continue;
    img.src = url;
    img.removeAttribute("data-mach-blocked-src");
    revealed += 1;
  }
  return revealed;
}

/* -------------------------------------------------------------------------- */
/* Keys inside the frame                                                       */
/* -------------------------------------------------------------------------- */

/**
 * Does the message document keep this keystroke, or does the app get it?
 *
 * A message body is a real iframe, so once anything in it has focus — one click
 * to select a word, or to scroll, or on the way to a link — its keydowns fire
 * in *its* document and never reach the window listener the keymap is on. Every
 * shortcut in the app goes dead: not just `r`, but archive, star, snooze and the
 * way back to the list. It comes back if you click the list again, which is what
 * made it read as "the R shortcut isn't working consistently" rather than as a
 * dead keyboard.
 *
 * So the frame forwards what it catches. The exceptions are the keys that mean
 * something *here*, in a document you are reading:
 *
 *  * the ones that move within it — arrows, page up and down, home, end, space.
 *    Forwarding those would scroll the thread list under a message you were
 *    trying to read down;
 *  * ⌘A and ⌘C, which are select-all and copy of this message's text. ⌘A is the
 *    dangerous one: the app binds it to "select every conversation".
 *
 * Everything else — the letters, Escape, ⌘Z, the account keys — has no meaning
 * inside a read-only document and belongs to the app. The sandbox has no
 * `allow-forms` and the sanitizer drops every input, so there is nothing in
 * here that a letter could be typed into.
 */
export function frameKeepsKey(event: {
  key: string;
  metaKey?: boolean;
  ctrlKey?: boolean;
}): boolean {
  const key = event.key;
  if (MOVES_WITHIN_DOCUMENT.has(key)) return true;
  if (event.metaKey || event.ctrlKey) {
    const letter = key.toLowerCase();
    return letter === "a" || letter === "c";
  }
  return false;
}

const MOVES_WITHIN_DOCUMENT: ReadonlySet<string> = new Set([
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "PageUp",
  "PageDown",
  "Home",
  "End",
  " ",
]);
