import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
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

  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}
