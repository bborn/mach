/**
 * The frontend half of attachments.
 *
 * Everything that decides *what* is shown, *what* is fetched and *what* is
 * substituted into a message body lives here as a pure function or an
 * injectable call, so `src/lib/attachments.test.ts` can drive it without a
 * Tauri runtime, a WebView or a mailbox.
 *
 * Nothing here fetches anything on its own. `openAttachment` and
 * `saveAttachment` are called from a click or an Enter key on a specific
 * attachment; `fetchInlineImages` is called for a message the reader has
 * already expanded. The Rust side enforces that too — see
 * `ipc::attachments` — but the rule is stated on both sides because it is a
 * rule about the product, not about a module.
 */

import type { AttachmentId, MessageId } from "@/types";

/* -------------------------------------------------------------------------- */
/* The transport                                                               */
/* -------------------------------------------------------------------------- */

export type InvokeFn = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

let invoker: InvokeFn | null = null;

/**
 * Test seam, the same one `src/lib/message-body.ts` uses. This module owns
 * three commands and nothing else, so it takes its own transport rather than
 * reaching into the app's data source — which would drag `MachDataSource`, and
 * therefore every fixture, into a feature that has no fixture form.
 */
export function setAttachmentInvoker(fn: InvokeFn | null): void {
  invoker = fn;
}

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (invoker) return invoker<T>(command, args);
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  return tauriInvoke<T>(command, args);
}

/* -------------------------------------------------------------------------- */
/* Payloads                                                                    */
/* -------------------------------------------------------------------------- */

/** Mirrors `ipc::attachments::AttachmentFile`. */
export interface AttachmentFile {
  attachmentId: AttachmentId;
  /** The **sanitized** name — the one the file on disk actually has. */
  filename: string;
  mimeType: string;
  sizeBytes: number;
  path: string;
  fromCache: boolean;
}

/** Mirrors `ipc::attachments::SavedAttachment`. `path` is null on cancel. */
export interface SavedAttachment {
  path: string | null;
  filename: string;
}

/** Mirrors `ipc::attachments::InlineImage`. */
export interface InlineImage {
  contentId: string;
  mimeType: string;
  base64: string;
}

/** Download if needed, then hand the file to the system handler. */
export function openAttachment(attachmentId: AttachmentId): Promise<AttachmentFile> {
  return invoke<AttachmentFile>("attachment_open", { attachmentId });
}

/** Download if needed, then show the save panel. */
export function saveAttachment(attachmentId: AttachmentId): Promise<SavedAttachment> {
  return invoke<SavedAttachment>("attachment_save", { attachmentId });
}

/** Resolve one `cid:` reference to bytes. */
export function fetchInlineImage(
  messageId: MessageId,
  contentId: string,
): Promise<InlineImage> {
  return invoke<InlineImage>("attachment_inline_image", { messageId, contentId });
}

/* -------------------------------------------------------------------------- */
/* Display                                                                     */
/* -------------------------------------------------------------------------- */

/**
 * A size a person can read at a glance.
 *
 * Decimal-ish on purpose: "1.2 MB" is what every other mail client and every
 * file manager on the machine says, and being technically right about mebibytes
 * would make Mach the only app disagreeing about the size of the same file.
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * The families of file the chip row has an icon for.
 *
 * Deliberately coarse. An icon per file type is a taxonomy nobody reads; what a
 * reader actually wants from an icon is "picture or document or spreadsheet",
 * decided fast, at 12 pixels.
 */
export type AttachmentKind =
  | "image"
  | "audio"
  | "video"
  | "archive"
  | "spreadsheet"
  | "code"
  | "document"
  | "executable"
  | "file";

const EXTENSION_KINDS: ReadonlyArray<readonly [AttachmentKind, readonly string[]]> = [
  ["spreadsheet", ["csv", "tsv", "xls", "xlsx", "numbers", "ods"]],
  ["archive", ["zip", "gz", "tgz", "bz2", "xz", "7z", "rar", "tar"]],
  ["code", ["json", "xml", "yml", "yaml", "toml", "html", "css", "ts", "tsx", "diff", "patch"]],
  ["document", ["pdf", "doc", "docx", "pages", "rtf", "odt", "txt", "md", "key", "ppt", "pptx"]],
];

/**
 * What kind of thing is this, for the icon.
 *
 * The MIME type is consulted first because it is what the sending mail client
 * believed, and the extension second because plenty of senders declare
 * `application/octet-stream` for everything. Neither is trusted for anything
 * that matters — the decision about whether a file may be *opened* is made in
 * Rust, on the sanitized name, and this function's answer never reaches it.
 */
export function attachmentKind(mimeType: string, filename: string): AttachmentKind {
  const mime = mimeType.split(";")[0]?.trim().toLowerCase() ?? "";
  if (isExecutable(filename, mimeType)) return "executable";
  if (mime.startsWith("image/")) return "image";
  if (mime.startsWith("audio/")) return "audio";
  if (mime.startsWith("video/")) return "video";
  if (mime === "application/pdf") return "document";

  const extension = extensionOf(filename);
  if (extension) {
    for (const [kind, extensions] of EXTENSION_KINDS) {
      if (extensions.includes(extension)) return kind;
    }
  }
  if (mime.startsWith("text/")) return "document";
  return "file";
}

/** The final extension, lowercased. `null` when there isn't one. */
export function extensionOf(filename: string): string | null {
  const dot = filename.lastIndexOf(".");
  if (dot <= 0 || dot === filename.length - 1) return null;
  return filename.slice(dot + 1).toLowerCase();
}

/**
 * Extensions that mean "a program".
 *
 * A **hint**, not a control. The authoritative refusal is
 * `attachments::names::is_dangerous` in Rust, which is what actually stands
 * between a message and LaunchServices; this list is shorter and exists so the
 * chip can say so before the reader clicks and gets an error. Keeping it short
 * is deliberate: a UI hint that drifts out of date is a cosmetic bug, whereas a
 * second half-copy of a security list that people start trusting is not.
 */
const EXECUTABLE_HINTS = [
  "exe", "com", "scr", "bat", "cmd", "msi", "ps1", "vbs", "jar", "lnk",
  "app", "command", "dmg", "pkg", "scpt", "webloc", "sh", "bash", "dylib", "so",
];

export function isExecutable(filename: string, mimeType: string): boolean {
  const extension = extensionOf(filename);
  if (extension && EXECUTABLE_HINTS.includes(extension)) return true;
  const mime = mimeType.split(";")[0]?.trim().toLowerCase() ?? "";
  return mime === "application/x-msdownload" || mime === "application/x-mach-binary";
}

/** The screen-reader name for one chip. Everything the eye gets, in words. */
export function attachmentLabel(filename: string, mimeType: string, sizeBytes: number): string {
  const kind = attachmentKind(mimeType, filename);
  const suffix = kind === "executable" ? ", a program Mach will not open" : "";
  return `${filename}, ${formatBytes(sizeBytes)}${suffix}`;
}

/* -------------------------------------------------------------------------- */
/* Inline images                                                               */
/* -------------------------------------------------------------------------- */

/**
 * Every `cid:` reference the sanitizer left in a body, deduplicated.
 *
 * The pattern matches what `render::sanitize::promote_marker` emits and nothing
 * else: a `data-mach-cid` attribute immediately followed by the placeholder
 * `src` it replaced. The captured value is constrained to the same character
 * class the Rust side allows through — a Content-ID is an addr-spec, and
 * anything outside that shape is a smuggling attempt rather than a reference
 * that could be resolved.
 */
const CID_REFERENCE_SOURCE = 'data-mach-cid="([A-Za-z0-9.\\-_+%@]{1,512})"\\s+src="[^"]*"';

/**
 * A fresh regex per call, never a shared one.
 *
 * A `/g` regex is a stateful object: `lastIndex` survives between uses, and the
 * two functions below use it in different ways (`matchAll` reads that state,
 * `replace` resets it). Sharing one instance happens to work today and is one
 * refactor away from a bug where the second body in a thread loses its first
 * inline image. A regex is cheap; the class of bug is not.
 */
function cidReference(): RegExp {
  return new RegExp(CID_REFERENCE_SOURCE, "g");
}

export function contentIdsIn(html: string): string[] {
  const found = new Set<string>();
  for (const match of html.matchAll(cidReference())) {
    if (match[1]) found.add(match[1]);
  }
  return [...found];
}

/** `data:` URLs we are willing to write into a frame. Matches `sniff_raster_image`. */
const INLINE_MIME_TYPES = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "image/bmp",
  "image/x-icon",
]);

/**
 * Turn a resolved inline image into the `data:` URL that will be its `src`.
 *
 * Returns `null` for anything that is not one of the raster types the Rust side
 * sniffs for, or whose payload is not strict base64. The Rust side already
 * guarantees both — this is the check that means a future change over there
 * cannot quietly widen what this side writes into a document.
 */
export function inlineImageUrl(image: InlineImage): string | null {
  if (!INLINE_MIME_TYPES.has(image.mimeType)) return null;
  if (!/^[A-Za-z0-9+/]*={0,2}$/.test(image.base64) || image.base64.length === 0) return null;
  return `data:${image.mimeType};base64,${image.base64}`;
}

/**
 * Put the resolved images back into the body.
 *
 * # Why this is a string substitution and why that is safe
 *
 * The ideal shape is a DOM property assignment inside the frame, the way
 * `revealBlockedImages` handles deferred remote images — invariant 4 of
 * `docs/message-rendering-invariants.md`. That assignment would have to live in
 * `MessageFrame`, which this unit does not own, so the substitution happens
 * here on the string instead.
 *
 * It is safe, and not by luck:
 *
 * * The input is **not** sender HTML. It is ammonia's re-serialization of a
 *   parsed DOM, so every attribute value is escaped and the first `"` after an
 *   attribute opens is unambiguously the end of it.
 * * The pattern only matches the exact pair `render::sanitize` emits, and the
 *   Content-ID it captures is restricted to `[A-Za-z0-9.\-_+%@]`.
 * * The replacement is a URL **we** built from an allowlisted MIME type and a
 *   strict-base64 payload, so it can contain no quote and cannot end the
 *   attribute early.
 * * A sender cannot pre-arm this: `render::sanitize::filter_attribute` drops
 *   every incoming `data-mach*` attribute, so no `<img>` in the output carries
 *   a `data-mach-cid` that Mach did not put there. The worst a sender can do is
 *   write the literal text into a text node, where a match rewrites text into
 *   other text and changes nothing that renders.
 * * Only Content-IDs present in `resolved` are substituted, so an unresolved
 *   reference keeps its placeholder pixel rather than becoming a broken image.
 */
export function applyInlineImages(html: string, resolved: ReadonlyMap<string, string>): string {
  if (!html || resolved.size === 0) return html;
  return html.replace(cidReference(), (whole, contentId: string) => {
    const url = resolved.get(contentId);
    return url ? `data-mach-cid="${contentId}" src="${url}"` : whole;
  });
}

/**
 * Resolve every `cid:` in a body, tolerating the ones that cannot be resolved.
 *
 * A message with six inline images and one missing part should render five of
 * them and a placeholder, not an error bar. So each fetch is settled
 * independently and a failure drops that one reference rather than the batch.
 */
export async function fetchInlineImages(
  messageId: MessageId,
  contentIds: readonly string[],
  fetcher: (messageId: MessageId, contentId: string) => Promise<InlineImage> = fetchInlineImage,
): Promise<Map<string, string>> {
  const resolved = new Map<string, string>();
  const settled = await Promise.allSettled(
    contentIds.map(async (contentId) => [contentId, await fetcher(messageId, contentId)] as const),
  );
  for (const outcome of settled) {
    if (outcome.status !== "fulfilled") continue;
    const [contentId, image] = outcome.value;
    const url = inlineImageUrl(image);
    if (url) resolved.set(contentId, url);
  }
  return resolved;
}
