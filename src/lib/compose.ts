/**
 * The composer's side of the seam.
 *
 * Three things live here, and nothing else does — the component is a
 * `<textarea>` with keybindings, which is the point.
 *
 * 1. **The IPC calls.** `send_message` is one Tauri command routing several
 *    operations (see `src-tauri/src/ipc/compose.rs`), because `lib.rs` belongs
 *    to another unit while these are being built in parallel. The router is
 *    hidden here: callers get `prepareDraft`, `saveDraft`, `sendDraft`,
 *    `undoSend`, `flushOutbox` as ordinary functions, so when the Rust side
 *    grows real sibling commands nothing above this file changes.
 *
 * 2. **The markdown-ish grammar**, mirroring `compose::markdown`. Two
 *    implementations of one grammar exist for one reason: the thread shows the
 *    reply the instant `⌘⏎` is pressed, before Rust has been asked anything, and
 *    that optimistic copy has to look like what will actually be sent. They are
 *    pinned to the same table of cases in `compose.test.ts` and
 *    `src-tauri/tests/compose.rs`; drift fails both suites. **The bytes that go
 *    on the wire are always Rust's** — this copy is never sent.
 *
 * 3. **A local fallback** so the composer works in a plain browser tab against
 *    the fixture data source. Without it `bun run dev` outside Tauri throws on
 *    the first keystroke, which makes the composer the one part of the app you
 *    cannot iterate on in a browser.
 */

import * as fixtures from "./fixtures";
import { isTauri } from "./ipc";
import { isBlankHtml, withoutSignature } from "./email-html";

/* -------------------------------------------------------------------------- */
/* Types — the wire shapes, camelCase, mirroring `compose::draft`              */
/* -------------------------------------------------------------------------- */

export interface Mailbox {
  name?: string;
  email: string;
}

/**
 * `adopted` is a draft written somewhere else — the phone, the web — and taken
 * over here. It is not a kind the UI ever asks for: Rust assigns it when a
 * synced draft is opened for the first time, and it exists so that rebuilding
 * the message leaves the body exactly as the other client wrote it. See
 * `compose::draft::DraftKind`.
 */
export type DraftKind = "new" | "reply" | "replyAll" | "forward" | "adopted";

/** Where a draft stands with Gmail. Mirrors `compose::draft::RemoteState`. */
export type RemoteState = "pending" | "synced" | "failed";

/**
 * The Gmail half of a draft.
 *
 * Read-only here: the editor rebuilds the draft object on every keystroke and
 * hands it back, and Rust deliberately ignores this block when it writes. The
 * one field the UI acts on is `state === "failed"`, which is the only thing
 * worth telling anybody — a draft that is only on this Mac while he reads mail
 * on his phone.
 */
export interface DraftRemote {
  state: RemoteState;
  draftId?: string | null;
  messageId?: string | null;
  threadId?: string | null;
  error?: string | null;
  syncedAt?: number;
}

/**
 * How to read `Draft.body`. Mirrors `compose::draft::BodyFormat`.
 *
 * `markdown` is what every draft written before the editor became a rich-text
 * one holds, and it is the column default in SQLite for exactly that reason.
 * The composer converts one to HTML when it opens it; nothing writes markdown
 * any more.
 */
export type BodyFormat = "markdown" | "html";

/** One file waiting to go out with a draft. Mirrors `compose::attach::Attachment`. */
export interface DraftAttachment {
  id: string;
  draftId: string;
  /** Already sanitized by Rust: this is the name the recipient will see. */
  filename: string;
  mimeType: string;
  sizeBytes: number;
  addedAt?: number;
  /** Drawn in the body, addressed by `contentId`, rather than listed under it. */
  inline?: boolean;
  /** The `Content-ID`, bare. The body points at it as `cid:<contentId>`. */
  contentId?: string;
}

export interface Draft {
  id: string;
  accountId: number;
  threadId?: number | null;
  /** The message being answered — threading follows this, not the thread. */
  replyToId?: number | null;
  kind: DraftKind;
  to: Mailbox[];
  cc: Mailbox[];
  bcc: Mailbox[];
  subject: string;
  /** The body, in whichever format `bodyFormat` names. */
  body: string;
  bodyFormat?: BodyFormat;
  updatedAt: number;
  /** Absent on a draft that has never been through Rust. See `DraftRemote`. */
  remote?: DraftRemote;
  /**
   * Read-only here, like `remote`: files are added and removed through their
   * own calls, and a draft object rebuilt on every keystroke must not be able
   * to drop one by leaving the field out.
   */
  attachments?: DraftAttachment[];
}

/** True when this draft exists here and nowhere else, and Google said why. */
export function isLocalOnly(draft: Draft): boolean {
  return draft.remote?.state === "failed";
}

/**
 * The draft's body as HTML, whatever it was stored as.
 *
 * The one place the old grammar is still read. A draft written in the
 * `<textarea>` composer opens in the rich-text one with its `**bold**` already
 * bold, rather than showing the owner his own asterisks; the conversion happens
 * once, when the composer opens it, and the first save writes HTML.
 */
export function bodyAsHtml(draft: Draft): string {
  if ((draft.bodyFormat ?? "markdown") === "html") return draft.body;
  return draft.body.trim() === "" ? "" : markdownToHtml(draft.body);
}

/**
 * Nothing worth keeping — no recipients, no subject, no files, and no words.
 *
 * Every "is this draft worth a row / a push / a confirmation before it is
 * thrown away" question runs through here. It reads the body as *text* because
 * an untouched rich-text editor is not an empty string: it holds
 * `<div><br></div>`, and a signature on top of that is still an untouched
 * composer.
 */
export function hasWrittenBody(draft: Draft): boolean {
  const body = bodyAsHtml(draft);
  // The signature does not count as having written anything: a composer that
  // opened, signed itself and was closed again is an untouched composer.
  const written = (draft.bodyFormat ?? "markdown") === "html" ? withoutSignature(body) : body;
  return !isBlankHtml(written);
}

/**
 * Does this message carry a subject the recipient will actually see?
 *
 * Trimmed, and that is the whole content of the function: a subject of spaces
 * is a subject Gmail draws as "(no subject)" at both ends, so whitespace has to
 * count as nothing or the check it guards is a check the writer can walk past
 * by holding the space bar. `isDraftEmpty` was already asking exactly this
 * question inline; the send path asks it too, so it has a name now.
 */
export function hasSubject(draft: Pick<Draft, "subject">): boolean {
  return draft.subject.trim() !== "";
}

export function isDraftEmpty(draft: Draft): boolean {
  return (
    !hasWrittenBody(draft) &&
    !hasSubject(draft) &&
    draft.to.length === 0 &&
    draft.cc.length === 0 &&
    draft.bcc.length === 0 &&
    (draft.attachments?.length ?? 0) === 0
  );
}

export type OutboxState = "holding" | "sending" | "sent" | "failed";

export interface OutboxEntry {
  id: string;
  accountId: number;
  threadId?: number | null;
  gmailThreadId?: string | null;
  subject: string;
  state: OutboxState;
  /** Nothing leaves before this instant. The undo window *is* this number. */
  sendAfter: number;
  createdAt: number;
  attempts: number;
  lastError?: string | null;
  sentMessageId?: string | null;
}

export interface FlushOutcome {
  id: string;
  sent: boolean;
  messageId?: string | null;
  error?: string | null;
  willRetry: boolean;
}

/** The spec's number, kept in step with `compose::outbox::UNDO_WINDOW_MS`. */
export const UNDO_WINDOW_MS = 10_000;

/** How long the editor waits after the last keystroke before saving. */
export const AUTOSAVE_DEBOUNCE_MS = 700;

/* -------------------------------------------------------------------------- */
/* IPC                                                                         */
/* -------------------------------------------------------------------------- */

async function call<T>(payload: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>("send_message", { draft: payload });
}

/**
 * A draft pre-filled from a conversation.
 *
 * `replyToId` names the message being answered. Without it the newest message
 * in the thread is the parent — what the strip under the conversation has
 * always meant — and with it the reply belongs to that one message: its
 * recipients, its quoted block, its place in the `References` chain. Rust reads
 * the thread back off the message, so the two arguments cannot disagree about
 * which conversation this is.
 */
export async function prepareDraft(
  threadId: number,
  kind: DraftKind,
  replyToId?: number | null,
): Promise<Draft> {
  if (!isTauri()) return localPrepare(threadId, kind, replyToId);
  const result = await call<{ draft: Draft }>({
    op: "prepare",
    threadId,
    kind,
    messageId: replyToId ?? undefined,
  });
  return result.draft;
}

/** What came across with a forward, and what would not. */
export interface ForwardedFiles {
  attachments: DraftAttachment[];
  /** Named, with Google's reason. Empty on the ordinary path. */
  refused: string[];
}

/**
 * Bring the forwarded message's files onto the draft.
 *
 * A separate call from `prepareDraft`, and after it, because the two cost
 * different things. A forward's *text* is free — Rust reproduces the original
 * out of rows that are already local, at build time — but a synced message
 * carries an attachment's name, type and size and not its bytes. Those arrive
 * only when somebody opens or saves one, so this almost always has to fetch,
 * and a fetch has no business between `f` and the composer appearing.
 *
 * Inline images are deliberately left behind: they are part of the body the
 * forward already reproduces, and attaching them would put every signature
 * graphic in the message twice.
 */
export async function forwardAttachments(
  draftId: string,
  messageId: number,
): Promise<ForwardedFiles> {
  if (!isTauri()) return { attachments: [], refused: [] };
  const { invoke } = await import("@tauri-apps/api/core");
  const result = await invoke<Partial<ForwardedFiles>>("forward_attachments", {
    draftId,
    messageId,
  });
  return { attachments: result.attachments ?? [], refused: result.refused ?? [] };
}

/**
 * A blank draft, addressed to nobody, belonging to no conversation.
 *
 * Built here rather than asked of Rust because `prepare` exists to read a
 * *thread* — its from-address, its recipients, its References header — and a
 * message you are starting has none of that to read. There is nothing to
 * infer, so there is nothing to make a round trip for. The row is written the
 * first time autosave fires, by the same `saveDraft` every other draft uses,
 * and `thread_id` is null all the way down to SQLite.
 *
 * The account is the one thing that has to be chosen rather than derived, and
 * the caller chooses it: whichever account the list is filtered to, or the
 * first one, which is the same rule the sidebar's own ordering follows.
 */
export function newDraft(accountId: number): Draft {
  return {
    id: `draft-new-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`,
    accountId,
    threadId: null,
    replyToId: null,
    kind: "new",
    to: [],
    cc: [],
    bcc: [],
    subject: "",
    body: "",
    bodyFormat: "html",
    updatedAt: 0,
    attachments: [],
  };
}

export async function loadDraftForThread(threadId: number): Promise<Draft | null> {
  if (!isTauri()) return localLookup(`thread:${threadId}`);
  const result = await call<{ draft: Draft | null }>({ op: "loadDraft", threadId });
  return result.draft;
}

/**
 * One draft, by its own id.
 *
 * The thread-keyed lookup above is what reopening a conversation uses. This is
 * what "open the draft the agent just wrote" uses, and it has to be by id: the
 * agent can leave two drafts on one thread, and the newest is not necessarily
 * the one whose button was pressed.
 */
export async function loadDraft(draftId: string): Promise<Draft | null> {
  if (!isTauri()) return localLookup(draftId);
  const result = await call<{ draft: Draft | null }>({ op: "loadDraft", draftId });
  return result.draft;
}

/**
 * The draft behind a message row in a conversation.
 *
 * A draft is mirrored into the thread it answers, so the reading pane holds a
 * *message* id and needs the editable copy. Not `loadDraftForThread`: a thread
 * can carry two drafts, and that one returns whichever was typed in last rather
 * than the one whose row was activated. Rust resolves the id, because the
 * mirror is renamed the moment Gmail accepts the push and only the store knows
 * which name it is under now.
 */
export async function loadDraftForMessage(messageId: number): Promise<Draft | null> {
  if (!isTauri()) return localLookup(`message:${messageId}`);
  const result = await call<{ draft: Draft | null }>({ op: "loadDraft", messageId });
  return result.draft;
}

export async function saveDraft(draft: Draft): Promise<Draft> {
  if (!isTauri()) return localSave(draft);
  const result = await call<{ draft: Draft }>({ op: "saveDraft", draft });
  return result.draft;
}

/**
 * What became of the Gmail copy when a draft was thrown away.
 *
 * `failed` is the one worth saying out loud: the row and the mirror are gone
 * here, the draft is still on his phone, and the next sync pass will adopt it
 * straight back into the conversation he just cleared.
 */
export interface DiscardResult {
  ok: boolean;
  remote: "none" | "deleted" | "failed";
  error?: string | null;
}

export async function discardDraft(draftId: string): Promise<DiscardResult> {
  if (!isTauri()) {
    localForget(draftId);
    return { ok: true, remote: "none" };
  }
  const result = await call<Partial<DiscardResult>>({ op: "discardDraft", draftId });
  // A build of Rust that predates the remote half answers `{ ok: true }` alone.
  return { ok: result.ok ?? true, remote: result.remote ?? "none", error: result.error };
}

/**
 * What became of the Gmail copy when a draft changed hands.
 *
 * `failed` is the one worth saying out loud, and it says something different
 * from the discard case: the draft is here, under the account that was picked,
 * and there is *also* a copy of it in the account it left. That one will come
 * back down the next sync as a draft of a message he is about to send from
 * somewhere else.
 */
export interface MoveAccountResult {
  /**
   * The row as it now stands, or `null` when there was never a row.
   *
   * A composer nobody has written in yet has not been saved — autosave
   * declines an empty draft — so `c` then `From`, which is the order somebody
   * who noticed the wrong address uses, has nothing for Rust to move. The
   * account is the composer's own until the first save carries it in.
   */
  draft: Draft | null;
  remote: "none" | "deleted" | "failed";
  error?: string | null;
}

/**
 * Send this draft from a different account.
 *
 * Not an ordinary save. Nothing Gmail knows about a draft survives the move —
 * see `draft::move_account` — and the copy in the old account has to be deleted
 * rather than left, so this is Rust's to do in one piece rather than something
 * the editor can express by writing a different `accountId` on the next
 * autosave.
 */
export async function moveDraftAccount(
  draftId: string,
  accountId: number,
): Promise<MoveAccountResult> {
  if (!isTauri()) {
    const existing = localDrafts.get(draftId);
    return {
      draft: existing ? localSave({ ...existing, accountId }) : null,
      remote: "none",
    };
  }
  const result = await call<Partial<MoveAccountResult>>({
    op: "moveDraftAccount",
    draftId,
    accountId,
  });
  return { draft: result.draft ?? null, remote: result.remote ?? "none", error: result.error };
}

/* -------------------------------------------------------------------------- */
/* Attachments                                                                 */
/* -------------------------------------------------------------------------- */

/** What an attach call did. `refused` is per file, and is never silent. */
export interface AttachResult {
  attachments: DraftAttachment[];
  added: DraftAttachment[];
  refused: string[];
}

/**
 * Open the system panel and attach whatever is chosen.
 *
 * The panel is Rust's: the capability file grants JavaScript no `dialog:`
 * permission, so the only thing this side can ask for is "let him pick files
 * for *this* draft".
 */
export async function chooseAttachments(
  draftId: string,
  inline = false,
): Promise<AttachResult> {
  if (!isTauri()) return { attachments: localAttachments(draftId), added: [], refused: [] };
  return call<AttachResult>({ op: "attachChoose", draftId, inline });
}

/** Attach files already named — what a drop on the composer produces. */
export async function attachPaths(
  draftId: string,
  paths: string[],
  inline = false,
): Promise<AttachResult> {
  if (!isTauri()) return { attachments: localAttachments(draftId), added: [], refused: [] };
  return call<AttachResult>({ op: "attachAdd", draftId, paths, inline });
}

export async function removeAttachment(attachmentId: string): Promise<DraftAttachment[]> {
  if (!isTauri()) return [];
  const result = await call<{ attachments: DraftAttachment[] }>({
    op: "attachRemove",
    attachmentId,
  });
  return result.attachments ?? [];
}

/** Move one image between the body and the list under the message. */
export async function setAttachmentInline(
  attachmentId: string,
  inline: boolean,
): Promise<DraftAttachment[]> {
  if (!isTauri()) return [];
  const result = await call<{ attachments: DraftAttachment[] }>({
    op: "attachInline",
    attachmentId,
    inline,
  });
  return result.attachments ?? [];
}

/** One inline image, with its bytes, for drawing the body as it will arrive. */
export interface InlineImageData {
  attachmentId: string;
  contentId: string;
  mimeType: string;
  filename: string;
  base64: string;
}

/**
 * The bytes of every inline image on a draft.
 *
 * Only the inline ones come back — an attached file is a name and a size in
 * the composer, and there is nothing to draw.
 */
export async function inlineImages(draftId: string): Promise<InlineImageData[]> {
  if (!isTauri()) return [];
  const result = await call<{ images: InlineImageData[] }>({ op: "attachImages", draftId });
  return result.images ?? [];
}

/** Gmail's ceiling on a message, which is the only limit worth having. */
export const MAX_ATTACHMENT_BYTES = 25 * 1024 * 1024;

/**
 * The largest image that can go *in* a body rather than beside it.
 *
 * Mirrors `compose::attach::MAX_INLINE_IMAGE_BYTES`, which is in turn the
 * number the receive side uses for an inline `cid:` image. Drawing one costs
 * base64 over IPC and a `data:` URL in the document; a 20 MB photograph is a
 * 27 MB string. Anything past this is attached, and the chip does not offer a
 * choice that Rust would refuse.
 */
export const MAX_INLINE_IMAGE_BYTES = 4 * 1024 * 1024;

export function humanSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} bytes`;
}

/**
 * Whether this file could be drawn in the body.
 *
 * A raster image, small enough to carry as a `data:` URL. SVG is excluded on
 * both sides — an inline SVG asks the recipient's client to render a document
 * from this Mac. An image already in the body keeps its control whatever its
 * size, or a picture placed by some other route could never be taken back out.
 *
 * This reads the *declared* type, because by the time a chip exists Rust has
 * already stored one. `compose::attach::inline_mime` is the stricter question
 * and the one that decides: it sniffs the bytes, so a zip named `.png` is
 * refused the body however this answers.
 */
export function isInlinableImage(file: DraftAttachment): boolean {
  if (file.inline === true) return true;
  const base = (file.mimeType ?? "").split(";")[0].trim().toLowerCase();
  if (!base.startsWith("image/") || base === "image/svg+xml") return false;
  return file.sizeBytes <= MAX_INLINE_IMAGE_BYTES;
}

function localAttachments(draftId: string): DraftAttachment[] {
  return localLookup(draftId)?.attachments ?? [];
}

/* -------------------------------------------------------------------------- */
/* Inline images in the body                                                   */
/* -------------------------------------------------------------------------- */

/**
 * What the draft body holds, and what the editor holds, are not the same string.
 *
 * The body that is saved, pushed to Gmail and eventually sent carries
 * `<img src="cid:…">` — a reference to a part of the message, which is the only
 * form that means anything to a recipient. A `cid:` resolves to nothing in a
 * webview, so the editor is handed the same markup with the `src` swapped for a
 * `data:` URL built from the bytes Rust holds. That is the whole of "renders in
 * the composer as it will in the sent mail".
 *
 * `MARKER` is what survives the swap in both directions. It is not on
 * `email-html`'s outgoing allowlist and is not on Rust's, so it is stripped at
 * send — it exists only between the draft row and the editor. The read side
 * uses the same attribute for the same job; see `lib/attachments.ts`.
 */
const MARKER = "data-mach-cid";

/** The `<img>` inserted at the caret when an image is placed in the body. */
export function inlineImageMarkup(contentId: string, filename: string): string {
  return `<img ${MARKER}="${escapeAttribute(contentId)}" src="cid:${escapeAttribute(
    contentId,
  )}" alt="${escapeAttribute(filename)}">`;
}

/**
 * A `data:` URL for one inline image, or null when there is nothing to draw.
 *
 * The read side has a function of the same shape in `lib/attachments.ts`. They
 * are not shared: that one takes an image out of a message somebody else sent
 * and sniffs the bytes before trusting the declared type, which is the right
 * caution there and pointless here, where the file came off this Mac.
 */
export function inlineImageDataUrl(image: InlineImageData): string | null {
  if (!image.base64) return null;
  const type = (image.mimeType || "application/octet-stream").split(";")[0].trim();
  return `data:${type};base64,${image.base64}`;
}

/**
 * The body as the editor should hold it: every `cid:` resolved to its bytes.
 *
 * An image whose bytes are not in `urls` keeps its `cid:` src and draws as a
 * broken image, which is honest — the alternative is a body that silently
 * differs from the one that will be sent.
 */
export function withInlineImages(html: string, urls: ReadonlyMap<string, string>): string {
  return rewriteImages(html, (contentId) => urls.get(contentId) ?? null);
}

/**
 * The body as it must be stored: every resolved image back to its `cid:`.
 *
 * The inverse of [`withInlineImages`], and the reason the marker exists. Run on
 * the way out of the editor, so a `data:` URL never reaches SQLite — a 4 MB
 * photograph would otherwise be re-encoded into the draft row on every autosave
 * and pushed to Gmail inside the HTML part as well as beside it.
 */
export function withCidReferences(html: string): string {
  return rewriteImages(html, (contentId) => `cid:${contentId}`);
}

/** Every content id the body refers to, in order, without repeats. */
export function inlineCidsIn(html: string): string[] {
  if (!html.includes("<img") || typeof DOMParser === "undefined") return [];
  const doc = new DOMParser().parseFromString(html, "text/html");
  const seen = new Set<string>();
  for (const image of doc.querySelectorAll("img")) {
    const id = contentIdOf(image);
    if (id) seen.add(id);
  }
  return [...seen];
}

/**
 * Walk the images, ask for a `src`, and hand the document back.
 *
 * Parsed rather than matched with a regular expression. Attribute order,
 * quoting and spacing are all the editor's to choose and it changes them as it
 * normalizes — an expression that agreed with today's output would come apart
 * on the day Squire wrote `src` before the marker.
 */
function rewriteImages(html: string, src: (contentId: string) => string | null): string {
  if (!html.includes("<img") || typeof DOMParser === "undefined") return html;
  const doc = new DOMParser().parseFromString(html, "text/html");
  let touched = false;
  for (const image of doc.querySelectorAll("img")) {
    const contentId = contentIdOf(image);
    if (!contentId) continue;
    const next = src(contentId);
    if (next === null) continue;
    // The marker is re-stamped rather than assumed: an image pasted from
    // elsewhere in the same message arrives through the editor's sanitizer,
    // which keeps the marker and drops the `data:` src it cannot allow.
    image.setAttribute(MARKER, contentId);
    image.setAttribute("src", next);
    touched = true;
  }
  // Untouched documents come back as they arrived. Re-serializing every body
  // that happens to contain an `<img>` would rewrite quoting and spacing on
  // every keystroke, and the diff would land in the draft row.
  return touched ? doc.body.innerHTML : html;
}

/** The content id an image is standing for, from the marker or from its src. */
function contentIdOf(image: Element): string | null {
  const marked = image.getAttribute(MARKER);
  if (marked) return marked;
  const src = image.getAttribute("src") ?? "";
  return src.toLowerCase().startsWith("cid:") ? src.slice(4) : null;
}

function escapeAttribute(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export interface SendResult {
  entry: OutboxEntry;
  undoUntil: number;
  scheduled: boolean;
}

/** Queue a message. `scheduleAt` turns the undo window into a scheduled send. */
export async function sendDraft(draft: Draft, scheduleAt?: number): Promise<SendResult> {
  if (!isTauri()) return localSend(draft, scheduleAt);
  return call<SendResult>({ op: "send", draft, scheduleAt });
}

export async function undoSend(outboxId: string): Promise<boolean> {
  if (!isTauri()) {
    localOutbox = localOutbox.filter((e) => e.id !== outboxId);
    return true;
  }
  const result = await call<{ cancelled: boolean }>({ op: "undo", outboxId });
  return result.cancelled;
}

/**
 * Send everything whose window has closed.
 *
 * Called on mount as well as on a timer, which is what makes "a message in the
 * outbox is never lost" true rather than aspirational: a reply queued in a
 * window that was then closed leaves the next time the app opens.
 */
export async function flushOutbox(): Promise<{ outcomes: FlushOutcome[]; pending: OutboxEntry[] }> {
  if (!isTauri()) {
    const pending = localOutbox.filter((e) => e.sendAfter > Date.now());
    localOutbox = pending;
    return { outcomes: [], pending };
  }
  return call<{ outcomes: FlushOutcome[]; pending: OutboxEntry[] }>({ op: "flush" });
}

export async function listOutbox(): Promise<OutboxEntry[]> {
  if (!isTauri()) return localOutbox;
  const result = await call<{ pending: OutboxEntry[] }>({ op: "outbox" });
  return result.pending;
}

/* -------------------------------------------------------------------------- */
/* Recipients                                                                  */
/* -------------------------------------------------------------------------- */

/** `Jane Doe <jane@x.com>, bob@y.com` — what the fields show and accept. */
export function formatRecipients(list: Mailbox[]): string {
  return list
    .map((m) => (m.name && m.name.trim() ? `${m.name} <${m.email}>` : m.email))
    .join(", ");
}

export function parseRecipients(input: string): Mailbox[] {
  const out: Mailbox[] = [];
  const seen = new Set<string>();
  for (const chunk of splitTopLevel(input)) {
    const trimmed = chunk.trim();
    if (!trimmed) continue;
    const parsed = parseOne(trimmed);
    const key = parsed.email.toLowerCase();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    out.push(parsed);
  }
  return out;
}

function parseOne(chunk: string): Mailbox {
  const open = chunk.lastIndexOf("<");
  const close = chunk.lastIndexOf(">");
  if (open !== -1 && close > open) {
    const name = chunk.slice(0, open).trim().replace(/^"|"$/g, "").trim();
    return { name: name || undefined, email: chunk.slice(open + 1, close).trim() };
  }
  return { email: chunk };
}

function splitTopLevel(input: string): string[] {
  const out: string[] = [];
  let start = 0;
  let quoted = false;
  for (let i = 0; i < input.length; i += 1) {
    const ch = input[i];
    if (ch === '"') quoted = !quoted;
    else if ((ch === "," || ch === ";") && !quoted) {
      out.push(input.slice(start, i));
      start = i + 1;
    }
  }
  out.push(input.slice(start));
  return out;
}

/* -------------------------------------------------------------------------- */
/* What a reply is made of — a mirror of `compose::address` and the subject     */
/* half of `compose::mime`                                                     */
/* -------------------------------------------------------------------------- */

/**
 * `Re: ` without stacking. Mirrors `compose::mime::reply_subject`.
 *
 * Only *reply* prefixes are stripped: Gmail's own answer to "Fwd: Invoice" is
 * "Re: Fwd: Invoice", and dropping the Fwd would rename somebody else's thread.
 */
export function replySubject(subject: string): string {
  const base = stripPrefixes(subject, ["re"]);
  return base === "" ? "Re:" : `Re: ${base}`;
}

/** `Fwd: ` without stacking, by the same rule. */
export function forwardSubject(subject: string): string {
  const base = stripPrefixes(subject, ["fwd", "fw"]);
  return base === "" ? "Fwd:" : `Fwd: ${base}`;
}

function stripPrefixes(subject: string, words: string[]): string {
  let rest = subject.trim();
  for (;;) {
    let stripped: string | null = null;
    for (const word of words) {
      stripped = stripOne(rest, word);
      if (stripped !== null) break;
    }
    if (stripped === null) return rest;
    rest = stripped.replace(/^\s+/, "");
  }
}

/** One leading `word:` / `word[n]:` / `word(n):`, case-insensitively. */
function stripOne(subject: string, word: string): string | null {
  const lower = subject.toLowerCase();
  if (!lower.startsWith(word)) return null;
  let rest = lower.slice(word.length).replace(/^\s+/, "");
  // `Re[2]:` and `Re(2):` are what some clients count replies with.
  const counted = /^[[(]\d*[\])]\s*/.exec(rest);
  if (counted) rest = rest.slice(counted[0].length);
  if (!rest.startsWith(":")) return null;
  // The offsets are the same in the lowercase copy.
  return subject.slice(subject.length - rest.length + 1);
}

/** The message a reply is being written against, as much of it as this matters. */
export interface ReplySource {
  from: Mailbox;
  /** `Reply-To`, when the sender set one. It wins over `from`. */
  replyTo?: Mailbox[];
  to: Mailbox[];
  cc: Mailbox[];
}

/**
 * Who a reply goes to. The rules, and the reasoning behind each, are in
 * `src-tauri/src/compose/address.rs`; this is the same arithmetic, and the two
 * are pinned to the same cases in `compose.test.ts` and `tests/compose.rs`.
 *
 * `mine` is every account address in the app — a reply-all that Ccs you at your
 * other address is still mailing you your own message. `selfAddress` decides
 * only whether the message being answered is one you wrote.
 */
export function replyRecipients(
  message: ReplySource,
  selfAddress: string,
  mine: string[],
  replyAll: boolean,
): { to: Mailbox[]; cc: Mailbox[] } {
  const me = [...mine, selfAddress].map((a) => a.trim()).filter((a) => a !== "");

  const author = (message.replyTo?.length ? message.replyTo : [message.from]).filter(
    (m) => m.email.trim() !== "",
  );
  const fromIsMe = message.from.email.trim().toLowerCase() === selfAddress.trim().toLowerCase();
  const primary = fromIsMe ? message.to.filter((m) => m.email.trim() !== "") : author;

  const to = dedupeMailboxes(without(primary, me));

  if (!replyAll) return { to: addressed(to, primary, author), cc: [] };

  const rest = [...(fromIsMe ? [] : message.to), ...message.cc];
  const already = [...to.map((m) => m.email), ...me];
  const cc = dedupeMailboxes(without(rest, already));

  // A reply-all whose To emptied out promotes the Cc: somebody else was on the
  // message and they are the better answer than your own address.
  if (to.length === 0 && cc.length > 0) return { to: cc, cc: [] };

  return { to: addressed(to, primary, author), cc };
}

/** A forward is addressed by hand. Mirrors `address::forward_recipients`. */
export function forwardRecipients(): { to: Mailbox[]; cc: Mailbox[] } {
  return { to: [], cc: [] };
}

/**
 * A reply is addressed to somebody.
 *
 * When removing your own addresses leaves nothing — a note you mailed to
 * yourself — they go back in, rather than the composer opening on a placeholder
 * where the address should be.
 */
function addressed(to: Mailbox[], primary: Mailbox[], author: Mailbox[]): Mailbox[] {
  if (to.length > 0) return to;
  const unfiltered = dedupeMailboxes(primary);
  return unfiltered.length > 0 ? unfiltered : dedupeMailboxes(author);
}

function without(list: Mailbox[], excluded: string[]): Mailbox[] {
  return list.filter(
    (m) => !excluded.some((e) => e.toLowerCase() === m.email.trim().toLowerCase()),
  );
}

/**
 * Case-insensitive dedupe on the address, first occurrence wins — except that a
 * later occurrence carrying a name beats an earlier one that had none.
 */
export function dedupeMailboxes(list: Mailbox[]): Mailbox[] {
  const out: Mailbox[] = [];
  for (const mailbox of list) {
    const email = mailbox.email.trim();
    if (email === "") continue;
    const existing = out.find((m) => m.email.toLowerCase() === email.toLowerCase());
    if (existing) {
      if (!existing.name && mailbox.name) existing.name = mailbox.name;
      continue;
    }
    out.push({ ...mailbox, email });
  }
  return out;
}

/* -------------------------------------------------------------------------- */
/* Scheduling                                                                  */
/* -------------------------------------------------------------------------- */

export interface ScheduleOption {
  label: string;
  at: number;
}

/**
 * `⌃S` is a schedule, not a date picker. Three answers cover what a person
 * actually means by "send this later"; anything else is a calendar, and there
 * is one of those a keystroke away.
 */
export function scheduleOptions(now: number = Date.now()): ScheduleOption[] {
  const at = (days: number, hour: number) => {
    const d = new Date(now);
    d.setDate(d.getDate() + days);
    d.setHours(hour, 0, 0, 0);
    return d.getTime();
  };
  const laterToday = new Date(now + 3 * 3600_000);
  const options: ScheduleOption[] = [
    { label: "In 3 hours", at: laterToday.getTime() },
    { label: "Tomorrow, 8am", at: at(1, 8) },
    { label: "Monday, 8am", at: at(((8 - new Date(now).getDay()) % 7) || 7, 8) },
  ];
  return options.filter((o) => o.at > now);
}

/* -------------------------------------------------------------------------- */
/* The markdown-ish grammar — a mirror of `compose::markdown`                   */
/* -------------------------------------------------------------------------- */

/** The plain-text part is the source. That is the whole idea of the editor. */
export function toPlainText(source: string): string {
  return source.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
}

export function markdownToHtml(source: string): string {
  const lines = toPlainText(source).split("\n");
  let out = "";
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    if (line.trim() === "") {
      i += 1;
      continue;
    }

    if (line.trimStart().startsWith("```")) {
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !lines[i].trimStart().startsWith("```")) {
        body.push(lines[i]);
        i += 1;
      }
      i += 1;
      out += `<pre><code>${escapeHtml(body.join("\n"))}</code></pre>`;
      continue;
    }

    const head = heading(line);
    if (head) {
      out += `<h${head.level}>${inline(head.text)}</h${head.level}>`;
      i += 1;
      continue;
    }

    if (quoteBody(line) !== null) {
      const body: string[] = [];
      while (i < lines.length) {
        const text = quoteBody(lines[i]);
        if (text === null) break;
        body.push(text);
        i += 1;
      }
      out += `<blockquote>${paragraph(body.join("\n"))}</blockquote>`;
      continue;
    }

    if (bulletBody(line) !== null) {
      out += "<ul>";
      while (i < lines.length) {
        const text = bulletBody(lines[i]);
        if (text === null) break;
        out += `<li>${inline(text)}</li>`;
        i += 1;
      }
      out += "</ul>";
      continue;
    }

    if (orderedBody(line) !== null) {
      out += "<ol>";
      while (i < lines.length) {
        const text = orderedBody(lines[i]);
        if (text === null) break;
        out += `<li>${inline(text)}</li>`;
        i += 1;
      }
      out += "</ol>";
      continue;
    }

    const body: string[] = [];
    while (i < lines.length && lines[i].trim() !== "" && !startsBlock(lines[i])) {
      body.push(lines[i]);
      i += 1;
    }
    out += paragraph(body.join("\n"));
  }

  return out || "<p></p>";
}

function startsBlock(line: string): boolean {
  return (
    heading(line) !== null ||
    quoteBody(line) !== null ||
    bulletBody(line) !== null ||
    orderedBody(line) !== null ||
    line.trimStart().startsWith("```")
  );
}

function heading(line: string): { level: number; text: string } | null {
  const match = /^(#{1,3}) (.*)$/.exec(line);
  return match ? { level: match[1].length, text: match[2].trim() } : null;
}

function quoteBody(line: string): string | null {
  if (!line.startsWith(">")) return null;
  const rest = line.slice(1);
  return rest.startsWith(" ") ? rest.slice(1) : rest;
}

function bulletBody(line: string): string | null {
  const match = /^\s*[-*+] (.*)$/.exec(line);
  return match ? match[1].trim() : null;
}

function orderedBody(line: string): string | null {
  const match = /^\s*\d+[.)] (.*)$/.exec(line);
  return match ? match[1].trim() : null;
}

function paragraph(body: string): string {
  return `<p>${inline(body).replace(/\n/g, "<br>")}</p>`;
}

/**
 * Code spans and URLs are lifted out before emphasis and put back after,
 * because both contain characters emphasis would eat — `_` is legal and common
 * in a URL path, and `/a_b_c` becoming `/a<em>b</em>c` inside an `href` is a
 * broken link rather than a typo.
 */
function inline(text: string): string {
  const fragments: string[] = [];
  const withoutCode = maskCodeSpans(text, fragments);
  const escaped = escapeHtml(withoutCode);
  const withoutLinks = maskLinks(escaped, fragments);
  return unmask(emphasis(withoutLinks), fragments);
}

const MASK = "\u0000";

function maskCodeSpans(text: string, fragments: string[]): string {
  let out = "";
  let rest = text;
  for (;;) {
    const open = rest.indexOf("`");
    if (open === -1) break;
    const after = rest.slice(open + 1);
    const close = after.indexOf("`");
    if (close === -1) break;
    out += rest.slice(0, open) + MASK + fragments.length + MASK;
    fragments.push(`<code>${escapeHtml(after.slice(0, close))}</code>`);
    rest = after.slice(close + 1);
  }
  return out + rest;
}

const SCHEME = /https?:\/\//;

function maskLinks(escaped: string, fragments: string[]): string {
  let out = "";
  let rest = escaped;
  for (;;) {
    const match = SCHEME.exec(rest);
    if (!match) break;
    const at = match.index;
    out += rest.slice(0, at);
    const tail = rest.slice(at);
    const stop = tail.search(/[\s<\u0000]/);
    let url = stop === -1 ? tail : tail.slice(0, stop);
    // Trailing punctuation belongs to the sentence, not to the URL.
    while (url.length > 0 && ".,)]!?:;".includes(url[url.length - 1])) {
      url = url.slice(0, -1);
    }
    if (!url) return out + tail;
    out += MASK + fragments.length + MASK;
    fragments.push(`<a href="${url}">${url}</a>`);
    rest = tail.slice(url.length);
  }
  return out + rest;
}

function unmask(text: string, fragments: string[]): string {
  let out = "";
  let rest = text;
  for (;;) {
    const open = rest.indexOf(MASK);
    if (open === -1) break;
    const after = rest.slice(open + 1);
    const close = after.indexOf(MASK);
    if (close === -1) break;
    const index = Number(after.slice(0, close));
    if (!Number.isInteger(index)) break;
    out += rest.slice(0, open) + (fragments[index] ?? "");
    rest = after.slice(close + 1);
  }
  return out + rest;
}

function emphasis(text: string): string {
  return wrap(wrap(wrap(text, "**", "strong"), "*", "em"), "_", "em");
}

/** An unmatched marker is left alone: a lone asterisk is punctuation. */
function wrap(text: string, marker: string, tag: string): string {
  let out = "";
  let rest = text;
  for (;;) {
    const start = rest.indexOf(marker);
    if (start === -1) break;
    const after = rest.slice(start + marker.length);
    const end = after.indexOf(marker);
    if (end === -1) break;
    const inner = after.slice(0, end);
    if (inner.trim() === "" || inner.startsWith(" ") || inner.endsWith(" ")) {
      out += rest.slice(0, start + marker.length);
      rest = after;
      continue;
    }
    out += `${rest.slice(0, start)}<${tag}>${inner}</${tag}>`;
    rest = after.slice(end + marker.length);
  }
  return out + rest;
}

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/* -------------------------------------------------------------------------- */
/* Browser fallback                                                            */
/* -------------------------------------------------------------------------- */

/**
 * Drafts, keyed three ways: by their own id, by `thread:<id>`, and by
 * `message:<id>` — the same three questions `compose::draft` answers out of
 * SQLite.
 *
 * The seed is the fixture draft `fixtures.ts` mirrors into a conversation.
 * Without it, activating that row in `bun run dev` would report "not editable",
 * and the browser is the only place this flow can be walked without a mailbox.
 */
const localDrafts = new Map<string, Draft>();
let localOutbox: OutboxEntry[] = [];

/**
 * Seeded on first use rather than at module load: this file and `fixtures` sit
 * in the same import cycle through `ipc` and `data`, and reading the namespace
 * while that cycle is still unwinding is a `ReferenceError` at start-up.
 */
let seeded = false;
function localSeed(): void {
  if (seeded) return;
  seeded = true;
  const seed: Draft = {
    id: fixtures.DRAFT_ID,
    accountId: 2,
    threadId: fixtures.DRAFT_THREAD_ID,
    replyToId: null,
    kind: "reply",
    to: [{ name: "Marcus Oyelaran", email: "marcus@lumen.example" }],
    cc: [],
    bcc: [],
    subject: "Re: Checkout conversion dropped 6% overnight",
    body: fixtures.DRAFT_BODY,
    // The fixture body is the old grammar, which is exactly what makes it a
    // useful seed: opening it in a browser tab exercises the conversion a real
    // draft written last week goes through.
    bodyFormat: "markdown",
    updatedAt: 0,
    attachments: [],
  };
  localDrafts.set(seed.id, seed);
  localDrafts.set(`thread:${fixtures.DRAFT_THREAD_ID}`, seed);
  localDrafts.set(`message:${fixtures.DRAFT_MESSAGE_ID}`, seed);
}

/** Every fixture-mode read goes through this, so the seed is always there. */
function localLookup(key: string): Draft | null {
  localSeed();
  return localDrafts.get(key) ?? null;
}

/**
 * `prepare`, against the fixture source.
 *
 * It reads the conversation the same way `draft::prepare` does — the last
 * message somebody actually sent, the account it arrived on, every account the
 * app holds — so `r` in a browser tab opens the composer a real reply opens:
 * addressed, with a subject. It used to answer an empty draft, which made the
 * one keystroke this file exists to support unexercisable outside Tauri.
 */
function localPrepare(threadId: number, kind: DraftKind, replyToId?: number | null): Draft {
  const thread = fixtures.threads.find((t) => t.id === threadId);
  const messages = fixtures.messagesByThread.get(threadId) ?? [];
  // The message being answered when one was named, and otherwise not the last
  // message: a draft mirrored into the conversation is one, and threading a
  // reply onto your own unsent text is `context_for_thread`'s bug to avoid here
  // too. A named message that is a draft, or is not in this conversation, falls
  // back the same way rather than producing a reply to nothing.
  const named = replyToId == null ? undefined : messages.find((m) => m.id === replyToId);
  const parent =
    (named && !named.isDraft ? named : undefined) ??
    [...messages].reverse().find((m) => !m.isDraft) ??
    messages[messages.length - 1];
  const accountId = thread?.accountId ?? parent?.accountId ?? 0;
  const account = fixtures.accounts.find((a) => a.id === accountId);
  const subject = thread?.subject ?? "";

  const { to, cc } =
    parent && kind !== "forward" && kind !== "new"
      ? replyRecipients(
          { from: parent.from, to: parent.to, cc: parent.cc },
          account?.email ?? "",
          fixtures.accounts.map((a) => a.email),
          kind === "replyAll",
        )
      : forwardRecipients();

  return {
    id: `draft-local-${threadId}-${kind}`,
    accountId,
    threadId,
    replyToId: parent?.id ?? null,
    kind,
    to,
    cc,
    bcc: [],
    subject:
      kind === "forward" ? forwardSubject(subject) : kind === "new" ? "" : replySubject(subject),
    body: "",
    bodyFormat: "html",
    updatedAt: 0,
    attachments: [],
  };
}

function localSave(draft: Draft): Draft {
  const saved = { ...draft, updatedAt: Date.now() };
  localDrafts.set(saved.id, saved);
  if (saved.threadId != null) localDrafts.set(`thread:${saved.threadId}`, saved);
  return saved;
}

function localForget(draftId: string): void {
  const existing = localDrafts.get(draftId);
  localDrafts.delete(draftId);
  if (existing?.threadId != null) localDrafts.delete(`thread:${existing.threadId}`);
}

function localSend(draft: Draft, scheduleAt?: number): SendResult {
  const now = Date.now();
  // Mirrors `ipc::compose`: a named instant wins, whatever it is. It used to
  // have to be in the future, which quietly turned "send with no delay at all"
  // — a legal setting — back into the default ten seconds in a browser tab.
  const sendAfter = scheduleAt == null ? now + UNDO_WINDOW_MS : Math.max(scheduleAt, now);
  const entry: OutboxEntry = {
    id: `ob-local-${now}`,
    accountId: draft.accountId,
    threadId: draft.threadId ?? null,
    gmailThreadId: null,
    subject: draft.subject,
    state: "holding",
    sendAfter,
    createdAt: now,
    attempts: 0,
  };
  localOutbox = [...localOutbox, entry];
  localForget(draft.id);
  return { entry, undoUntil: sendAfter, scheduled: sendAfter > now + UNDO_WINDOW_MS };
}

/* -------------------------------------------------------------------------- */
/* Keys                                                                        */
/* -------------------------------------------------------------------------- */

/**
 * The composer's bindings, as data, so the component and its tests cannot
 * disagree about them.
 *
 * `send` and `schedule` are the two that matter, and both must be registered
 * with `allowInInput: true`: every other binding in Mach is deliberately dead
 * while you are typing, and a send key that follows that rule would only work
 * when the editor was not focused — which is never.
 */
export const COMPOSER_KEYS = {
  send: "mod+enter",
  schedule: "ctrl+s",
  close: "escape",
  /**
   * Throw the draft away.
   *
   * This was ⌘⌫, Apple Mail's key for the same act, and it cost a message. On
   * macOS ⌘⌫ is a text-editing key: delete to the beginning of the line, which
   * is a thing you press several times in a row while writing. The binding
   * carries `allowInInput: true`, so it answered in the editor, and the two
   * presses that discard a draft — one to ask, one to mean it — threw a
   * half-written message away instead of deleting two lines of it. Reported
   * from inside the app as "command+delete should delete a line, but instead it
   * deletes the whole draft".
   *
   * ⇧⌘⌫ is not a system editing command, so there is nothing for it to collide
   * with, and it joins the composer's other shifted keys — `attach`, `popOut`,
   * `composeAnother` — which are shifted for the same reason: every one of them
   * is pressed while the cursor is in a field.
   *
   * The calendar's `mod+backspace` deletes an event and is untouched. It is
   * live only while the event modal is up, and the two keys no longer overlap.
   */
  discard: "shift+mod+backspace",
  /** Attach files. Gmail has no key for this; ⇧⌘A is free and reads as "attach". */
  attach: "shift+mod+a",
  /** Recall a message inside its window. */
  undoSend: "mod+z",
  /** Gmail's `c`. Mode-scoped: the calendar's `c` creates an event. */
  compose: "c",
  /**
   * A second message while the first one is still open.
   *
   * `c` cannot do this job on its own: it is a bare letter, so it is dead while
   * you are typing, and the new-message overlay traps focus inside a panel where
   * you always are. Gmail has no key for it because Gmail opens its composers
   * from the list. This is the smallest addition that makes "more than one at a
   * time" reachable without the mouse.
   */
  composeAnother: "shift+mod+c",
  reply: "r",
  replyAll: "a",
  forward: "f",
  /**
   * Take the draft off the conversation and give it the window, or put it back.
   *
   * Gmail pops its composer out with a modifier held over a mouse target and
   * has no key for it, so there is nothing to match and nothing to diverge
   * from. ⇧⌘O is free in this app, reads as "out", and is a modified letter —
   * which it has to be, because it is pressed while typing.
   */
  popOut: "shift+mod+o",
} as const;

/**
 * Which of the open composers belongs on screen beside this conversation.
 *
 * A new message floats over the window and is shown wherever you are. A reply
 * is drawn only inside its own thread: under somebody else's it would read as a
 * reply to *that*.
 *
 * Lives here rather than inline in the dock because `r`, `a` and `f` are gated
 * on it, and the gate is where this went wrong. They used to ask whether *any*
 * composer was open — which meant a reply left on one conversation killed all
 * three keys on every other one, silently, while the footer under the message
 * went on offering them by name. The question a reply key has to ask is about
 * the conversation in front of it, and this is that question.
 */
export function visibleComposer<T extends { kind: DraftKind; threadId?: number | null }>(
  draft: T | null,
  threadId: number | null,
): T | null {
  if (!draft) return null;
  const belongs =
    draft.kind === "new" || draft.threadId == null || draft.threadId === threadId;
  return belongs ? draft : null;
}

/**
 * What `r` does, given what is already on screen.
 *
 * `focus` rather than nothing when this conversation's composer is up: it is
 * the only keyboard route back into a half-written message. ⇥ steps between the
 * rail and the list on purpose, so a caret knocked out of the composer by one
 * click in the list had no way home.
 */
export function replyKeyAim(visible: unknown): "focus" | "open" {
  return visible ? "focus" : "open";
}

/**
 * The marker the composer's root carries, so the shell can tell where the
 * keyboard is.
 */
export const COMPOSER_ROOT = "data-mach-composer";

/**
 * Is the keyboard inside a composer right now?
 *
 * Mail mode binds ⇥ to "sidebar or list", which is right everywhere except
 * here. The keymap already declines ordinary bindings while the target is a
 * field, so ⇥ out of To behaved — but the moment focus sat on a *button* in the
 * composer (cc / bcc, attach, discard, pop out) the binding fired and threw the
 * keyboard out of a half-written message into the rail. In a docked forward
 * that made the Subject field unreachable: ⇧⇥ out of To landed on cc / bcc, and
 * the next ⇧⇥ left the composer entirely, one stop short of it.
 *
 * Read off `document.activeElement` rather than off `ui`, because "where the
 * keyboard is" is a fact about the DOM and nothing in the store tracks it at
 * this resolution.
 */
export function keyboardInComposer(): boolean {
  if (typeof document === "undefined") return false;
  const active = document.activeElement;
  return active instanceof Element && active.closest(`[${COMPOSER_ROOT}]`) !== null;
}

/* -------------------------------------------------------------------------- */
/* Autosave                                                                    */
/* -------------------------------------------------------------------------- */

export interface Autosave {
  /** Note a change. Saves once the typing stops. */
  queue(draft: Draft): void;
  /** Save what is pending right now — closing, sending, switching threads. */
  flush(): void;
  /** Forget what is pending, without saving. */
  cancel(): void;
}

/**
 * Debounced autosave.
 *
 * A crash must not lose typing, so the draft is written to SQLite rather than
 * held in the webview — but writing on every keystroke would put a transaction
 * behind the cursor. Debouncing is the compromise, and `flush` is what closes
 * the remaining gap: every path that leaves the editor calls it, so the only
 * unsaved state that can exist is the last `AUTOSAVE_DEBOUNCE_MS` of typing
 * before an actual crash.
 */
export function createAutosave(
  save: (draft: Draft) => void,
  delayMs: number = AUTOSAVE_DEBOUNCE_MS,
): Autosave {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: Draft | null = null;

  const clear = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };

  return {
    queue(draft) {
      pending = draft;
      clear();
      timer = setTimeout(() => {
        timer = null;
        const next = pending;
        pending = null;
        if (next) save(next);
      }, delayMs);
    },
    flush() {
      clear();
      const next = pending;
      pending = null;
      if (next) save(next);
    },
    cancel() {
      clear();
      pending = null;
    },
  };
}
