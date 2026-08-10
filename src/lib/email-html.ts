/**
 * The HTML half of the composer.
 *
 * Three jobs, and they are separate on purpose.
 *
 * 1. **[[cleanFragment]] — what survives a paste.** Squire hands every paste,
 *    and every `setHTML`, to a sanitizer of the editor's choosing. This is that
 *    sanitizer. Word and Google Docs paste catastrophic markup — a `<style>`
 *    block, MSO conditional comments carrying a second one, `class="MsoNormal"`
 *    on every paragraph, `font-family:"Calibri",sans-serif` with a `mso-`
 *    property beside it, and `<o:p>` elements that mean nothing outside Word —
 *    and none of it may reach the message, because none of it means anything in
 *    the recipient's client either.
 *
 * 2. **[[htmlToPlainText]] — the `text/plain` twin.** A mirror of
 *    `compose::html::to_text` in Rust, for the same reason `markdownToHtml` was
 *    a mirror of `compose::markdown`: the editor needs to answer "is there
 *    anything in this draft" without a round trip, and the answer has to be the
 *    same one Rust will give. They are pinned to one table of cases in
 *    `email-html.test.ts` and `src-tauri/tests/compose.rs`. **The bytes that go
 *    on the wire are always Rust's.**
 *
 * 3. **Signatures**, which are a text preference that has to become HTML.
 *
 * # The allowlist is shared with Rust, by hand
 *
 * [[ALLOWED_TAGS]] and [[STYLE_PROPERTIES]] are the same lists as
 * `compose::html`. Two copies is one more than anybody wants; the alternative is
 * a round trip to Rust on every paste, which would put an IPC call between ⌘V
 * and the text appearing.
 */

/** Tags an outgoing message may contain. Anything else is unwrapped. */
export const ALLOWED_TAGS: readonly string[] = [
  "a", "b", "blockquote", "br", "code", "div", "em", "h1", "h2", "h3", "h4", "h5", "h6", "hr",
  "i", "img", "li", "ol", "p", "pre", "s", "span", "strike", "strong", "sub", "sup", "table",
  "tbody", "td", "tfoot", "th", "thead", "tr", "u", "ul",
];

/**
 * Tags whose *content* goes too.
 *
 * Unwrapping a `<style>` would put the stylesheet in the message as text, which
 * is how a pasted Word document ends up with `p.MsoNormal {margin:0}` printed
 * at the top of it.
 */
const DROP_WITH_CONTENT: readonly string[] = [
  "script", "style", "meta", "link", "title", "head", "noscript", "iframe", "object", "embed",
  "form", "input", "button", "select", "textarea", "svg", "math",
];

/**
 * CSS properties that may ride inline. See `compose::html::STYLE_PROPERTIES`.
 *
 * `font-family` and `font-size` are deliberately absent. Pasting two paragraphs
 * out of Word otherwise carries Calibri at 14pt into a reply that is otherwise
 * in the recipient's own default — the "somebody else's fonts" problem that was
 * the original argument against a rich-text composer at all. Weight, style,
 * colour and structure survive; the typeface is the reader's business.
 */
export const STYLE_PROPERTIES: readonly string[] = [
  "background-color", "border-left", "color", "font-style",
  "font-weight", "margin", "margin-bottom", "margin-left", "margin-right", "margin-top",
  "padding", "padding-bottom", "padding-left", "padding-right", "padding-top", "text-align",
  "text-decoration", "white-space",
];

/** Substrings that disqualify a declaration. See `compose::html`. */
const UNSAFE_VALUE_MARKERS: readonly string[] = [
  "var(", "calc(", "clamp(", "min(", "max(", "env(", "oklch(", "oklab(", "lab(", "lch(",
  "color-mix(", "color(", "url(", "expression(", "javascript:", "!important", "@",
];

/**
 * `data-mach-cid` is the one attribute here that does not go on the wire.
 *
 * It names which part of the message an inline image is, and the composer
 * needs it to survive the editor's own cleaning: a `data:` src is not on
 * `SAFE_SCHEMES` and never will be, so an inline image that is copied and
 * pasted within a message loses its src and would have nothing left to say
 * which image it was. Rust's `compose::html::sanitize` does not allow the
 * attribute, so it is gone by the time the message is built — see
 * `withInlineImages` in `lib/compose.ts`.
 */
const TAG_ATTRIBUTES: Record<string, readonly string[]> = {
  a: ["href", "title"],
  img: ["src", "alt", "width", "height", "data-mach-cid"],
  td: ["colspan", "rowspan"],
  th: ["colspan", "rowspan"],
  ol: ["start"],
};

const GLOBAL_ATTRIBUTES: readonly string[] = ["style", "dir"];

const SAFE_SCHEMES: readonly string[] = ["http:", "https:", "mailto:", "tel:", "cid:"];

/**
 * Clean one declaration list. Exported because it is the part of the cleaner
 * that can be tested without a DOM, and the part most likely to be wrong.
 */
export function cleanStyle(value: string): string {
  const kept: string[] = [];
  for (const declaration of value.split(";")) {
    const at = declaration.indexOf(":");
    if (at === -1) continue;
    const property = declaration.slice(0, at).trim().toLowerCase();
    const val = declaration.slice(at + 1).trim();
    if (!val || !STYLE_PROPERTIES.includes(property)) continue;
    if (!isSafeStyleValue(val)) continue;
    kept.push(`${property}: ${val}`);
  }
  return kept.join("; ");
}

export function isSafeStyleValue(value: string): boolean {
  const lowered = value.toLowerCase();
  if (lowered.length > 200) return false;
  return !UNSAFE_VALUE_MARKERS.some((marker) => lowered.includes(marker));
}

function isSafeUrl(raw: string): boolean {
  const trimmed = raw.trim();
  // A relative URL cannot resolve in somebody else's inbox, but an anchor or a
  // path is harmless as text — it is only a link target that has to be real.
  if (/^[a-z][a-z0-9+.-]*:/i.test(trimmed)) {
    return SAFE_SCHEMES.some((scheme) => trimmed.toLowerCase().startsWith(scheme));
  }
  return false;
}

/**
 * Clean an HTML string into a fragment the editor can hold.
 *
 * Takes the document rather than reaching for a global so the same function
 * runs under a test DOM.
 */
export function cleanFragment(html: string, doc: Document): DocumentFragment {
  const parsed = new DOMParser().parseFromString(html, "text/html");
  const fragment = doc.createDocumentFragment();
  for (const child of Array.from(parsed.body.childNodes)) {
    const cleaned = cleanNode(child, doc);
    if (cleaned) fragment.appendChild(cleaned);
  }
  return fragment;
}

/** The string form: what `cleanFragment` would produce, serialized. */
export function cleanHtml(html: string, doc: Document): string {
  const holder = doc.createElement("div");
  holder.appendChild(cleanFragment(html, doc));
  return holder.innerHTML;
}

/**
 * One node, cleaned.
 *
 * Returns the node to keep, or `null` for one that goes. A tag that is not on
 * the list is *unwrapped* rather than dropped — a `<section>` around three
 * paragraphs is meaningless in mail, but the three paragraphs are the message.
 */
function cleanNode(node: Node, doc: Document): Node | null {
  if (node.nodeType === 3 /* text */) {
    return doc.createTextNode(node.nodeValue ?? "");
  }
  // Comments go, always. MSO conditional comments are how a pasted Word
  // document smuggles a stylesheet past a filter that only looks at tags.
  if (node.nodeType !== 1 /* element */) return null;

  const element = node as Element;
  const name = element.tagName.toLowerCase();
  if (DROP_WITH_CONTENT.includes(name)) return null;

  const children = Array.from(element.childNodes)
    .map((child) => cleanNode(child, doc))
    .filter((child): child is Node => child !== null);

  // A `<span>` that carries nothing is markup with no meaning. Word and Docs
  // emit them by the dozen, and each one is a thing for a mail client to
  // mishandle.
  if (name === "span" && !survivingAttributes(element).length) {
    const holder = doc.createDocumentFragment();
    for (const child of children) holder.appendChild(child);
    return holder;
  }

  if (!ALLOWED_TAGS.includes(name)) {
    // `<o:p>`, `<font>`, `<section>`: keep what is inside, drop the wrapper.
    const holder = doc.createDocumentFragment();
    for (const child of children) holder.appendChild(child);
    return holder;
  }

  const kept = doc.createElement(name);
  for (const [attributeName, value] of survivingAttributes(element)) {
    kept.setAttribute(attributeName, value);
  }
  for (const child of children) kept.appendChild(child);
  return kept;
}

/** The attributes of one element that survive the allowlist, already cleaned. */
function survivingAttributes(element: Element): Array<[string, string]> {
  const name = element.tagName.toLowerCase();
  const allowed = [...GLOBAL_ATTRIBUTES, ...(TAG_ATTRIBUTES[name] ?? [])];
  const out: Array<[string, string]> = [];
  for (const attribute of Array.from(element.attributes)) {
    const attributeName = attribute.name.toLowerCase();
    if (!allowed.includes(attributeName)) continue;
    if (attributeName === "style") {
      const cleaned = cleanStyle(attribute.value);
      if (cleaned) out.push(["style", cleaned]);
      continue;
    }
    if ((attributeName === "href" || attributeName === "src") && !isSafeUrl(attribute.value)) {
      continue;
    }
    out.push([attributeName, attribute.value]);
  }
  return out;
}

/* -------------------------------------------------------------------------- */
/* The plain-text twin — a mirror of `compose::html::to_text`                   */
/* -------------------------------------------------------------------------- */

/**
 * Blocks that render with air around them, and so get a blank line.
 *
 * `<div>` is deliberately not one. A `<div>` has no default margin, so two of
 * them in a row are two adjacent lines on screen — and the editor writes one
 * `<div>` per line, which would turn every message into double-spaced text if
 * this list were "everything that is a block". `<p>` does have a margin, and
 * gets the blank line it draws.
 */
const SPACED_BLOCK_TAGS = new Set([
  "p", "h1", "h2", "h3", "h4", "h5", "h6", "table", "tfoot", "thead", "tbody",
]);

/** Blocks that end the line and nothing more. */
const LINE_BLOCK_TAGS = new Set(["div", "tr", "li"]);

interface TextState {
  out: string;
  lists: (number | null)[];
  quoteDepth: number;
  preDepth: number;
  link: { href: string; from: number } | null;
  pending: number;
  lineStarted: boolean;
  /**
   * How deep inside a list item we are.
   *
   * Google Docs wraps every list item's text in a `<p>`, and a paragraph break
   * between the bullet and its own words puts "- " on a line of its own. Inside
   * an item, a block break is a line break and nothing more.
   */
  itemDepth: number;
  /** Swallow the next break: the line so far is a list marker awaiting text. */
  swallowBreak: boolean;
}

/**
 * The `text/plain` alternative, derived from the HTML.
 *
 * Structure is reproduced rather than stripped: blocks separated by a blank
 * line, list items keeping their marker, a blockquote keeping its `>`, and a
 * link carrying its URL when the text is not already the URL. Emphasis is not
 * reproduced — there is no markdown in this editor any more, and asterisks in a
 * plain-text part would be indistinguishable from asterisks somebody typed.
 */
export function htmlToPlainText(html: string): string {
  const state: TextState = {
    out: "",
    lists: [],
    quoteDepth: 0,
    preDepth: 0,
    link: null,
    pending: 0,
    lineStarted: false,
    itemDepth: 0,
    swallowBreak: false,
  };
  for (const token of tokenize(html)) {
    if (token.kind === "text") pushText(state, decodeEntities(token.text));
    else if (token.kind === "open") openTag(state, token.name, token.attributes);
    else closeTag(state, token.name);
  }
  state.pending = 0;
  return state.out.replace(/[\n ]+$/, "");
}

function pushText(state: TextState, raw: string): void {
  if (state.preDepth > 0) {
    const lines = raw.split("\n");
    lines.forEach((line, index) => {
      if (index > 0) newline(state, 1);
      if (line) write(state, line);
    });
    return;
  }
  const collapsed = collapseWhitespace(raw);
  if (!collapsed) return;
  // Whitespace between two blocks is layout, not a word gap.
  if (!collapsed.trim() && (state.pending > 0 || !state.lineStarted)) return;
  if (!state.lineStarted && collapsed.startsWith(" ")) {
    const trimmed = collapsed.trimStart();
    if (!trimmed) return;
    write(state, trimmed);
    return;
  }
  write(state, collapsed);
}

function write(state: TextState, text: string): void {
  flushBreaks(state);
  if (!state.lineStarted) {
    state.out += "> ".repeat(state.quoteDepth);
    state.lineStarted = true;
  }
  state.out += text;
  state.swallowBreak = false;
}

function newline(state: TextState, count: number): void {
  if (!state.out && !state.lineStarted) return;
  if (state.swallowBreak) return;
  state.pending = Math.max(state.pending, count);
}

function flushBreaks(state: TextState): void {
  const breaks = state.pending;
  for (let i = 0; i < breaks; i += 1) {
    state.out += "\n";
    state.lineStarted = false;
  }
  // A blank line inside a quote is still inside the quote.
  if (breaks > 1 && state.quoteDepth > 0) {
    const marker = "> ".repeat(state.quoteDepth).trimEnd();
    state.out = `${state.out.slice(0, -1)}${marker}\n`;
  }
  state.pending = 0;
}

function openTag(state: TextState, name: string, attributes: string): void {
  if (name === "br") return newline(state, 1);
  if (name === "hr") {
    newline(state, 2);
    write(state, "---");
    newline(state, 2);
    return;
  }
  if (SPACED_BLOCK_TAGS.has(name)) return newline(state, state.itemDepth > 0 ? 1 : 2);
  if (name === "div" || name === "tr") return newline(state, 1);
  if (name === "td" || name === "th") {
    if (state.lineStarted) write(state, "\t");
    return;
  }
  if (name === "pre") {
    newline(state, 2);
    state.preDepth += 1;
    return;
  }
  if (name === "blockquote") {
    newline(state, 2);
    state.quoteDepth += 1;
    return;
  }
  if (name === "ul") {
    newline(state, 2);
    state.lists.push(null);
    return;
  }
  if (name === "ol") {
    newline(state, 2);
    const start = Number(attribute(attributes, "start") ?? "1");
    state.lists.push(Number.isFinite(start) && start > 0 ? start : 1);
    return;
  }
  if (name === "li") {
    newline(state, 1);
    const indent = "  ".repeat(Math.max(state.lists.length - 1, 0));
    const last = state.lists.length - 1;
    let marker = "- ";
    if (last >= 0 && state.lists[last] !== null) {
      marker = `${state.lists[last]}. `;
      state.lists[last] = (state.lists[last] as number) + 1;
    }
    write(state, `${indent}${marker}`);
    state.itemDepth += 1;
    state.swallowBreak = true;
    return;
  }
  if (name === "a") {
    const href = attribute(attributes, "href");
    if (href) state.link = { href: decodeEntities(href), from: state.out.length };
  }
}

function closeTag(state: TextState, name: string): void {
  if (SPACED_BLOCK_TAGS.has(name)) return newline(state, state.itemDepth > 0 ? 1 : 2);
  if (name === "li") {
    state.itemDepth = Math.max(state.itemDepth - 1, 0);
    return newline(state, 1);
  }
  if (LINE_BLOCK_TAGS.has(name)) return newline(state, 1);
  if (name === "pre") {
    state.preDepth = Math.max(state.preDepth - 1, 0);
    newline(state, 2);
    return;
  }
  if (name === "blockquote") {
    state.quoteDepth = Math.max(state.quoteDepth - 1, 0);
    newline(state, 2);
    return;
  }
  if (name === "ul" || name === "ol") {
    state.lists.pop();
    newline(state, 2);
    return;
  }
  if (name === "a" && state.link) {
    const { href, from } = state.link;
    state.link = null;
    const text = state.out.slice(from).trim();
    if (href && text && text !== href && !href.startsWith("mailto:") && !href.startsWith("cid:")) {
      write(state, ` <${href}>`);
    }
  }
}

function collapseWhitespace(raw: string): string {
  const collapsed = raw.replace(/\s+/g, " ");
  if (!collapsed.trim()) return collapsed ? " " : "";
  return collapsed;
}

type Token =
  | { kind: "text"; text: string }
  | { kind: "open"; name: string; attributes: string }
  | { kind: "close"; name: string };

/** A tag-level scanner. See `compose::html::Scanner` for why this is enough. */
function* tokenize(html: string): Generator<Token> {
  let rest = html;
  while (rest) {
    if (rest.startsWith("<")) {
      const end = rest.indexOf(">");
      if (end !== -1) {
        const inner = rest.slice(1, end).trim();
        rest = rest.slice(end + 1);
        if (inner.startsWith("/")) {
          yield { kind: "close", name: tagName(inner.slice(1)) };
          continue;
        }
        const body = inner.endsWith("/") ? inner.slice(0, -1) : inner;
        const space = body.search(/\s/);
        yield {
          kind: "open",
          name: tagName(body),
          attributes: space === -1 ? "" : body.slice(space + 1),
        };
        continue;
      }
    }
    const next = rest.indexOf("<", 1);
    const end = next === -1 ? rest.length : next;
    yield { kind: "text", text: rest.slice(0, end) };
    rest = rest.slice(end);
  }
}

function tagName(inner: string): string {
  return inner.split(/[\s/]/)[0].toLowerCase();
}

function attribute(attributes: string, name: string): string | null {
  const match = new RegExp(`(?:^|\\s)${name}\\s*=\\s*("([^"]*)"|'([^']*)'|([^\\s>]+))`, "i").exec(
    attributes,
  );
  if (!match) return null;
  return match[2] ?? match[3] ?? match[4] ?? null;
}

const ENTITIES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
  "#39": "'",
  nbsp: " ",
  "#160": " ",
};

export function decodeEntities(raw: string): string {
  if (!raw.includes("&")) return raw;
  return raw.replace(/&(#?[a-zA-Z0-9]+);/g, (whole, entity: string) => {
    const known = ENTITIES[entity];
    if (known !== undefined) return known;
    if (entity.startsWith("#")) {
      const code = Number(entity.slice(1));
      if (Number.isInteger(code) && code > 0 && code < 0x110000) return String.fromCodePoint(code);
    }
    return whole;
  });
}

/* -------------------------------------------------------------------------- */
/* Emptiness, escaping, signatures                                             */
/* -------------------------------------------------------------------------- */

export function escapeHtmlText(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/**
 * Nothing in it.
 *
 * An empty rich-text editor is not an empty string — it holds `<div><br></div>`,
 * because a contenteditable with no block in it cannot be typed into. Every
 * "should this draft be saved / pushed / confirmed before discarding" question
 * runs through here, which is why it reads the text rather than the markup.
 */
export function isBlankHtml(html: string): boolean {
  if (!html) return true;
  if (/<img\b/i.test(html)) return false;
  return htmlToPlainText(html).trim() === "";
}

/** Plain text as HTML: one block per line, escaped. */
export function htmlFromPlainText(text: string): string {
  const normalized = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  return normalized
    .split("\n")
    .map((line) => (line.trim() ? `<div>${escapeHtmlText(line)}</div>` : "<div><br></div>"))
    .join("");
}

/**
 * The RFC 3676 delimiter as HTML — the line is exactly `-- `, trailing space
 * and all, which is what every client looks for when it greys a signature out
 * or trims it from a quote.
 */
export const SIGNATURE_MARKER = "<div>-- </div>";

/** Put the signature at the bottom of an HTML body, once. */
export function withHtmlSignature(body: string, signature: string): string {
  const trimmed = signature.trim();
  if (!trimmed) return body;
  if (body.includes(SIGNATURE_MARKER)) return body;
  return `${body}<div><br></div>${SIGNATURE_MARKER}${htmlFromPlainText(trimmed)}`;
}

/**
 * The body with the signature taken back off, for asking "has anything been
 * written here?" A composer that opens with a signature in it is still an
 * untouched composer, and closing it must not leave a draft behind.
 */
export function withoutSignature(body: string): string {
  const at = body.indexOf(SIGNATURE_MARKER);
  return at === -1 ? body : body.slice(0, at);
}
