/**
 * ⌘K resolution — the seam, plus the local layer.
 *
 * The design calls for one box resolved in layers, where only the last layer
 * touches the network:
 *
 *   layer 0  local ranked results (threads, events, people, commands)   ← here
 *   layer 1  operator queries — `from:tawny has:attachment`             ← later unit
 *   layer 2  `>` command mode                                          ← here (trivial half)
 *   layer 3  ⇥ hands the sentence to the agent                         ← `lib/agent.ts`
 *
 * A resolver is a pure function from a query to results, plus a `claims`
 * predicate saying whether it wants this query at all. `resolve()` walks the
 * chain in priority order and concatenates whatever claims the input. To add
 * the operator layer, write a resolver and `registerResolver(it)` — no changes
 * to the palette component.
 */

import type { CalendarEvent, EventId, LabelId, Participant, Thread, ThreadId } from "@/types";
import type { MailboxTarget } from "@/lib/mailboxes";
import { feedbackResolver } from "@/lib/feedback";
import { agentResolver } from "@/lib/agent";

export type PaletteResultKind =
  | "thread"
  | "event"
  | "person"
  | "mailbox"
  | "command"
  | "agent";

export interface PaletteResult {
  id: string;
  kind: PaletteResultKind;
  title: string;
  subtitle?: string;
  /** Right-aligned metadata: a time, a shortcut, an account name. */
  meta?: string;
  /** 1..5 to draw the account colour chip. */
  colorIndex?: number;
  run: () => void;
  /** Higher sorts first inside its kind. */
  score?: number;
}

export interface PaletteContext {
  query: string;
  threads: readonly Thread[];
  events: readonly CalendarEvent[];
  people: readonly Participant[];
  /** Every mailbox and label, named unambiguously. The rail only shows a few. */
  mailboxes: readonly MailboxTarget[];
  commands: readonly PaletteCommand[];
  /** Selection side effects the palette hands back to the shell. */
  actions: {
    openThread: (id: ThreadId) => void;
    openEvent: (id: EventId) => void;
    openMailbox: (id: LabelId) => void;
    runCommand: (id: string) => void;
    composeTo: (email: string) => void;
  };
}

export interface PaletteCommand {
  id: string;
  title: string;
  hint?: string;
  keywords?: string;
}

export interface PaletteResolver {
  id: string;
  /** Higher runs first. */
  priority: number;
  claims: (query: string) => boolean;
  resolve: (ctx: PaletteContext) => PaletteResult[];
}

/* -------------------------------------------------------------------------- */
/* Ranking                                                                     */
/* -------------------------------------------------------------------------- */

/**
 * Subsequence match with a bias towards prefixes and word starts. Deliberately
 * small: local ranking has to feel instant on every keystroke, and FTS5 does
 * the real work once the Rust side is wired up.
 */
export function fuzzyScore(haystack: string, needle: string): number {
  if (!needle) return 1;
  const hay = haystack.toLowerCase();
  const pin = needle.toLowerCase();

  const direct = hay.indexOf(pin);
  if (direct === 0) return 1000;
  if (direct > 0) return 700 - Math.min(direct, 200) + (hay[direct - 1] === " " ? 100 : 0);

  let score = 0;
  let cursor = 0;
  for (const char of pin) {
    const found = hay.indexOf(char, cursor);
    if (found === -1) return 0;
    score += found === 0 || hay[found - 1] === " " ? 8 : 2;
    cursor = found + 1;
  }
  return score;
}

/* -------------------------------------------------------------------------- */
/* Layer 0 — local                                                             */
/* -------------------------------------------------------------------------- */

const LOCAL_LIMIT = 6;

export const localResolver: PaletteResolver = {
  id: "local",
  priority: 10,
  claims: (query) => !query.startsWith(">"),
  resolve(ctx) {
    const q = ctx.query.trim();
    if (!q) return [];

    const threads = rank(
      ctx.threads.map((t) => ({
        value: t,
        score: Math.max(
          fuzzyScore(t.subject, q),
          fuzzyScore(t.participants.map((p) => p.name).join(" "), q) * 0.9,
          fuzzyScore(t.snippet, q) * 0.4,
        ),
      })),
    ).map<PaletteResult>(({ value, score }) => ({
      id: `thread:${value.id}`,
      kind: "thread",
      title: value.subject,
      subtitle: value.participants[0]?.name,
      score,
      run: () => ctx.actions.openThread(value.id),
    }));

    const events = rank(
      ctx.events.map((e) => ({ value: e, score: fuzzyScore(e.title, q) })),
    ).map<PaletteResult>(({ value, score }) => ({
      id: `event:${value.id}`,
      kind: "event",
      title: value.title,
      subtitle: value.location,
      score,
      run: () => ctx.actions.openEvent(value.id),
    }));

    const people = rank(
      ctx.people.map((p) => ({
        value: p,
        score: Math.max(fuzzyScore(p.name, q), fuzzyScore(p.email, q)),
      })),
    ).map<PaletteResult>(({ value, score }) => ({
      id: `person:${value.email}`,
      kind: "person",
      title: value.name,
      subtitle: value.email,
      score,
      run: () => ctx.actions.composeTo(value.email),
    }));

    return [...threads, ...events, ...people];
  },
};

/* -------------------------------------------------------------------------- */
/* Layer 0 — mailboxes and labels                                              */
/* -------------------------------------------------------------------------- */

/**
 * Labels live here rather than in the rail.
 *
 * There can be hundreds of them and they are all one keystroke away by name,
 * which is a better deal than a scrollable list of every folder you have ever
 * made. Ranked above threads: typing "receipts" when a label is called that
 * almost always means "take me there".
 */
export const mailboxResolver: PaletteResolver = {
  id: "mailbox",
  priority: 15,
  claims: (query) => !query.startsWith(">"),
  resolve(ctx) {
    const q = ctx.query.trim();
    if (!q) return [];

    return rank(
      ctx.mailboxes.map((mailbox) => ({ value: mailbox, score: fuzzyScore(mailbox.name, q) })),
    ).map<PaletteResult>(({ value, score }) => ({
      id: `mailbox:${value.accountId ?? "all"}:${value.id}`,
      kind: "mailbox",
      title: value.name,
      meta: value.kind === "user" ? "label" : "mailbox",
      score,
      run: () => ctx.actions.openMailbox(value.id),
    }));
  },
};

/* -------------------------------------------------------------------------- */
/* Layer 2 — `>` commands                                                      */
/* -------------------------------------------------------------------------- */

/**
 * Commands render above everything else, so an implicit match has to be a
 * confident one. `fuzzyScore` will happily scatter "boomer" across the letters
 * of "bookmark conversation" and put a command on top of the four Boomerang
 * labels the user was obviously reaching for. In `>` mode a scattered match is
 * welcome — the user said they wanted commands. Outside it, only a real prefix
 * or substring hit earns the top of the list.
 */
const IMPLICIT_COMMAND_FLOOR = 500;

export const commandResolver: PaletteResolver = {
  id: "command",
  priority: 20,
  claims: () => true,
  resolve(ctx) {
    const explicit = ctx.query.startsWith(">");
    const q = (explicit ? ctx.query.slice(1) : ctx.query).trim();
    if (!explicit && !q) return [];

    const scored = ctx.commands
      .map((c) => ({
        value: c,
        score:
          explicit && !q
            ? 1
            : Math.max(fuzzyScore(c.title, q), fuzzyScore(c.keywords ?? "", q) * 0.8),
      }))
      .filter((c) => explicit || c.score >= IMPLICIT_COMMAND_FLOOR);

    return rank(scored, explicit ? 20 : 4).map<PaletteResult>(({ value, score }) => ({
      id: `command:${value.id}`,
      kind: "command",
      title: value.title,
      meta: value.hint,
      score,
      run: () => ctx.actions.runCommand(value.id),
    }));
  },
};

function rank<T>(items: { value: T; score: number }[], limit = LOCAL_LIMIT) {
  return items
    .filter((i) => i.score > 0)
    .sort((a, b) => b.score - a.score)
    .slice(0, limit);
}

/* -------------------------------------------------------------------------- */
/* The chain                                                                   */
/* -------------------------------------------------------------------------- */

// `feedback.ts` and `agent.ts` import only *types* from this module, so
// registering their resolvers here costs no runtime cycle — and the palette
// component still knows nothing about either.
//
// `agentResolver` sits last on purpose. Ordinary typing must stay instant and
// local, so the layer that costs a model round trip ranks below every layer
// that does not, and it only ever offers — the handoff still needs ⇥ or ⏎.
const resolvers: PaletteResolver[] = [
  feedbackResolver,
  commandResolver,
  mailboxResolver,
  localResolver,
  agentResolver,
];

/** Register a later layer — operators, agent handoff, server-side search. */
export function registerResolver(resolver: PaletteResolver): () => void {
  resolvers.push(resolver);
  return () => {
    const i = resolvers.indexOf(resolver);
    if (i >= 0) resolvers.splice(i, 1);
  };
}

export function resolve(ctx: PaletteContext): PaletteResult[] {
  return [...resolvers]
    .sort((a, b) => b.priority - a.priority)
    .filter((r) => r.claims(ctx.query))
    .flatMap((r) => r.resolve(ctx));
}

export const KIND_LABELS: Record<PaletteResultKind, string> = {
  command: "Commands",
  mailbox: "Mailboxes and labels",
  thread: "Mail",
  event: "Calendar",
  person: "People",
  agent: "Ask",
};

/** Render order for the grouped result list. */
export const KIND_ORDER: PaletteResultKind[] = [
  "command",
  "mailbox",
  "thread",
  "event",
  "person",
  "agent",
];
