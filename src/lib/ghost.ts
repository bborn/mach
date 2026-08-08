/**
 * Ghost text — the grey continuation sitting under the caret.
 *
 * The rule this whole module exists to enforce is that a completion is an
 * *offer*: it renders behind the field, it costs ⇥ to take, and it disappears
 * the moment you keep typing. Nothing here ever changes a draft on its own.
 *
 * Four pieces, in the order they run:
 *
 *  1. **`shouldRequest`** — the cheap "is this even worth asking" gate. It runs
 *     on every keystroke and it is deliberately conservative: a request that is
 *     not going to be accepted still costs latency, tokens, and a flicker of
 *     grey text in the corner of somebody's eye.
 *  2. **`buildPrompt`** — one system prompt per kind of field, the surrounding
 *     facts as context lines, and the text before the caret as the thing to
 *     continue. No history, no tools, no session.
 *  3. **`cleanCompletion`** — the model's answer is never trusted verbatim.
 *     Models like to re-state the prefix, wrap answers in quotes, apologise, and
 *     write four paragraphs when one clause was wanted. All four are handled
 *     here rather than in the prompt, because a prompt is a request and this is
 *     a guarantee.
 *  4. **the IPC**, which is one round trip to `agent_complete` and no session.
 *
 * # Graceful fallback
 *
 * `completionSupport()` answers "can this work at all" *before* anything is
 * sent, and it answers false — quietly — in a browser tab, without
 * `ANTHROPIC_API_KEY`, or with ghost text switched off. Every caller treats
 * "not supported" as "there is no suggestion", which is exactly what the field
 * looks like when a completion simply came back empty. There is no error state
 * for a feature nobody asked for out loud.
 */

import { isTauri, tauriTransport } from "./ipc";

/* -------------------------------------------------------------------------- */
/* What can be completed                                                       */
/* -------------------------------------------------------------------------- */

export type GhostKind = "emailBody" | "emailSubject" | "eventTitle" | "eventDescription";

export interface GhostRequest {
  kind: GhostKind;
  /** Everything typed before the caret. */
  prefix: string;
  /** Surrounding facts, one per line: recipients, subject, what is being answered. */
  context?: readonly string[];
}

interface KindRules {
  /** Below this many characters there is nothing to continue. */
  minPrefix: number;
  /** Hard ceiling on the accepted continuation, in characters. */
  maxCompletion: number;
  /** A field that cannot hold a newline. */
  singleLine: boolean;
  system: string;
}

const RULES: Record<GhostKind, KindRules> = {
  emailBody: {
    minPrefix: 10,
    maxCompletion: 240,
    singleLine: false,
    system:
      "You are an inline autocomplete inside an email composer. " +
      "Continue the message from exactly where it stops, in the writer's voice. " +
      "At most one sentence — you are finishing a thought, not writing the email.",
  },
  emailSubject: {
    minPrefix: 3,
    maxCompletion: 60,
    singleLine: true,
    system:
      "You are an inline autocomplete for an email subject line. " +
      "Continue the subject from exactly where it stops. " +
      "A few words at most, no final punctuation.",
  },
  eventTitle: {
    minPrefix: 3,
    maxCompletion: 60,
    singleLine: true,
    system:
      "You are an inline autocomplete for a calendar event title. " +
      "Continue the title from exactly where it stops. A few words at most.",
  },
  eventDescription: {
    minPrefix: 10,
    maxCompletion: 200,
    singleLine: false,
    system:
      "You are an inline autocomplete inside a calendar event's notes. " +
      "Continue from exactly where the text stops. At most one sentence.",
  },
};

/** Shared by every kind, and the part that actually keeps the output usable. */
const COMMON_RULES =
  "Reply with the continuation only: no preamble, no quotes, no markdown fences, " +
  "and never repeat any of the text that is already written. " +
  "If the text ends mid-word, finish that word first. " +
  "If there is nothing sensible to add, reply with nothing at all.";

/* -------------------------------------------------------------------------- */
/* The gate                                                                    */
/* -------------------------------------------------------------------------- */

/**
 * Whether this keystroke is worth a round trip.
 *
 * Completing anywhere but the end of the text is a different feature — it means
 * editing what is already there, and grey text after the caret would be a lie
 * about where it lands. So the caret has to be at the end, and there has to be
 * enough written to continue.
 */
export function shouldRequest(kind: GhostKind, prefix: string, caretAtEnd: boolean): boolean {
  if (!caretAtEnd) return false;
  const rules = RULES[kind];
  if (prefix.trim().length < rules.minPrefix) return false;
  // A blank line at the end is a paragraph break the writer just made. They are
  // about to start a new thought; guessing it is the most annoying moment to
  // guess.
  if (/\n\s*\n\s*$/.test(prefix)) return false;
  return true;
}

/* -------------------------------------------------------------------------- */
/* The prompt                                                                  */
/* -------------------------------------------------------------------------- */

export interface Prompt {
  system: string;
  prompt: string;
  maxTokens: number;
}

/** How much of the prefix to send. The tail is what a continuation follows. */
const PREFIX_WINDOW = 2000;

export function buildPrompt(request: GhostRequest): Prompt {
  const rules = RULES[request.kind];
  const context = (request.context ?? []).map((line) => line.trim()).filter(Boolean);

  const prefix =
    request.prefix.length > PREFIX_WINDOW
      ? `…${request.prefix.slice(-PREFIX_WINDOW)}`
      : request.prefix;

  const parts: string[] = [];
  if (context.length > 0) parts.push(`Context:\n${context.join("\n")}`);
  parts.push(`Text so far (continue from the very end, mid-word if it stops mid-word):\n${prefix}`);

  return {
    system: `${rules.system} ${COMMON_RULES}`,
    prompt: parts.join("\n\n"),
    // Characters, not tokens — a generous ceiling that still cannot run away.
    maxTokens: Math.ceil(rules.maxCompletion / 2) + 32,
  };
}

/* -------------------------------------------------------------------------- */
/* Cleaning the answer                                                         */
/* -------------------------------------------------------------------------- */

/** Openers that mean the model answered the question instead of continuing. */
const REFUSALS = /^(i (can|cannot|can't|am|'m)\b|sure[,!]|here('s| is)\b|as an ai\b)/i;

/**
 * The model's text, made safe to render behind a caret.
 *
 * The one subtle case is the overlap: asked to continue "Thanks for the", a
 * model will quite often reply "Thanks for the update" — helpfully repeating
 * what it was given. Rendering that verbatim as ghost text produces "Thanks for
 * theThanks for the update", so the longest suffix of the prefix that the
 * answer starts with is removed before anything else is decided.
 */
export function cleanCompletion(kind: GhostKind, prefix: string, raw: string): string {
  const rules = RULES[kind];
  let text = raw.replace(/\r\n/g, "\n");

  // Fences and wrapping quotes are formatting the field cannot use.
  text = text.replace(/^```[a-z]*\n?/i, "").replace(/\n?```$/, "");
  if (text.length > 1 && text.startsWith('"') && text.endsWith('"')) {
    text = text.slice(1, -1);
  }

  text = stripOverlap(prefix, text);
  if (!text.trim()) return "";
  if (REFUSALS.test(text.trimStart())) return "";

  if (rules.singleLine) {
    const firstLine = text.split("\n")[0] ?? "";
    text = firstLine;
  } else {
    // Trailing blank lines would show as ghost whitespace and accept as junk.
    text = text.replace(/\s+$/, (match) => (match.includes("\n") ? "" : match));
  }

  // A continuation of a word must not start with a space, and a continuation of
  // a sentence must. The model is asked for both and gets it wrong sometimes;
  // only the first case can be fixed without guessing.
  const lastChar = prefix.slice(-1);
  if (lastChar && /\s/.test(lastChar)) text = text.replace(/^[ \t]+/, "");

  if (text.length > rules.maxCompletion) {
    text = truncateAtBoundary(text, rules.maxCompletion);
  }

  return text.trim() === "" ? "" : text;
}

/** Longest suffix of `prefix` that `text` opens with, removed from `text`. */
function stripOverlap(prefix: string, text: string): string {
  const window = Math.min(prefix.length, 200);
  for (let length = window; length > 0; length -= 1) {
    const tail = prefix.slice(prefix.length - length);
    if (text.startsWith(tail)) return text.slice(length);
    if (text.toLowerCase().startsWith(tail.toLowerCase())) return text.slice(length);
  }
  return text;
}

/** Cut at the last space before the limit, so a suggestion never ends mid-word. */
function truncateAtBoundary(text: string, limit: number): string {
  const cut = text.slice(0, limit);
  const space = cut.lastIndexOf(" ");
  return space > limit / 2 ? cut.slice(0, space) : cut;
}

/* -------------------------------------------------------------------------- */
/* The switch                                                                  */
/* -------------------------------------------------------------------------- */

export const GHOST_STORAGE_KEY = "mach.ghost.v1";

/**
 * Ghost text sends what you are writing to a model. That is a thing a person is
 * entitled to say no to, and saying no has to be possible without unsetting the
 * key the agent also uses — hence a switch of its own, on by default, remembered
 * the same way favorites are.
 */
export function ghostEnabled(): boolean {
  try {
    return localStorage.getItem(GHOST_STORAGE_KEY) !== "off";
  } catch {
    return true;
  }
}

export function setGhostEnabled(enabled: boolean): void {
  try {
    localStorage.setItem(GHOST_STORAGE_KEY, enabled ? "on" : "off");
  } catch {
    /* a disabled localStorage leaves the default in place */
  }
  support = null;
  for (const listener of [...switchListeners]) listener();
}

const switchListeners = new Set<() => void>();

export function subscribeGhost(listener: () => void): () => void {
  switchListeners.add(listener);
  return () => void switchListeners.delete(listener);
}

/* -------------------------------------------------------------------------- */
/* IPC                                                                         */
/* -------------------------------------------------------------------------- */

export interface CompletionSupport {
  supported: boolean;
  /** Why not, when not — shown nowhere by default, but useful in the palette. */
  reason?: string;
}

let support: Promise<CompletionSupport> | null = null;

/**
 * Whether a completion can be asked for right now. Cached, because the answer
 * is a launch-time fact and this is consulted on every debounce tick.
 *
 * Invalidated by the switch above, and by nothing else: adding the key to the
 * environment already costs a relaunch.
 */
export function completionSupport(): Promise<CompletionSupport> {
  if (!ghostEnabled()) {
    return Promise.resolve({ supported: false, reason: "Ghost completions are switched off." });
  }
  if (!isTauri()) {
    return Promise.resolve({
      supported: false,
      reason: "Ghost completions need the desktop app — this is a browser tab.",
    });
  }
  support ??= tauriTransport
    .invoke<{ configured: boolean; message?: string | null }>("agent_status", {})
    .then((status) => ({
      supported: status.configured,
      reason: status.message ?? undefined,
    }))
    .catch(() => ({ supported: false, reason: "The agent could not be reached." }));
  return support;
}

/** Test seam, and what `setGhostEnabled` uses to make the switch take effect. */
export function resetCompletionSupport(): void {
  support = null;
}

/**
 * One completion. Resolves to `""` for every reason a completion might not
 * happen — no key, no desktop, a model that declined, a transport that failed.
 * Ghost text has no error state by design.
 */
export async function requestCompletion(request: GhostRequest): Promise<string> {
  const { supported } = await completionSupport();
  if (!supported) return "";

  const { system, prompt, maxTokens } = buildPrompt(request);
  try {
    const result = await tauriTransport.invoke<{ text: string }>("agent_complete", {
      system,
      prompt,
      maxTokens,
    });
    return cleanCompletion(request.kind, request.prefix, result.text ?? "");
  } catch {
    return "";
  }
}
