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
  setDataSource(isTauri() ? createTauriSource() : fixtureSource);

  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}
