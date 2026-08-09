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

import { isTauri } from "./ipc";

/* -------------------------------------------------------------------------- */
/* Types — the wire shapes, camelCase, mirroring `compose::draft`              */
/* -------------------------------------------------------------------------- */

export interface Mailbox {
  name?: string;
  email: string;
}

export type DraftKind = "new" | "reply" | "replyAll" | "forward";

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
  /** Markdown-ish source, exactly as typed. */
  body: string;
  updatedAt: number;
  /** Absent on a draft that has never been through Rust. See `DraftRemote`. */
  remote?: DraftRemote;
}

/** True when this draft exists here and nowhere else, and Google said why. */
export function isLocalOnly(draft: Draft): boolean {
  return draft.remote?.state === "failed";
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

export async function prepareDraft(threadId: number, kind: DraftKind): Promise<Draft> {
  if (!isTauri()) return localPrepare(threadId, kind);
  const result = await call<{ draft: Draft }>({ op: "prepare", threadId, kind });
  return result.draft;
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
    updatedAt: 0,
  };
}

export async function loadDraftForThread(threadId: number): Promise<Draft | null> {
  if (!isTauri()) return localDrafts.get(`thread:${threadId}`) ?? null;
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
  if (!isTauri()) return localDrafts.get(draftId) ?? null;
  const result = await call<{ draft: Draft | null }>({ op: "loadDraft", draftId });
  return result.draft;
}

export async function saveDraft(draft: Draft): Promise<Draft> {
  if (!isTauri()) return localSave(draft);
  const result = await call<{ draft: Draft }>({ op: "saveDraft", draft });
  return result.draft;
}

export async function discardDraft(draftId: string): Promise<void> {
  if (!isTauri()) {
    localForget(draftId);
    return;
  }
  await call({ op: "discardDraft", draftId });
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

const localDrafts = new Map<string, Draft>();
let localOutbox: OutboxEntry[] = [];

function localPrepare(threadId: number, kind: DraftKind): Draft {
  return {
    id: `draft-local-${threadId}-${kind}`,
    accountId: 0,
    threadId,
    replyToId: null,
    kind,
    to: [],
    cc: [],
    bcc: [],
    subject: "",
    body: "",
    updatedAt: 0,
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
  /** Recall a message inside its window. */
  undoSend: "mod+z",
  /** Gmail's `c`. Mode-scoped: the calendar's `c` creates an event. */
  compose: "c",
  reply: "r",
  replyAll: "a",
  forward: "f",
} as const;

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
