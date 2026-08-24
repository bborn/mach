/**
 * ⌘⌥C — everything on screen, as text, on the clipboard.
 *
 * The ask was "copy this whole thing I'm looking at so I can paste it into an
 * LLM". Mach already had to answer *exactly* that question, for the agent: ⌘K
 * from anywhere attaches whatever is on screen, and `agent::context::render`
 * turns those references into text a model reads. So this is not a second
 * serialiser and must never become one. It builds the same `ContextItem[]` the
 * ⌘K handoff builds, hands them to the same Rust function, and asks for the
 * clipboard's budget instead of the model's — see `Audience` in
 * `src-tauri/src/agent/context.rs` for what that changes and why.
 *
 * Two consequences worth keeping:
 *
 *   * what gets copied and what the agent gets can drift only in *how much* of
 *     a conversation they carry, never in what the app thinks "this" is;
 *   * the copy resolves against SQLite. It is a local read of rows that are
 *     already on screen, so it lands in single-digit milliseconds and cannot
 *     wait on Google.
 *
 * # The clipboard is the whole point, and it is still egress
 *
 * This puts the contents of his mail on the system clipboard because that is
 * what he asked for. Nothing here logs the text, keeps a copy of it, or sends
 * it anywhere — the text does not even cross the IPC boundary: Rust renders it
 * and hands it to the pasteboard, and what comes back is a character count.
 */

import type { CalendarEvent, Thread } from "@/types";
import type { PaletteContext, PaletteResolver, PaletteResult } from "./palette/resolver";
import { contextFor, type ContextItem, type ShellView } from "./agent";
import { isTauri, tauriTransport, toMachError } from "./ipc";

/* -------------------------------------------------------------------------- */
/* What the search view is showing                                             */
/* -------------------------------------------------------------------------- */

/**
 * Search is a mode of the list pane with its own state, not a field on
 * `useMach` — `SearchView` owns the query and the rows, and nothing outside it
 * can see either. This is how it says so, in the same two-line-store shape
 * `lib/agent.ts` uses for the ⌘K ask: a plain function, no hooks, and the view
 * publishes into it rather than the copy path reaching in.
 *
 * Null whenever the search is closed, which is what makes the ordinary mailbox
 * the answer again the moment Escape is pressed.
 */
export interface SearchSnapshot {
  query: string;
  results: readonly Thread[];
}

let search: SearchSnapshot | null = null;

export function publishSearch(snapshot: SearchSnapshot | null): void {
  search = snapshot;
}

export function currentSearch(): SearchSnapshot | null {
  return search;
}

/* -------------------------------------------------------------------------- */
/* What "this whole thing" is                                                  */
/* -------------------------------------------------------------------------- */

/** Everything the copy needs to know about the shell, gathered by the caller. */
export interface CopyableView extends ShellView {
  /** Left out of `ShellView` because only this path attaches it. */
  visibleThreads?: readonly Thread[];
  visibleEvents?: readonly CalendarEvent[];
}

/**
 * The items ⌘⌥C copies, per surface.
 *
 * | on screen | what lands on the clipboard |
 * |---|---|
 * | a conversation open | every message in it: sender, date, body |
 * | a mailbox, cursor on a row | that conversation |
 * | a mailbox, nothing selected | the mailbox and the rows in view |
 * | a search | the query and its results |
 * | a search with a result open | that conversation, and the query |
 * | an event selected | the event and the range on screen |
 * | the calendar, nothing selected | the range on screen and the events in it |
 *
 * Two rules hold the table together. **A named conversation beats the list**:
 * whether it is open or merely under the cursor, it is the thing he pointed at,
 * and the list beside it is where he came from — attaching both put a numbered
 * index of thirty-four unrelated subject lines around the one conversation he
 * meant, and it was most of the payload. And **a copy never refuses**: the
 * thinnest surface in the app, an empty mailbox, still copies the line naming
 * it, which is a true answer to "what am I looking at" even if it is a short
 * one.
 */
export function copyableContext(view: CopyableView): ContextItem[] {
  const snapshot = currentSearch();
  const withSearch: CopyableView =
    snapshot && view.mode === "mail"
      ? { ...view, search: snapshot.query, visibleThreads: snapshot.results }
      : view;
  return contextFor(withSearch, { listing: true });
}

/**
 * The one message somebody pointed at, rather than the conversation round it.
 *
 * `copyableContext` above answers "what am I looking at", and in a thread that
 * is every message in it. This answers a narrower question the reading pane can
 * ask and the shell cannot: *this* message, the one under the pointer or the
 * cursor.
 *
 * It is the same item shape and it goes to the same renderer — `messageId`
 * narrows the expansion `threadId` already does, so the rows, the scrubbing and
 * the fence are all the ones a conversation gets. See
 * `agent::context::ContextItem`.
 *
 * The label carries the sender as well as the subject because a block headed
 * `[message] Invoice` says nothing a paste-reader could not have guessed, and
 * whose message it was is the first thing they need.
 */
export function copyableMessage(message: {
  threadId: number;
  messageId: number;
  subject: string;
  from?: string;
}): ContextItem[] {
  const subject = message.subject.trim();
  const from = message.from?.trim();
  const label = [from, subject].filter(Boolean).join(" — ") || "Message";
  return [
    {
      id: `message:${message.messageId}`,
      kind: "message",
      label,
      threadId: message.threadId,
      messageId: message.messageId,
    },
  ];
}

/* -------------------------------------------------------------------------- */
/* IPC                                                                         */
/* -------------------------------------------------------------------------- */

/** What came back from a copy: how much, and whether anything was left out. */
export interface CopyReceipt {
  /** Characters put on the clipboard. `0` means the view had nothing in it. */
  chars: number;
  /** A body was clipped, or a message did not fit under the ceiling. */
  truncated: boolean;
}

const NEEDS_DESKTOP = "Copying the view needs the desktop app — this is a browser tab.";

/**
 * Render the block and put it on the system pasteboard, in one call.
 *
 * The write is Rust's rather than the webview's, and that is not an arbitrary
 * split — see `src-tauri/src/clipboard.rs`. The short version: WebKit's
 * `navigator.clipboard` needs the document to be frontmost *and* a trusted user
 * gesture, so a copy driven from the QA port could never succeed, and a feature
 * that cannot be looked at in the real app has no business being in this
 * codebase.
 *
 * The text never comes back over IPC. Nothing here needs it, and not returning
 * it means the contents of his mail exist in exactly two places: SQLite, and the
 * pasteboard he asked for.
 */
export async function copyContextText(context: ContextItem[]): Promise<CopyReceipt> {
  if (!isTauri()) throw toMachError(NEEDS_DESKTOP);
  try {
    const raw = await tauriTransport.invoke<Partial<CopyReceipt>>("copy_context_text", {
      context,
    });
    return {
      chars: typeof raw?.chars === "number" ? raw.chars : 0,
      truncated: raw?.truncated === true,
    };
  } catch (error) {
    throw toMachError(error);
  }
}

/* -------------------------------------------------------------------------- */
/* What the toast says                                                         */
/* -------------------------------------------------------------------------- */

/**
 * A copy that succeeds silently is indistinguishable from one that failed, so
 * this always produces a sentence — and it names the thing rather than the act,
 * because "Copied" on its own does not say *what*.
 *
 * The cap is said out loud for the same reason. The text itself already carries
 * a line where it stops; this is the half he sees without pasting.
 */
export function describeCopy(items: readonly ContextItem[], truncated: boolean): string {
  const named =
    items.find((item) => item.kind === "message") ??
    items.find((item) => item.kind === "thread") ??
    items.find((item) => item.kind === "event") ??
    items.find((item) => item.kind === "search") ??
    items.find((item) => item.kind === "mailbox") ??
    items.find((item) => item.kind === "selection") ??
    items[0];

  const what = named ? `“${named.label}”` : "the view";
  const rows = items.find((item) => item.id === "listing");
  const alongside = named && rows && named !== rows ? ` and ${rows.label}` : "";
  return `Copied ${what}${alongside}${truncated ? " — trimmed to fit" : ""}`;
}

/* -------------------------------------------------------------------------- */
/* ⌘K                                                                          */
/* -------------------------------------------------------------------------- */

/**
 * The request store, in the shape `lib/agent.ts` already uses.
 *
 * A palette resolver is a plain function with no access to `useMach`, and the
 * shell is the only thing that knows what is on screen. So ⌘K records a request
 * and `chrome/CopyView.tsx` — which has the shell and owns the keystroke — does
 * the copying. One code path, two ways in.
 */
export interface CopyRequest {
  /** Bumped every time, so copying twice in a row copies twice. */
  nonce: number;
}

let request: CopyRequest | null = null;
const listeners = new Set<() => void>();

export function subscribeCopyView(listener: () => void): () => void {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

/** Referentially stable while nothing changes — `useSyncExternalStore` needs that. */
export function copyRequest(): CopyRequest | null {
  return request;
}

export function requestCopyView(): void {
  request = { nonce: (request?.nonce ?? 0) + 1 };
  for (const listener of [...listeners]) listener();
}

export function clearCopyRequest(): void {
  if (request === null) return;
  request = null;
  for (const listener of [...listeners]) listener();
}

const TITLE = "copy this view as text";

/** Words for the thing whose name nobody remembers. */
const KEYWORDS = "copy view text clipboard llm chatgpt claude paste export";

/**
 * How much this query wants the copy. `0` means "not at all".
 *
 * The bar is higher than the feedback resolver's because there is no sentence
 * form of this request: nobody types a paragraph meaning "copy". A prefix of
 * the title, or one of the words above, or `>` mode.
 */
export function copyScore(query: string): number {
  const explicit = query.startsWith(">");
  const q = (explicit ? query.slice(1) : query).trim().toLowerCase();
  if (!q) return explicit ? 500 : 0;
  if (q.length >= 2 && TITLE.startsWith(q)) return 1000;

  const words = q.split(/\s+/).filter(Boolean);
  for (const word of words) {
    if (word.length < 3) continue;
    for (const keyword of KEYWORDS.split(" ")) {
      if (keyword.startsWith(word)) return 880;
    }
  }
  return 0;
}

export const copyViewResolver: PaletteResolver = {
  id: "copy-view",
  // Beside the ordinary command layer rather than above it: this is a command,
  // not the escalation the feedback row is.
  priority: 22,
  claims: () => true,
  resolve(ctx: PaletteContext): PaletteResult[] {
    const score = copyScore(ctx.query);
    if (score <= 0) return [];
    return [
      {
        id: "command:copy-view",
        kind: "command",
        title: "Copy what's on screen as text",
        meta: "⌘⌥C",
        score,
        run: () => requestCopyView(),
      },
    ];
  },
};
