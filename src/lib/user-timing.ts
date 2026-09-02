/**
 * Why a development window throws away its own performance measurements.
 *
 * React's development build calls `performance.measure()` on every component
 * render, every effect and every commit — seventeen call sites in
 * `react-dom-client.development.js`, and not one matching `clearMeasures`. The
 * gate is `typeof performance.measure === "function"`, so it is on whenever the
 * engine supports user timing; it is *not* conditioned on anyone profiling, or
 * on DevTools being open. `StrictMode` doubles the render count and so doubles
 * the rate.
 *
 * Nothing ever evicts those entries. The Performance Timeline is unbounded by
 * specification: the buffer that `performance.getEntriesByType("measure")`
 * reads from grows for the lifetime of the document and is only emptied by an
 * explicit `clearMeasures`, or by a navigation. A browser tab gets that for
 * free, because it is reloaded a dozen times an hour. This window is not — Mach
 * is developed while it is being used as a mail client, and `hmr-hold.ts` exists
 * precisely so that a frontend edit does *not* reload it. The two facts compose
 * badly: the mechanism that protects the open conversation is also what lets
 * this buffer run for a working day.
 *
 * Measured on a five-hour window: 1,441,623 `WebCore::PerformanceMeasure`
 * objects and 4.2 GB of physical footprint, against a JavaScript heap of 32 KB.
 * The entries do not live in the JS heap — each one is a WebCore object holding
 * a structured clone of React's `detail` payload, on the malloc side where no
 * garbage collector and no amount of freeing JavaScript objects will reach
 * them. The window was idle at 0.1% CPU the whole time, so the memory was not
 * in use; it was retained, compressed, and eventually swapped, which is the
 * shape this took on a machine with 24 GB of RAM. Clearing the buffer returned
 * the process to 614 MB without disturbing the session.
 *
 * # Why an interval and not a smarter trigger
 *
 * There is no event for "the buffer is getting large". `PerformanceObserver`
 * reports entries as they are *added*, so an observer would run on every render
 * — the very thing being paid for — to do bookkeeping the timer does for free
 * once every thirty seconds. Clearing on idle via `requestIdleCallback` sounds
 * better and is worse: the window is idle for hours at a time, which is exactly
 * when nothing needs clearing, and busy when it does.
 *
 * Thirty seconds is chosen to be far below the hours it takes to matter and far
 * above the milliseconds a render takes, so no measurement is ever cut short
 * mid-commit. It is a fixed cost of two calls per interval.
 *
 * # What this destroys
 *
 * Every user-timing mark and measure in the window, including any left by hand
 * from the console, not only React's. That is the trade: a name-by-name sweep
 * would have to know React's naming (component measures are prefixed with a
 * zero-width space, `U+200B`) and would silently stop working when that detail
 * changed. If you are profiling, stop this first — it is a disposer, and
 * `stopTrimmingUserTiming()` is the whole interface.
 *
 * None of it reaches production. `import.meta.env.DEV` is the literal `false`
 * after Vite substitutes it, the caller's branch is dead, and this module
 * tree-shakes out — the same claim the QA bridge and the held-update toast make.
 * A production build of React never calls `performance.measure` in the first
 * place, so there would be nothing here to clear.
 */

/** How often the buffer is emptied. Far below hours, far above a commit. */
const TRIM_INTERVAL_MS = 30_000;

/**
 * Start discarding user-timing entries, and hand back the way to stop.
 *
 * Safe to call when the engine has no user timing at all: the capability is
 * checked once here rather than on every tick, and an engine without it has
 * nothing to accumulate.
 */
export function trimUserTiming(): () => void {
  if (
    typeof performance === "undefined" ||
    typeof performance.clearMeasures !== "function" ||
    typeof performance.clearMarks !== "function"
  ) {
    return () => {};
  }

  const timer = setInterval(() => {
    performance.clearMeasures();
    performance.clearMarks();
  }, TRIM_INTERVAL_MS);

  return () => clearInterval(timer);
}
