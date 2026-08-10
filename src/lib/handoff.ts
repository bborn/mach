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

export type HandoffMode = "terminal" | "inline";

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
 * His instruction, as typed. Empty when the query was a search rather than a
 * sentence — seeding a handoff with the word "handoff" would be one more thing
 * to delete.
 */
export function noteFromQuery(query: string): string {
  const q = stripPrefix(query);
  const words = q.split(/\s+/).filter(Boolean);
  if (words.length < SENTENCE_WORDS) return "";
  return q;
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
            meta: describeTarget(target),
            // One point apart so the ranking above survives the sort.
            score: score - index,
            run: () => openHandoff(target.id, note || stripPrefix(ctx.query)),
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
