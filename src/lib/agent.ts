/**
 * Agent sessions — the non-visual half.
 *
 * Five things live here, and none of them renders anything:
 *
 *   1. **the ⌘K entry point** — a palette resolver, so the palette component
 *      keeps knowing nothing about the agent (`resolver.ts` only puts it in the
 *      chain), exactly as `feedback.ts` does;
 *   2. **the ask store** — a two-line external store the dock subscribes to, so
 *      a resolver (a plain function, no hooks) can open a session;
 *   3. **context extraction** — a pure function from what the shell is showing
 *      to the lines the session attaches. This is the thing that makes "reply
 *      to this" mean anything, and it is pure so it can be tested without a
 *      window;
 *   4. **the session state machine** — a reducer over the events Rust pushes.
 *      Pure, so running → awaiting-approval → done is testable without a model;
 *   5. **the IPC calls**.
 *
 * The wire shapes mirror `src-tauri/src/agent/session.rs`. A rename on either
 * side breaks exactly one file.
 */

import type { CalendarEvent, Thread, ThreadDetail } from "@/types";
import type { PaletteContext, PaletteResolver, PaletteResult } from "./palette/resolver";
import { isTauri, tauriTransport, toMachError } from "./ipc";

/* -------------------------------------------------------------------------- */
/* Wire shapes                                                                 */
/* -------------------------------------------------------------------------- */

export const AGENT_SESSION_EVENT = "agent-session";

export type SessionStatus = "running" | "awaitingApproval" | "done" | "failed";
export type ToolState = "running" | "ok" | "error" | "denied";

/** One thing the owner was looking at when they asked. */
export interface ContextItem {
  id: string;
  kind: "thread" | "event" | "day" | "search" | "mailbox" | "selection";
  /** What the removable line says. */
  label: string;
  threadId?: number;
  eventId?: number;
  detail?: string;
}

export type Entry =
  | { role: "user"; text: string }
  | { role: "agent"; text: string }
  | { role: "tool"; id: string; name: string; summary: string; state: ToolState };

export interface PendingApproval {
  toolUseId: string;
  name: string;
  summary: string;
  input: Record<string, unknown>;
}

export interface AgentSession {
  id: string;
  title: string;
  status: SessionStatus;
  createdAt: number;
  context: ContextItem[];
  entries: Entry[];
  pending?: PendingApproval;
  error?: string;
  /** Which brain answered — "Claude Code", "Anthropic API (claude-opus-5)". */
  backend?: string;
  /** Tokens arriving right now, not yet part of an entry. Client-side only. */
  streaming?: string;
}

export type AgentEvent =
  | { type: "created"; sessionId: string; session: AgentSession }
  | { type: "status"; sessionId: string; status: SessionStatus }
  | { type: "delta"; sessionId: string; text: string }
  | { type: "entry"; sessionId: string; entry: Entry }
  | { type: "approval"; sessionId: string; pending: PendingApproval }
  | { type: "context"; sessionId: string; context: ContextItem[] }
  | { type: "failed"; sessionId: string; message: string }
  | { type: "closed"; sessionId: string };

/* -------------------------------------------------------------------------- */
/* The state machine                                                           */
/* -------------------------------------------------------------------------- */

/**
 * Apply one event to the list of sessions.
 *
 * Pure and total: an event for a session we have never seen is ignored rather
 * than inventing a half-populated row, because `agent_sessions` is the way to
 * recover from a missed `created` — not guesswork.
 *
 * Three rules carry the whole machine:
 *
 * - a `delta` accumulates into `streaming`, which the next `entry` clears —
 *   the completed agent line is authoritative over the tokens that built it;
 * - a tool `entry` replaces the entry with the same id, so "Searching…"
 *   becomes "7 matches" in place rather than stacking up;
 * - `approval` sets `pending` *and* the status, because a session waiting on a
 *   human is a state, not a modal.
 */
export function reduceSessions(sessions: AgentSession[], event: AgentEvent): AgentSession[] {
  if (event.type === "created") {
    const without = sessions.filter((s) => s.id !== event.sessionId);
    return [...without, { ...event.session, entries: [...event.session.entries] }];
  }
  if (event.type === "closed") {
    return sessions.filter((s) => s.id !== event.sessionId);
  }
  return sessions.map((session) =>
    session.id === event.sessionId ? reduceSession(session, event) : session,
  );
}

export function reduceSession(session: AgentSession, event: AgentEvent): AgentSession {
  switch (event.type) {
    case "status":
      return {
        ...session,
        status: event.status,
        // Leaving the approval state must clear the prompt, whichever way it
        // was answered.
        pending: event.status === "awaitingApproval" ? session.pending : undefined,
      };

    case "delta":
      return { ...session, streaming: (session.streaming ?? "") + event.text };

    case "entry":
      return {
        ...session,
        streaming: event.entry.role === "agent" ? undefined : session.streaming,
        entries: upsertEntry(session.entries, event.entry),
      };

    case "approval":
      return { ...session, status: "awaitingApproval", pending: event.pending };

    case "context":
      return { ...session, context: event.context };

    case "failed":
      return { ...session, status: "failed", error: event.message, pending: undefined };

    default:
      return session;
  }
}

function upsertEntry(entries: Entry[], entry: Entry): Entry[] {
  if (entry.role !== "tool") return [...entries, entry];
  const index = entries.findIndex((e) => e.role === "tool" && e.id === entry.id);
  if (index < 0) return [...entries, entry];
  const next = [...entries];
  next[index] = entry;
  return next;
}

/** Whether a session is still doing something, for the pill's spinner. */
export function isBusy(session: AgentSession): boolean {
  return session.status === "running";
}

/** Sessions that need the owner before they can continue. */
export function needsAttention(sessions: readonly AgentSession[]): AgentSession[] {
  return sessions.filter((s) => s.status === "awaitingApproval" || s.status === "failed");
}

/* -------------------------------------------------------------------------- */
/* Context extraction                                                          */
/* -------------------------------------------------------------------------- */

/** What the shell is showing. Deliberately a plain value, not the hook. */
export interface ShellView {
  mode: "mail" | "calendar";
  calendarView: "day" | "week" | "month";
  /** The day the calendar is anchored on. */
  anchor: number;
  labelId: string;
  mailboxName?: string;
  threadId: number | null;
  eventId: number | null;
  /** The thread under the cursor when none is open. */
  selectedThread?: Thread | null;
  openThread?: ThreadDetail | Thread | null;
  selectedEvent?: CalendarEvent | null;
  /** The search the list is filtered by, when there is one. */
  search?: string;
}

/**
 * What "this" means, right now.
 *
 * Ordered by how specific it is: an open conversation beats the mailbox it is
 * in, and a selected event beats the day it sits on. At most a handful of
 * lines — the point is "the thing I am looking at", not a dump of the app's
 * state.
 *
 * The label is what the owner reads on the removable line; the ids are what
 * Rust resolves against the store. They are separate on purpose, so a stale
 * label can never make the agent act on the wrong thread.
 */
export function contextFor(view: ShellView): ContextItem[] {
  const items: ContextItem[] = [];

  if (view.mode === "mail") {
    const thread = view.threadId != null ? view.openThread : view.selectedThread;
    const subject = threadSubject(thread);
    if (view.threadId != null && subject) {
      items.push({
        id: `thread:${view.threadId}`,
        kind: "thread",
        label: subject,
        threadId: view.threadId,
      });
    } else if (view.selectedThread) {
      items.push({
        id: `thread:${view.selectedThread.id}`,
        kind: "thread",
        label: view.selectedThread.subject,
        threadId: view.selectedThread.id,
      });
    }

    if (view.search?.trim()) {
      items.push({
        id: "search",
        kind: "search",
        label: `Search: ${view.search.trim()}`,
        detail: `The mail list is filtered by the search "${view.search.trim()}".`,
      });
    } else if (items.length === 0 && view.labelId) {
      // Nothing selected: the mailbox itself is the context, so "archive
      // everything from linkedin" still knows where it is standing.
      items.push({
        id: `mailbox:${view.labelId}`,
        kind: "mailbox",
        label: view.mailboxName ?? view.labelId,
        detail: `Viewing the mailbox ${view.mailboxName ?? view.labelId} (labelId ${view.labelId}).`,
      });
    }
    return items;
  }

  if (view.eventId != null && view.selectedEvent) {
    items.push({
      id: `event:${view.eventId}`,
      kind: "event",
      label: view.selectedEvent.title,
      eventId: view.eventId,
    });
  }

  const range = calendarRange(view);
  items.push({
    id: "day",
    kind: "day",
    label: rangeLabel(view),
    detail:
      `The calendar is showing the ${view.calendarView} of ${rangeLabel(view)} — ` +
      `unix milliseconds ${range.start} to ${range.end}.`,
  });
  return items;
}

function threadSubject(thread: ThreadDetail | Thread | null | undefined): string | undefined {
  if (!thread) return undefined;
  return "thread" in thread ? thread.thread.subject : thread.subject;
}

/** The window the calendar is showing, in epoch milliseconds. */
export function calendarRange(view: ShellView): { start: number; end: number } {
  const anchor = new Date(view.anchor);
  const startOfDay = new Date(anchor.getFullYear(), anchor.getMonth(), anchor.getDate()).getTime();
  const DAY = 86_400_000;
  if (view.calendarView === "day") return { start: startOfDay, end: startOfDay + DAY };
  if (view.calendarView === "week") {
    const start = startOfDay - anchor.getDay() * DAY;
    return { start, end: start + 7 * DAY };
  }
  const start = new Date(anchor.getFullYear(), anchor.getMonth(), 1).getTime();
  const end = new Date(anchor.getFullYear(), anchor.getMonth() + 1, 1).getTime();
  return { start, end };
}

function rangeLabel(view: ShellView): string {
  const anchor = new Date(view.anchor);
  if (view.calendarView === "month") {
    return anchor.toLocaleDateString(undefined, { month: "long", year: "numeric" });
  }
  return anchor.toLocaleDateString(undefined, {
    weekday: "long",
    day: "numeric",
    month: "long",
  });
}

/* -------------------------------------------------------------------------- */
/* ⌘K                                                                          */
/* -------------------------------------------------------------------------- */

/**
 * A sentence, not a search.
 *
 * Two words is a search for mail; "reply to this next tues" is not. The bar is
 * deliberately low because the row is only ever an *offer* — the round trip
 * still costs an explicit keystroke, so an unwanted offer costs nothing but a
 * line on screen.
 */
const SENTENCE_WORDS = 3;

export function looksLikeAQuestion(query: string): boolean {
  const q = query.trim();
  if (!q || q.startsWith(">")) return false;
  if (q.endsWith("?")) return true;
  return q.split(/\s+/).filter(Boolean).length >= SENTENCE_WORDS;
}

/**
 * The last query the palette resolved.
 *
 * The palette owns its input as local state, and ⇥ has to hand *that* string
 * to the agent. The resolver sees every keystroke, so recording it here is how
 * the keybinding — which lives in the dock, not the palette — knows what was
 * typed without either component importing the other.
 */
let lastQuery = "";

export function paletteQuery(): string {
  return lastQuery;
}

export const agentResolver: PaletteResolver = {
  id: "agent",
  // Below commands and mailboxes: a local match is instant and free, and this
  // one costs a model round trip. It is an offer, not a default.
  priority: 5,
  claims: (query) => !query.startsWith(">"),
  resolve(ctx: PaletteContext): PaletteResult[] {
    lastQuery = ctx.query;
    if (!looksLikeAQuestion(ctx.query)) return [];
    const prompt = ctx.query.trim();
    return [
      {
        id: "agent:ask",
        kind: "agent",
        title: prompt,
        subtitle: "Ask the agent about what you are looking at",
        meta: "⇥",
        score: 1,
        run: () => requestAsk(prompt),
      },
    ];
  },
};

/* -------------------------------------------------------------------------- */
/* The ask store                                                               */
/* -------------------------------------------------------------------------- */

export interface AskRequest {
  prompt: string;
  /** Bumped on every ask so the same sentence twice still opens twice. */
  nonce: number;
}

let ask: AskRequest | null = null;
const listeners = new Set<() => void>();

function emit() {
  for (const listener of [...listeners]) listener();
}

export function subscribeAsk(listener: () => void): () => void {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

/** Referentially stable while nothing changes — `useSyncExternalStore` needs that. */
export function askRequest(): AskRequest | null {
  return ask;
}

/** Hand a sentence to the agent. The dock does the rest. */
export function requestAsk(prompt: string): void {
  const trimmed = prompt.trim();
  if (!trimmed) return;
  ask = { prompt: trimmed, nonce: (ask?.nonce ?? 0) + 1 };
  emit();
}

export function clearAsk(): void {
  if (ask === null) return;
  ask = null;
  emit();
}

/* -------------------------------------------------------------------------- */
/* IPC                                                                         */
/* -------------------------------------------------------------------------- */

const NEEDS_DESKTOP = "The agent needs the desktop app — this is a browser tab.";

export async function startSession(
  prompt: string,
  context: ContextItem[],
): Promise<AgentSession> {
  if (!isTauri()) throw toMachError(NEEDS_DESKTOP);
  try {
    return await tauriTransport.invoke<AgentSession>("agent_start", { prompt, context });
  } catch (error) {
    throw toMachError(error);
  }
}

export async function listSessions(): Promise<AgentSession[]> {
  if (!isTauri()) return [];
  try {
    return await tauriTransport.invoke<AgentSession[]>("agent_sessions", {});
  } catch {
    // A dock that cannot list is an empty dock, not a broken app.
    return [];
  }
}

async function send(args: Record<string, unknown>): Promise<void> {
  if (!isTauri()) throw toMachError(NEEDS_DESKTOP);
  try {
    await tauriTransport.invoke("agent_send", args);
  } catch (error) {
    throw toMachError(error);
  }
}

export function sendMessage(sessionId: string, message: string): Promise<void> {
  return send({ sessionId, action: "message", message });
}

export function approve(sessionId: string, toolUseId: string): Promise<void> {
  return send({ sessionId, action: "approve", toolUseId });
}

export function deny(sessionId: string, toolUseId: string, reason?: string): Promise<void> {
  return send({ sessionId, action: "deny", toolUseId, reason: reason ?? null });
}

export function closeSession(sessionId: string): Promise<void> {
  return send({ sessionId, action: "close" });
}

export function removeContext(sessionId: string, itemId: string): Promise<void> {
  return send({ sessionId, action: "removeContext", itemId });
}

/* -------------------------------------------------------------------------- */
/* Which brain                                                                 */
/* -------------------------------------------------------------------------- */

/**
 * What a ⌘K would use right now, and what else this machine offers.
 *
 * Read by the preferences dialog so that what it reports and what actually runs
 * cannot drift: both answers come from the same resolution in Rust. `message`
 * is non-null exactly when nothing can run, and it is a sentence naming the
 * remedy — the whole point being that "not configured" should tell you what to
 * install, not just what is missing.
 */
export interface AgentBackendStatus {
  backend: "claudeCli" | "anthropicApi" | "command" | null;
  label: string | null;
  claudePath: string | null;
  apiKey: boolean;
  message: string | null;
}

export const UNKNOWN_BACKEND: AgentBackendStatus = {
  backend: null,
  label: null,
  claudePath: null,
  apiKey: false,
  message: null,
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseBackendStatus(raw: unknown): AgentBackendStatus {
  if (!isRecord(raw)) return UNKNOWN_BACKEND;
  const backend = raw.backend;
  return {
    backend:
      backend === "claudeCli" || backend === "anthropicApi" || backend === "command"
        ? backend
        : null,
    label: typeof raw.label === "string" ? raw.label : null,
    claudePath: typeof raw.claudePath === "string" ? raw.claudePath : null,
    apiKey: raw.apiKey === true,
    message: typeof raw.message === "string" ? raw.message : null,
  };
}

/** Never rejects: a browser tab, or a command this build does not register, is
 * "nothing detected" rather than an error in a settings surface. */
export async function loadBackendStatus(): Promise<AgentBackendStatus> {
  if (!isTauri()) return UNKNOWN_BACKEND;
  try {
    return parseBackendStatus(await tauriTransport.invoke<unknown>("agent_backend_status", {}));
  } catch {
    return UNKNOWN_BACKEND;
  }
}

/** Subscribe to the one push channel every session speaks on. */
export async function listenToSessions(
  handler: (event: AgentEvent) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  return tauriTransport.listen<AgentEvent>(AGENT_SESSION_EVENT, handler);
}
