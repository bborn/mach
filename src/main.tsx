import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "@/components/chrome/ErrorBoundary";
import { fixtureSource, setDataSource } from "@/lib/data";
import { createTauriSource, isTauri } from "@/lib/ipc";
import "./styles/globals.css";

/*
 * The plugin sandbox conformance probe, which is not the app.
 *
 * `src-tauri/src/bin/plugin_probe.rs` sets this flag on a hidden window and
 * waits for the verdict. It is here rather than behind a separate entry point
 * because the whole value of the probe is that it runs the *same* code the app
 * runs — same protocol, same guest, same worker — in the same WebView.
 */
if ((globalThis as Record<string, unknown>).__MACH_PLUGIN_PROBE__) {
  void import("@/lib/plugins/probe").then((probe) => probe.runProbeAndReport());
} else {
  /*
   * The swap. Inside the Tauri window everything comes from Rust; a plain
   * `bun run dev` browser tab has no IPC to talk to, so it renders the fixtures
   * instead of throwing on the first `invoke`. No component knows the difference.
   */
  /*
   * `?fixtures` forces the invented data even inside the Tauri window.
   *
   * The published screenshots need the real application — traffic lights, the
   * overlay title bar, WebKit's own scrollbars — showing mail that belongs to
   * nobody. Before this the only way to see fixtures was a browser tab, which
   * has none of that chrome, and the only way to get a real window was the
   * owner's own mailbox, which cannot be published. So neither route could
   * produce the picture, and one of them exposed his mail to do it.
   *
   * A query parameter rather than an environment variable because the frontend
   * cannot read the process it is hosted by; `scripts/qa` puts it on the dev
   * URL. It is checked before `isTauri` so it wins, and nothing else in the app
   * knows the difference.
   */
  const fixturesRequested =
    typeof window !== "undefined" &&
    new URLSearchParams(window.location.search).has("fixtures");

  setDataSource(isTauri() && !fixturesRequested ? createTauriSource() : fixtureSource);

  /*
   * Whether the fingers are on the trackpad, which no web engine will tell us
   * and Rust reads off the NSEvent — see `scroll-phase.ts`.
   *
   * Started here and never stopped: it is one listener for the life of the
   * window, feeding a module-level value that a `wheel` handler reads
   * synchronously. Hanging it off a component would tie a fact about the
   * hardware to a mount, and the one place that reads it is a listener written
   * specifically to survive re-renders mid-swipe.
   *
   * Unlike the data source this is *not* gated on `?fixtures`. Fixtures are
   * invented mail; the trackpad is still a trackpad, and the screenshot window
   * should swipe like the real one.
   */
  if (isTauri()) {
    void import("@/lib/scroll-phase").then((phase) => phase.connectScrollPhase());
  }

  /*
   * React's development build measures every render and never clears them up.
   *
   * Unbounded, and this window is not reloaded for hours at a time — see
   * `lib/user-timing.ts` for what that costs and why a timer is the answer.
   *
   * Not gated on `isTauri`: a `bun run dev` browser tab runs the same React
   * build and accumulates the same entries. It is gated on `DEV` here as well
   * as inside the module so a production build drops the import entirely, the
   * way the QA bridge is — and a production React never calls `measure` at all.
   *
   * Started and never stopped, like the scroll-phase listener above: it is one
   * timer for the life of the window. `trimUserTiming` returns a disposer for
   * the console, if you are profiling and need it to stop.
   */
  if (import.meta.env.DEV) {
    void import("@/lib/user-timing").then((timing) => timing.trimUserTiming());
  }

  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      {/*
        Outside `App` and inside `StrictMode`, which is the only placement that
        catches everything the app can throw while still being caught itself by
        nothing — there is no boundary above this one, so it must not be part of
        the tree it is guarding.
      */}
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </React.StrictMode>,
  );
}
