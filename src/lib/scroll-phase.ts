/**
 * The frontend's view of whether the fingers are on the trackpad.
 *
 * Rust reads `phase` and `momentumPhase` off each scroll `NSEvent` and emits
 * only the changes — see `src-tauri/src/scroll.rs`, which is where the
 * reasoning lives. This side is a two-field cache and nothing else.
 *
 * # Why a module-level value and not React state
 *
 * The reader is a `wheel` listener that runs a hundred and fifty times per
 * swipe and must answer synchronously, inside the same call that decides
 * whether to `preventDefault`. Routing that through a context or a hook would
 * put a re-render between the fact and its use, and re-rendering the calendar
 * mid-swipe is the exact thing the wheel handler goes out of its way to
 * survive. So: one value, written by a listener, read by whoever asks.
 *
 * # It is allowed to be absent
 *
 * In a browser — `bun run dev`, the fixture harness, vitest — there is no Rust
 * and no monitor, so the phase stays `null` forever and the gesture logic falls
 * back to reading the deltas alone. That fallback is worse (it cannot tell a
 * momentum tail from a second swipe without waiting for silence) and it is not
 * a fallback anything in the real app runs, which is the point of saying so
 * here rather than letting the two paths look interchangeable.
 */

/** What the hardware is doing. Mirrors `scroll::Phase` in Rust. */
export type Phase = "fingers-down" | "fingers-up" | "no-phase";

/** The event Rust emits. Mirrors `scroll::SCROLL_PHASE_EVENT`. */
export const SCROLL_PHASE_EVENT = "scroll-phase";

export interface ScrollPhasePayload {
  phase: Phase;
  /** Increments each time the fingers come back down. */
  gesture: number;
}

/**
 * The phase as a gesture reads it, or `null` when nothing is publishing one.
 *
 * `no-phase` collapses to `null` on purpose: a scroll wheel saying "I have no
 * fingers" and a monitor that never started are the same instruction to the
 * caller — do not lean on this.
 */
export interface FingerState {
  down: boolean;
  /** Which gesture this is. Changes when a hand lands. */
  gesture: number;
}

let live: FingerState | null = null;

/** The current state, for a `wheel` handler that needs an answer now. */
export function readFingers(): FingerState | null {
  return live;
}

/**
 * Apply one payload. Exported for tests; the listener below is the only other
 * caller.
 */
export function applyScrollPhase(payload: ScrollPhasePayload): void {
  live =
    payload.phase === "no-phase"
      ? null
      : { down: payload.phase === "fingers-down", gesture: payload.gesture };
}

/** Forget everything. Tests only — a module-level value outlives a test file. */
export function resetScrollPhase(): void {
  live = null;
}

/**
 * Start listening. Returns a function that stops, and resets the state with it
 * so a torn-down listener cannot leave a stale "fingers are down" behind.
 */
export async function connectScrollPhase(): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  const off = await listen<ScrollPhasePayload>(SCROLL_PHASE_EVENT, (event) =>
    applyScrollPhase(event.payload),
  );
  return () => {
    live = null;
    void off();
  };
}
