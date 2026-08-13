/**
 * Reply suggestions — the stances, the keys, and what happened to them.
 *
 * Rust writes them off the sync loop and this reads them. Everything here is a
 * local read: opening a conversation asks SQLite for a row, and pressing a
 * stance uses text that is already in this process. There is deliberately no
 * "generate" call — generation belongs to the arrival of a message, and a
 * command the webview could invoke would be a way to spend money by scrolling.
 *
 * Called directly through `invoke` rather than through `MachDataSource`, the way
 * `prefs.ts` and `compose.ts` are: this is not mail, it is not paged, it is not
 * pushed, and putting it on the data seam would mean the fixture source had to
 * implement a store it has no use for. Outside Tauri every read comes back
 * empty, which is exactly right — a browser tab against Vite has no sync loop
 * and therefore nothing to have suggested.
 */

import { isTauri } from "./ipc";
import { htmlFromPlainText, htmlToPlainText } from "./email-html";

/* -------------------------------------------------------------------------- */
/* The model                                                                   */
/* -------------------------------------------------------------------------- */

/**
 * One stance: a decision about what to do, and the reply that carries it out.
 *
 * The label is the button. Three to five words, imperative — "Say you'll be
 * there", "Ask for a raincheck". It names the *stance*, not the content, which
 * is what lets the row be read at a glance instead of read.
 */
export interface Stance {
  label: string;
  body: string;
}

export interface ReplySuggestion {
  threadId: number;
  /** The message these answer. Rust will not hand them back once it is stale. */
  messageId: number;
  model: string;
  createdAt: number;
  stances: Stance[];
}

/** What can be recorded. Mirrors `suggest::store::Outcome` in Rust. */
export type SuggestionOutcome =
  | "suggested"
  | "picked"
  | "sentAsWritten"
  | "sentEdited"
  | "dismissed";

/** Which limit stopped it. Mirrors `suggest::budget::Capped` in Rust. */
export type SuggestionCap = "hour" | "day" | "spend";

/**
 * What the agent has generated lately, against what it is allowed.
 *
 * Both windows roll — "the last hour", not "since the hour struck" — so
 * `resumesAt` is an exact instant rather than the top of something.
 */
export interface SuggestionBudget {
  hourCount: number;
  hourLimit: number;
  dayCount: number;
  dayLimit: number;
  /**
   * Dollars over the last day, or `null` when nothing reported a price.
   *
   * `null` is the normal case on a Claude subscription: the tokens are real and
   * they draw down a quota, but no invoice exists for them, so a figure here
   * would be one Rust made up. The count limits are what protect that path.
   */
  spendUsd: number | null;
  spendLimitUsd: number;
  /** Which limit is currently refusing, or `null`. */
  cappedBy: SuggestionCap | null;
  /** When the cap lifts, epoch ms, or `null` when nothing is capped. */
  resumesAt: number | null;
}

export interface SuggestionStats {
  /** Model calls made — always at least `suggested`, because a call that came
   *  back with nothing usable still spent its tokens. */
  generated: number;
  suggested: number;
  picked: number;
  sentAsWritten: number;
  sentEdited: number;
  dismissed: number;
  /** Sent roughly as written over everything suggested, or `null` before there
   *  is a denominator. */
  asWrittenRate: number | null;
  winningLabels: { label: string; count: number }[];
  budget: SuggestionBudget;
}

export const EMPTY_BUDGET: SuggestionBudget = {
  hourCount: 0,
  hourLimit: 0,
  dayCount: 0,
  dayLimit: 0,
  spendUsd: null,
  spendLimitUsd: 0,
  cappedBy: null,
  resumesAt: null,
};

export const EMPTY_STATS: SuggestionStats = {
  generated: 0,
  suggested: 0,
  picked: 0,
  sentAsWritten: 0,
  sentEdited: 0,
  dismissed: 0,
  asWrittenRate: null,
  winningLabels: [],
  budget: EMPTY_BUDGET,
};

/**
 * Below this, the feature is costing more attention than it saves.
 *
 * Not enforced anywhere — nothing switches itself off — but it is the number the
 * preferences panel colours against, because "40% of what it writes goes out
 * roughly as written" is the claim the whole thing rests on and he should be
 * able to see it fail.
 */
export const WORTH_IT_RATE = 0.4;

/* -------------------------------------------------------------------------- */
/* Transport                                                                   */
/* -------------------------------------------------------------------------- */

async function call<T>(command: string, args: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(command, args);
}

/**
 * The stances for a conversation, or `null`.
 *
 * `null` covers every reason there are none — never written, gone stale, the
 * preference off — because the row behaves the same way for all of them: it is
 * not there. A caller that distinguished them would have four states to render
 * and three of them would say nothing.
 */
export async function loadSuggestion(threadId: number): Promise<ReplySuggestion | null> {
  if (!isTauri()) return null;
  const found = await call<ReplySuggestion | null>("reply_suggestions", { threadId });
  // A row written by a newer build, or hand-edited. One bad row costs one
  // conversation's stances rather than a blank reading pane.
  if (!found || !Array.isArray(found.stances) || found.stances.length === 0) return null;
  const stances = found.stances.filter(
    (s): s is Stance =>
      typeof s?.label === "string" &&
      typeof s?.body === "string" &&
      s.label.trim() !== "" &&
      s.body.trim() !== "",
  );
  return stances.length > 0 ? { ...found, stances } : null;
}

/**
 * Note what he did. Never throws, and nothing waits on it: a counter that fails
 * is a counter, and refusing a keystroke because the statistics could not be
 * written would be the tail wagging the dog.
 */
export function recordOutcome(
  kind: SuggestionOutcome,
  detail: { stanceIndex?: number; stanceLabel?: string; threadId?: number } = {},
): void {
  if (!isTauri()) return;
  void call("reply_suggestion_record", {
    kind,
    stanceIndex: detail.stanceIndex ?? null,
    stanceLabel: detail.stanceLabel ?? null,
    threadId: detail.threadId ?? null,
  }).catch(() => {
    /* a counter is not worth a red pixel */
  });
}

export async function loadSuggestionStats(): Promise<SuggestionStats> {
  if (!isTauri()) return EMPTY_STATS;
  const stats = await call<SuggestionStats>("reply_suggestion_stats", {}).catch(() => null);
  if (!stats) return EMPTY_STATS;
  // A payload from a build that predates the budget still has a hit rate worth
  // showing; missing spend is missing, not zero.
  return { ...EMPTY_STATS, ...stats, budget: { ...EMPTY_BUDGET, ...stats.budget } };
}

/* -------------------------------------------------------------------------- */
/* The budget, as words                                                        */
/* -------------------------------------------------------------------------- */

/**
 * When the cap lifts, short enough for a field description.
 *
 * The daily window rolls, so "until 09:12" can mean tomorrow — which is exactly
 * the reading a bare time invites and exactly the wrong one. The day is named
 * whenever it is not today.
 */
export function resumeLabel(resumesAt: number, now: number = Date.now()): string {
  const at = new Date(resumesAt);
  // `numeric` rather than `2-digit`: a zero-padded "04:58 PM" reads like a
  // duration or a timestamp, and this is a time somebody glances at.
  const time = at.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const midnight = new Date(now);
  midnight.setHours(24, 0, 0, 0);
  return resumesAt < midnight.getTime() ? time : `${time} tomorrow`;
}

/**
 * The one line the panel shows when nothing is being written, and why.
 *
 * `null` when nothing is capped — there is no state to report, and reporting
 * "within limits" every time somebody opens preferences would be the software
 * talking about itself.
 */
export function capLabel(budget: SuggestionBudget, now: number = Date.now()): string | null {
  const which =
    budget.cappedBy === "hour"
      ? "Hourly limit"
      : budget.cappedBy === "day"
        ? "Daily limit"
        : budget.cappedBy === "spend"
          ? "Daily spend limit"
          : null;
  if (!which) return null;
  return budget.resumesAt === null
    ? which
    : `${which} · paused until ${resumeLabel(budget.resumesAt, now)}`;
}

/** Spend so far today, or nothing at all when no price was ever reported. */
export function spendLabel(budget: SuggestionBudget): string | null {
  return budget.spendUsd === null ? null : `$${budget.spendUsd.toFixed(2)}`;
}

/* -------------------------------------------------------------------------- */
/* Keys                                                                        */
/* -------------------------------------------------------------------------- */

/**
 * The stance keys: `1`, `2`, `3`, and `0` for "write it myself".
 *
 * # Why bare digits
 *
 * The number is already on the button, so the key needs no learning and no help
 * sheet — which is the property a row that appears and disappears actually
 * needs. Gmail has no equivalent gesture to match, so there is nothing to
 * diverge from.
 *
 * `0` for the empty composer rather than a fourth digit: it reads as "none of
 * these", it stays in the same place whether there are one stances or three, and
 * it cannot be confused with a stance that has scrolled off.
 *
 * # Why they do not collide
 *
 * Digits are bound in exactly two other places, and neither can be live at the
 * same moment as these. The calendar's `1`/`2`/`3` switch views and are gated on
 * calendar mode; the snooze picker's are at `OVERLAY_KEY_FLOOR` and exist only
 * while its overlay holds the keyboard. These are gated on mail mode with no
 * composer on screen and no overlay open. `suggestion-keys.test.ts` is what
 * keeps that true — it reads the source rather than trusting this paragraph.
 *
 * None of them is `allowInInput`, so a `1` typed into the search box or into a
 * half-written reply is a character.
 */
export const SUGGESTION_KEYS = {
  /** Stance *n* is `String(n + 1)`. */
  stance: (index: number): string => String(index + 1),
  /** The empty composer. */
  mine: "0",
} as const;

/**
 * How many stances get a key. Two buttons is a glance.
 *
 * Kept in step with `suggest::prompt::MAX_STANCES`, which is where the reason
 * lives: the row carries reply, reply-all and forward too, and three stances
 * plus that strip does not fit on one line in the window he actually uses.
 */
export const MAX_KEYED_STANCES = 2;

/* -------------------------------------------------------------------------- */
/* Promotion                                                                   */
/* -------------------------------------------------------------------------- */

/**
 * A stance's plain text as the composer's HTML.
 *
 * One block per line, escaped — the same `htmlFromPlainText` a signature goes
 * through, so a stance and a signature end up as the same kind of markup and
 * the editor has nothing to reconcile.
 */
export function stanceHtml(stance: Stance): string {
  return htmlFromPlainText(stance.body.trim());
}

/**
 * Where the caret goes when a stance opens: the end of what was written.
 *
 * The caret is carried by *character offset* (see `caret-offset.ts`), and the
 * offset is into the concatenated text — so this counts the text, not the
 * markup.
 */
export function caretAfter(stance: Stance): number {
  return stance.body.trim().length;
}

/* -------------------------------------------------------------------------- */
/* Did he send it as written?                                                  */
/* -------------------------------------------------------------------------- */

/**
 * How much of the stance survived, from 0 to 1.
 *
 * Word-level rather than character-level, and set-based rather than positional:
 * the question is "is this still the reply the model wrote", and a person who
 * moves a sentence, fixes a typo or drops a word has still sent what was
 * suggested. A person who keeps two words out of forty has not.
 *
 * Signature and quoted text are stripped from both sides first, because the
 * composer adds a signature the stance never had and a reply carries the message
 * it answers.
 */
export function retention(stanceBody: string, sentHtml: string): number {
  const suggested = words(stanceBody);
  if (suggested.length === 0) return 0;
  const sent = new Set(words(htmlToPlainText(sentHtml)));
  const kept = suggested.filter((word) => sent.has(word)).length;
  return kept / suggested.length;
}

/**
 * The line between "sent roughly as written" and "sent heavily edited".
 *
 * Eight in ten words surviving is a reply he skimmed and sent, possibly with a
 * sentence added; below that he rewrote it, and counting that as a success would
 * flatter the feature exactly where it most needs not to be flattered.
 */
export const AS_WRITTEN_RETENTION = 0.8;

/** Which outcome a send counts as. */
export function sendOutcome(stanceBody: string, sentHtml: string): SuggestionOutcome {
  return retention(stanceBody, sentHtml) >= AS_WRITTEN_RETENTION
    ? "sentAsWritten"
    : "sentEdited";
}

/** Lowercased words, punctuation dropped, quoted lines dropped. */
function words(text: string): string[] {
  return text
    .split("\n")
    .filter((line) => !line.trimStart().startsWith(">"))
    .join(" ")
    .toLowerCase()
    .split(/[^\p{L}\p{N}']+/u)
    .filter((word) => word.length > 0);
}
