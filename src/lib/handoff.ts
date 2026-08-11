/**
 * Handing work out of Mach — the non-visual half.
 *
 * ⌘K, type the sentence, pick where it goes. That is the whole interaction, and
 * this file is the part of it that renders nothing:
 *
 *   1. the palette layer — a resolver, so `CommandPalette` keeps knowing nothing
 *      about handoffs and `resolver.ts` keeps not importing this file;
 *   2. the five IPC calls;
 *   3. two small stores — the target list, and which dialog is open.
 *
 * # Mach does not follow what it throws
 *
 * There is deliberately no session, no status, no list of past handoffs and
 * nothing to come back to. A handoff is his sentence leaving the building. The
 * only thing written back is `lastRunAt`, and only because a target that has
 * never run gets one confirmation before it does — see `HandoffDialog`.
 *
 * # Why the target list is cached here rather than in a hook
 *
 * A palette resolver is a plain function called on every keystroke. It cannot
 * await, and it cannot hold React state. So the list is loaded once, kept in a
 * module-level snapshot, and refreshed whenever the editor writes. The same
 * two-line store pattern `feedback.ts` uses, for the same reason.
 */

import type { PaletteContext, PaletteResolver, PaletteResult } from "./palette/resolver";
import { isTauri, tauriTransport, toMachError } from "./ipc";

/* -------------------------------------------------------------------------- */
/* Shapes                                                                      */
/* -------------------------------------------------------------------------- */

export type HandoffMode = "terminal" | "inline" | "session";

export interface HandoffTarget {
  id: string;
  name: string;
  /** Where the command runs. `~` is expanded on the Rust side. */
  dir: string;
  /** The command line, with `{{placeholders}}`. */
  run: string;
  mode: HandoffMode;
  /** When this target last launched something. `null` means never. */
  lastRunAt?: number | null;
}

/** What was on screen. Row ids only — Rust turns them into text. */
export interface HandoffSourceRef {
  kind: "mail" | "event" | "none";
  threadId?: number;
  eventId?: number;
}

/** The real plan, rendered before it runs. */
export interface HandoffPreview {
  targetId: string;
  targetName: string;
  mode: HandoffMode;
  dir: string;
  command: string;
  argv: string[];
  prompt: string;
  contextLabel: string;
  contextFile: string;
  unproven: boolean;
}

/** A session that is running in the pane. One tab. */
export interface HandoffSession {
  sessionId: string;
  targetName: string;
  /** argv, joined for reading. Nothing runs this string. */
  command: string;
  dir: string;
  /** What the process was started with, exactly. Stays on screen. */
  prompt: string;
  contextFile: string;
  /**
   * What this session was given of Mach itself — `["Mach's tools"]`, or
   * nothing. A session that can read and send mail and one that cannot are not
   * the same thing to have open, so the tab says which.
   */
  resources: string[];
}

/** One thing the running session said. */
export type HandoffSessionEvent =
  | { type: "output"; sessionId: string; base64: string; dropped: number }
  | { type: "exited"; sessionId: string; status: number | null };

/** What a launch is willing to say afterwards. Never an outcome. */
export interface HandoffReceipt {
  targetName: string;
  mode: HandoffMode;
  dir: string;
  command: string;
  contextFile: string;
  message: string;
  status: number | null;
  stdout: string;
  stderr: string;
}

/** One terminal application that is installed on this Mac. */
export interface InstalledTerminal {
  /** What `open -a` is given, and what the menu says. */
  name: string;
  /** The bundle it was found at. */
  path: string;
}

/** What the editor's terminal menu is built from. */
export interface Terminals {
  installed: InstalledTerminal[];
  /** `MACH_HANDOFF_TERMINAL_APP`, when it is set. It wins over the setting. */
  forced: string | null;
}

/** Nothing installed, nothing forced. What the browser and a failed call get. */
export const NO_TERMINALS: Terminals = { installed: [], forced: null };

/**
 * The two menu entries that are not applications.
 *
 * Sentinels rather than values, because the stored preference is free text:
 * `""` means the system default and anything unrecognised means he typed a
 * name of his own, and neither of those is a row the menu can hold as itself.
 */
export const SYSTEM_TERMINAL = "system";
export const OTHER_TERMINAL = "other";

/**
 * Which menu row a stored value selects.
 *
 * `""` is the system default. A value naming something that was detected
 * selects that row. Anything else is a name he typed — an application in a
 * place macOS does not look, or one that is no longer there — and it selects
 * "Other", where the text is still on screen and still editable.
 */
export function terminalSelection(stored: string, installed: readonly InstalledTerminal[]): string {
  const value = stored.trim();
  if (!value) return SYSTEM_TERMINAL;
  return installed.some((terminal) => terminal.name === value) ? value : OTHER_TERMINAL;
}

/** The menu, in the order it reads: the system's, what is installed, then Other. */
export function terminalItems(
  installed: readonly InstalledTerminal[],
): { value: string; label: string }[] {
  return [
    { value: SYSTEM_TERMINAL, label: "System default" },
    ...installed.map((terminal) => ({ value: terminal.name, label: terminal.name })),
    { value: OTHER_TERMINAL, label: "Other…" },
  ];
}

/**
 * What picking a menu row stores.
 *
 * "Other" stores nothing new: it opens a text field, and what he types there is
 * the next write. Keeping the current value rather than clearing it means
 * picking it while `iTerm` is selected leaves `iTerm` in the box to be edited
 * into a path, instead of an empty field and a handoff that has silently gone
 * back to the system default.
 *
 * Which is why "Other" cannot be inferred from the value alone — `iTerm` is a
 * detected name whichever way it was arrived at — and the editor holds it as
 * its own state. {@link terminalSelection} is only the answer on first render.
 */
export function terminalFromSelection(selection: string, stored: string): string {
  if (selection === SYSTEM_TERMINAL) return "";
  if (selection === OTHER_TERMINAL) return stored.trim();
  return selection;
}

/** Every `{{name}}` a `run` template understands. Shown under the field. */
export const PLACEHOLDERS = [
  "prompt",
  "note",
  "subject",
  "from",
  "date",
  "body",
  "permalink",
  "attachments",
  "context_file",
] as const;

export const SEED_RUN = 'claude "{{prompt}}"';

/* -------------------------------------------------------------------------- */
/* The target list                                                             */
/* -------------------------------------------------------------------------- */

let targets: HandoffTarget[] = [];
const targetListeners = new Set<() => void>();

/** Referentially stable while nothing changes — `useSyncExternalStore` needs that. */
export function targetSnapshot(): HandoffTarget[] {
  return targets;
}

export function subscribeTargets(listener: () => void): () => void {
  targetListeners.add(listener);
  return () => void targetListeners.delete(listener);
}

/**
 * The write side of the snapshot.
 *
 * Exported because the two IPC calls are not the only legitimate writers: the
 * tests drive the palette layer through it, and nothing about the store cares
 * where a list came from.
 */
export function setTargets(next: HandoffTarget[]): HandoffTarget[] {
  targets = next;
  for (const listener of [...targetListeners]) listener();
  return next;
}

/* -------------------------------------------------------------------------- */
/* Open / closed                                                               */
/* -------------------------------------------------------------------------- */

/**
 * What the dialog should be doing.
 *
 * `run` is the common case and usually renders nothing at all: a target he has
 * used before launches straight away. `edit` is the target editor. `nonce` is
 * bumped on every open so re-running the same target re-fires.
 */
export type HandoffRequest =
  | { kind: "run"; targetId: string; note: string; nonce: number }
  | { kind: "edit"; nonce: number };

let request: HandoffRequest | null = null;
const listeners = new Set<() => void>();
let nonce = 0;

function emit() {
  for (const listener of [...listeners]) listener();
}

export function subscribeHandoff(listener: () => void): () => void {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

export function handoffRequest(): HandoffRequest | null {
  return request;
}

export function openHandoff(targetId: string, note: string): void {
  request = { kind: "run", targetId, note: note.trim(), nonce: ++nonce };
  emit();
}

export function openHandoffTargets(): void {
  request = { kind: "edit", nonce: ++nonce };
  emit();
}

export function closeHandoff(): void {
  if (request === null) return;
  request = null;
  emit();
}

/* -------------------------------------------------------------------------- */
/* ⌘K                                                                          */
/* -------------------------------------------------------------------------- */

/**
 * Words that mean "take this somewhere else".
 *
 * Short on purpose. The main way in is not a keyword at all — it is having
 * typed a sentence, because a sentence in the search box was never a search.
 */
const TRIGGERS = ["hand off", "handoff", "hand to", "send to", "ship to"];

/** A sentence is an instruction; a word or two is a search. */
export const SENTENCE_WORDS = 4;

/** Never more than this many target rows, however many are configured. */
const MAX_ROWS = 5;

export function stripPrefix(query: string): string {
  return (query.startsWith(">") ? query.slice(1) : query).trim();
}

/**
 * Whether the query is the keyword and nothing else — including the part of it
 * that is on screen while it is still being typed.
 *
 * A prefix counts, and that is the whole point. {@link handoffScore} puts the
 * target rows on top from the first letter, because that is what makes ⌘K worth
 * having: type until the row you want appears, then ⏎. So ⏎ regularly arrives
 * mid-word, and the query at that instant is `hando` — five characters of the
 * word he was typing to *find* the row, not a sentence about the mail.
 */
export function isKeywordOnly(query: string): boolean {
  const q = stripPrefix(query).toLowerCase();
  if (!q) return true;
  return TRIGGERS.some((trigger) => trigger.startsWith(q) || trigger === q);
}

/**
 * His instruction, as typed. Empty when the query is only the word that found
 * the row.
 *
 * The empty case is a refusal, not a default: `LaunchPlan::prepare` says "say
 * what you want done" and the dialog shows it. That is the correct end for a
 * handoff with no sentence in it — the alternative, which shipped, was handing
 * a whole mail thread to an agent with tools under the instruction `hando`, and
 * the agent on the other end had to stop and ask what that meant.
 */
export function noteFromQuery(query: string): string {
  return isKeywordOnly(query) ? "" : stripPrefix(query);
}

/**
 * How much this query wants a handoff. `0` means "not at all".
 *
 * Below the feedback layer's trigger words on purpose: "the calendar header is
 * broken" is about Mach and belongs in the feedback loop, while "implement this
 * feature request from Katie" is about somewhere else. Both rows appear for an
 * ambiguous sentence; the scores decide which is on top.
 */
export function handoffScore(query: string): number {
  const explicit = query.startsWith(">");
  const q = stripPrefix(query).toLowerCase();

  if (!q) return explicit ? 500 : 0;

  for (const trigger of TRIGGERS) {
    if (trigger.startsWith(q) || q.startsWith(trigger)) return 950;
  }

  return q.split(/\s+/).filter(Boolean).length >= SENTENCE_WORDS ? 320 : 0;
}

/**
 * Targets, with the one he named first.
 *
 * "reschedule the standups, offerlab" should not make him arrow past three
 * other repositories. A name mentioned anywhere in the sentence wins; the rest
 * keep the order he arranged them in.
 */
export function rankTargets(query: string, list: readonly HandoffTarget[]): HandoffTarget[] {
  const q = stripPrefix(query).toLowerCase();
  const named = (target: HandoffTarget) => {
    const name = target.name.trim().toLowerCase();
    return name.length >= 3 && q.includes(name);
  };
  return [...list].sort((a, b) => Number(named(b)) - Number(named(a)));
}

/** "claude · terminal" — enough to tell two targets apart at a glance. */
export function describeTarget(target: HandoffTarget): string {
  const program = target.run.trim().split(/\s+/)[0] ?? "";
  return `${program} · ${target.mode}`;
}

export const handoffResolver: PaletteResolver = {
  id: "handoff",
  // Below the feedback layer, above ordinary commands: a sentence is more often
  // work to send somewhere than it is a command name typed badly.
  priority: 25,
  claims: () => true,
  resolve(ctx: PaletteContext): PaletteResult[] {
    const results: PaletteResult[] = [];
    const score = handoffScore(ctx.query);
    const note = noteFromQuery(ctx.query);
    const list = targetSnapshot();

    if (score > 0) {
      if (list.length === 0) {
        results.push({
          id: "command:handoff-setup",
          kind: "command",
          title: "Set up a handoff target…",
          meta: "pick a directory",
          score,
          run: () => openHandoffTargets(),
        });
      } else {
        for (const [index, target] of rankTargets(ctx.query, list).slice(0, MAX_ROWS).entries()) {
          results.push({
            id: `command:handoff:${target.id}`,
            kind: "command",
            title: `Hand off to ${target.name}`,
            // The row is up before there is anything to send, so it says what
            // is missing rather than describing a target he cannot use yet.
            meta: note ? describeTarget(target) : "type what you want done",
            // One point apart so the ranking above survives the sort.
            score: score - index,
            run: () => openHandoff(target.id, note),
          });
        }
      }
    }

    // The editor, reachable by name whether or not anything is configured.
    const editScore = editorScore(ctx.query);
    if (editScore > 0) {
      results.push({
        id: "command:handoff-targets",
        kind: "command",
        title: "Handoff targets…",
        meta: list.length === 1 ? "1 target" : `${list.length} targets`,
        score: editScore,
        run: () => openHandoffTargets(),
      });
    }

    return results;
  },
};

const EDITOR_TITLE = "handoff targets";

function editorScore(query: string): number {
  const explicit = query.startsWith(">");
  const q = stripPrefix(query).toLowerCase();
  if (!q) return explicit ? 480 : 0;
  if (q.length >= 3 && EDITOR_TITLE.startsWith(q)) return 1000;
  return 0;
}

/* -------------------------------------------------------------------------- */
/* IPC                                                                         */
/* -------------------------------------------------------------------------- */

const NEEDS_DESKTOP = "Handoff needs the desktop app — this is a browser tab.";

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) throw toMachError(NEEDS_DESKTOP);
  try {
    return await tauriTransport.invoke<T>(command, args);
  } catch (error) {
    throw toMachError(error);
  }
}

/** Load the list and publish it, so the resolver sees it on the next keystroke. */
export async function loadTargets(): Promise<HandoffTarget[]> {
  return setTargets(await call<HandoffTarget[]>("handoff_targets"));
}

/** Replace the list. Comes back normalized — trimmed, with ids filled in. */
export async function saveTargets(next: HandoffTarget[]): Promise<HandoffTarget[]> {
  return setTargets(await call<HandoffTarget[]>("handoff_save_targets", { targets: next }));
}

/**
 * The terminals this Mac has, and any environment override.
 *
 * Never rejects: a browser tab has no applications to find, and an editor that
 * refused to render because it could not enumerate them would be worse than one
 * offering the system default and a text field.
 */
export async function loadTerminals(): Promise<Terminals> {
  try {
    const answer = await call<Terminals>("handoff_terminals");
    return {
      installed: Array.isArray(answer?.installed) ? answer.installed : [],
      forced: answer?.forced ?? null,
    };
  } catch {
    return NO_TERMINALS;
  }
}

/** The system folder panel. `null` when he cancelled. */
export async function pickDirectory(): Promise<string | null> {
  return (await call<string | null>("handoff_pick_directory")) ?? null;
}

export async function previewHandoff(input: {
  targetId: string;
  note: string;
  source: HandoffSourceRef;
}): Promise<HandoffPreview> {
  return call<HandoffPreview>("handoff_preview", {
    targetId: input.targetId,
    note: input.note,
    source: input.source,
  });
}

export async function runHandoff(input: {
  targetId: string;
  note: string;
  source: HandoffSourceRef;
}): Promise<HandoffReceipt> {
  const receipt = await call<HandoffReceipt>("handoff_run", {
    targetId: input.targetId,
    note: input.note,
    source: input.source,
  });
  // `lastRunAt` decided whether the confirmation was shown, and it has just
  // changed on the Rust side. Refresh rather than guess.
  void loadTargets().catch(() => undefined);
  return receipt;
}

/* -------------------------------------------------------------------------- */
/* The session pane                                                            */
/* -------------------------------------------------------------------------- */

/** The one channel a running session speaks on. Mirrors `ipc::handoff`. */
export const HANDOFF_SESSION_EVENT = "handoff-session";

/**
 * The tabs, and which one is in front.
 *
 * A module store rather than React state for the same reason the target list is
 * one: the thing that starts a session is the handoff dialog and the thing that
 * renders it is the pane, and neither is inside the other.
 *
 * Rust caps the list; this side never refuses one, it just holds what came
 * back. Both arrays are replaced rather than mutated, because
 * `useSyncExternalStore` compares snapshots by identity and a mutated array is
 * a render that never happens.
 */
let sessions: HandoffSession[] = [];
let activeId: string | null = null;
const sessionListeners = new Set<() => void>();

export function sessionsSnapshot(): HandoffSession[] {
  return sessions;
}

/** The tab in front, or `null` when the pane is empty. */
export function activeSessionId(): string | null {
  return activeId;
}

export function subscribeSession(listener: () => void): () => void {
  sessionListeners.add(listener);
  return () => void sessionListeners.delete(listener);
}

function emitSessions() {
  for (const listener of [...sessionListeners]) listener();
}

/**
 * Replace the list, keeping the front tab in front where it still exists.
 *
 * `active` names one explicitly — a session that has just started puts itself
 * in front. Otherwise the current one stays selected if it survived, and the
 * last tab takes over if it did not, which is what a browser does.
 */
export function setSessions(next: HandoffSession[], active?: string | null): void {
  sessions = next;
  const wanted = active === undefined ? activeId : active;
  activeId =
    (wanted && next.some((s) => s.sessionId === wanted) ? wanted : null) ??
    next[next.length - 1]?.sessionId ??
    null;
  emitSessions();
}

/** Bring one tab to the front. A id that is not a tab is ignored. */
export function selectSession(id: string): void {
  if (id === activeId || !sessions.some((s) => s.sessionId === id)) return;
  activeId = id;
  emitSessions();
}

/**
 * The next tab along, wrapping.
 *
 * Wrapping rather than stopping at the ends because there are at most four of
 * them: ⌥⌘→ four times should come back to where it started rather than leave
 * you wondering whether the key is broken.
 */
export function stepSession(delta: number): void {
  if (sessions.length < 2) return;
  const at = sessions.findIndex((s) => s.sessionId === activeId);
  const next = (((at < 0 ? 0 : at) + delta) % sessions.length + sessions.length) % sessions.length;
  selectSession(sessions[next].sessionId);
}

/**
 * Everything the session said before anything was ready to draw it.
 *
 * Three things have to happen before the first byte can be painted: the Tauri
 * listener has to attach, the pane has to mount, and 300 kB of emulator has to
 * arrive over a dynamic import. The process does not wait for any of them — a
 * command that prints its banner and exits can be finished before the second
 * one — and the first time this ran in the real app, the whole of the banner
 * was gone. So the stream is attached *before* the process is started and
 * everything it says is kept here until a consumer takes it.
 */
let sessionQueue: HandoffSessionEvent[] = [];
const sessionConsumers = new Map<string, (event: HandoffSessionEvent) => void>();
let sessionStream: Promise<() => void> | null = null;

/**
 * How many events may wait for a tab that has not mounted yet.
 *
 * Rust caps a chunk at a share of one frame and sends at most one per frame per
 * session, so an ordinary wait — a mount and one dynamic import — is a handful
 * of these. The number is here for the tab that never arrives: without it, a
 * process talking to a pane that failed to render would grow this array
 * forever.
 */
const MAX_QUEUED_EVENTS = 256;

/**
 * Attach the one listener, once.
 *
 * Idempotent and never detached: there is one channel for the life of the
 * window, whatever is open on it. Every event carries its `sessionId`, which is
 * how one channel serves four tabs.
 */
function ensureSessionStream(): Promise<() => void> {
  sessionStream ??= listenToSession((event) => {
    const consumer = sessionConsumers.get(event.sessionId);
    if (consumer) {
      consumer(event);
      return;
    }
    sessionQueue.push(event);
    if (sessionQueue.length > MAX_QUEUED_EVENTS) sessionQueue.shift();
  });
  return sessionStream;
}

/**
 * Take one session's stream, and everything it has been holding.
 *
 * A tab calls this when its emulator exists. Whatever arrived for *that*
 * session in the meantime is delivered first, in order, before anything live —
 * a command that prints a banner and exits can be finished before its tab has
 * mounted, and the first time this ran the whole banner was gone.
 */
export function consumeSession(
  sessionId: string,
  handler: (event: HandoffSessionEvent) => void,
): () => void {
  const waiting = sessionQueue.filter((event) => event.sessionId === sessionId);
  sessionQueue = sessionQueue.filter((event) => event.sessionId !== sessionId);
  sessionConsumers.set(sessionId, handler);
  for (const event of waiting) handler(event);
  return () => {
    if (sessionConsumers.get(sessionId) === handler) sessionConsumers.delete(sessionId);
  };
}

/** Start the target's command on a pty and give the pane a tab for it. */
export async function openSession(input: {
  targetId: string;
  note: string;
  source: HandoffSourceRef;
  cols: number;
  rows: number;
}): Promise<HandoffSession> {
  // Before the process, not after it. See `sessionQueue`.
  await ensureSessionStream();
  const started = await call<HandoffSession>("handoff_session_open", {
    targetId: input.targetId,
    note: input.note,
    source: input.source,
    cols: input.cols,
    rows: input.rows,
  });
  // A new tab goes in front, which is the only thing that could be meant by
  // starting one.
  setSessions([...sessions, started], started.sessionId);
  // `lastRunAt` moved on the Rust side; the palette's rows read it.
  void loadTargets().catch(() => undefined);
  return started;
}

/**
 * Adopt the sessions that are already running.
 *
 * A webview that reloaded — hot module replacement, a renderer that crashed —
 * comes back with an empty store while the processes are still on their ptys.
 * Asked once, on mount, exactly as the agent dock asks for its sessions. What
 * was printed before the reload is gone with the emulators that held it.
 */
export async function adoptSessions(): Promise<HandoffSession[]> {
  if (!isTauri()) return [];
  try {
    await ensureSessionStream();
    const running = await call<HandoffSession[]>("handoff_sessions");
    if (Array.isArray(running) && running.length > 0) setSessions(running);
    return running ?? [];
  } catch {
    return [];
  }
}

export async function writeSession(sessionId: string, data: string): Promise<void> {
  await call<void>("handoff_session_write", { sessionId, data });
}

export async function resizeSession(
  sessionId: string,
  cols: number,
  rows: number,
): Promise<void> {
  await call<void>("handoff_session_resize", { sessionId, cols, rows });
}

/**
 * End one tab, and forget it. The others carry on.
 *
 * Never rejects. Closing a session whose process has already exited is the
 * ordinary case — the tab stays up after the exit so the last lines can be
 * read — and a pane that refused to close because the thing it was closing was
 * already gone would be worse than useless.
 */
export async function closeSession(sessionId: string): Promise<void> {
  const next = sessions.filter((s) => s.sessionId !== sessionId);
  if (next.length !== sessions.length) {
    // Closing the front tab hands the front to its left-hand neighbour, or to
    // whatever is left when it was the first.
    const at = sessions.findIndex((s) => s.sessionId === sessionId);
    const heir = sessionId === activeId ? next[Math.max(0, at - 1)]?.sessionId ?? null : activeId;
    setSessions(next, heir);
  }
  try {
    await call<void>("handoff_session_close", { sessionId });
  } catch {
    /* nothing to close */
  }
}

async function listenToSession(
  handler: (event: HandoffSessionEvent) => void,
): Promise<() => void> {
  if (!isTauri()) return () => undefined;
  return tauriTransport.listen<HandoffSessionEvent>(HANDOFF_SESSION_EVENT, handler);
}

/**
 * The wire's base64 back to the bytes the emulator wants.
 *
 * Output crosses as base64 because a pty carries bytes: escape sequences, and
 * multi-byte characters that a chunk boundary can land in the middle of.
 * Decoding to a string on this side would corrupt exactly those, so the bytes
 * stay bytes until the emulator — the one thing that knows where a character
 * ends — takes them.
 */
export function decodeChunk(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/* -------------------------------------------------------------------------- */
/* Editing                                                                     */
/* -------------------------------------------------------------------------- */

/** A blank row for the editor. Not saved until he fills it in. */
export function draftTarget(dir = ""): HandoffTarget {
  return {
    id: "",
    name: dir ? nameFromDir(dir) : "",
    dir,
    run: SEED_RUN,
    mode: "terminal",
    lastRunAt: null,
  };
}

/** `~/Projects/offerlab` → `offerlab`. Mirrors `target::name_from_dir`. */
export function nameFromDir(dir: string): string {
  const parts = dir.trim().replace(/\/+$/, "").split("/").filter(Boolean);
  const last = parts[parts.length - 1];
  return last && last !== "~" ? last : "Handoff";
}

/**
 * What is wrong with this row, as a sentence, or `null`.
 *
 * The same three refusals Rust makes, checked while he types so the field that
 * is wrong is the one under the cursor. Rust still checks: this is an
 * affordance, not the boundary.
 */
export function targetProblem(target: HandoffTarget): string | null {
  if (!target.name.trim()) return "Name it";
  if (!target.dir.trim()) return "Give it a directory";
  const run = target.run.trim();
  if (!run) return "Give it a command";
  if (unbalanced(run)) return "Unclosed quote";
  const program = run.split(/\s+/)[0] ?? "";
  if (program.includes("=")) return "A command can't start with an assignment";
  if (program.includes("{{")) return "The program can't be a placeholder";
  return null;
}

/**
 * One pass, mirroring `handoff::template::tokenize`.
 *
 * Tracking a single "which quote am I inside" rather than counting each kind
 * separately is what stops `claude "don't"` being reported as broken — the
 * apostrophe is inside double quotes, where it is a character rather than a
 * quote.
 */
export function unbalanced(run: string): boolean {
  let inside: '"' | "'" | null = null;
  for (let i = 0; i < run.length; i += 1) {
    const char = run[i];
    if (inside === "'") {
      if (char === "'") inside = null;
      continue;
    }
    if (inside === '"') {
      if (char === "\\") i += 1;
      else if (char === '"') inside = null;
      continue;
    }
    if (char === "\\") i += 1;
    else if (char === '"' || char === "'") inside = char;
  }
  return inside !== null;
}
