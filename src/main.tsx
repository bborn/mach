import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { fixtureSource, setDataSource } from "@/lib/data";
import { createTauriSource, isTauri } from "@/lib/ipc";
import "./styles/globals.css";

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
