import { useCallback, useEffect, useRef, useState } from "react";
import {
  completionSupport,
  requestCompletion,
  shouldRequest,
  subscribeGhost,
  type GhostKind,
} from "@/lib/ghost";

/**
 * The keystroke side of ghost text.
 *
 * `lib/ghost.ts` holds the rules; this holds the timing, which is the part that
 * decides whether the feature feels like help or like weather. Three rules:
 *
 *  * **Nothing is asked for while you are typing.** The request goes out
 *    `delayMs` after the last keystroke, so a burst of typing costs one call
 *    rather than forty. A suggestion that arrives mid-word would be wrong by
 *    the time it rendered anyway.
 *  * **An answer for text you have moved on from is thrown away.** The value
 *    the request was made against is captured and checked when it returns.
 *  * **Not-configured is silence.** `completionSupport()` resolves false in a
 *    browser tab, without a key, or with the switch off, and every one of those
 *    produces exactly what an empty completion produces: no grey text, no
 *    banner, no console noise.
 */

export interface GhostText {
  /** The continuation to render behind the caret. `""` when there is none. */
  suggestion: string;
  /** Take it — returns the new field value, or null if there was nothing. */
  accept: () => string | null;
  /** Escape. Holds until the value changes again. */
  dismiss: () => void;
}

export interface GhostOptions {
  kind: GhostKind;
  value: string;
  /** Surrounding facts for the prompt: recipients, subject, what is quoted. */
  context?: readonly string[];
  /**
   * Whether a suggestion makes sense right now — focused, caret at the end,
   * field not read-only. The caller knows; this hook cannot.
   */
  active: boolean;
  delayMs?: number;
}

/** Long enough that ordinary typing never triggers it, short enough to feel live. */
export const GHOST_DEBOUNCE_MS = 550;

export function useGhostText({
  kind,
  value,
  context,
  active,
  delayMs = GHOST_DEBOUNCE_MS,
}: GhostOptions): GhostText {
  const [suggestion, setSuggestion] = useState("");
  const [dismissedAt, setDismissedAt] = useState<string | null>(null);
  // Bumped by the switch, so turning ghost text off clears what is on screen
  // and turning it on starts asking again without a reload.
  const [epoch, setEpoch] = useState(0);
  const latest = useRef(0);

  useEffect(() => subscribeGhost(() => setEpoch((n) => n + 1)), []);

  // The context lines are rebuilt on every render by most callers; comparing
  // them by content keeps that from restarting the timer forever.
  const contextKey = (context ?? []).join("\n");

  useEffect(() => {
    setSuggestion("");
    if (!active || dismissedAt === value) return;
    if (!shouldRequest(kind, value, true)) return;

    const ticket = (latest.current += 1);
    const timer = window.setTimeout(() => {
      void (async () => {
        const { supported } = await completionSupport();
        if (!supported || ticket !== latest.current) return;
        const text = await requestCompletion({ kind, prefix: value, context });
        if (ticket !== latest.current) return;
        setSuggestion(text);
      })();
    }, delayMs);

    return () => window.clearTimeout(timer);
    // `context` is covered by `contextKey`; including the array itself would
    // restart the timer on every parent render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind, value, contextKey, active, dismissedAt, delayMs, epoch]);

  const accept = useCallback(() => {
    if (!suggestion) return null;
    latest.current += 1;
    setSuggestion("");
    return value + suggestion;
  }, [suggestion, value]);

  const dismiss = useCallback(() => {
    latest.current += 1;
    setSuggestion("");
    setDismissedAt(value);
  }, [value]);

  return { suggestion, accept, dismiss };
}
