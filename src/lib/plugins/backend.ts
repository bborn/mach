/**
 * The plugin half of the IPC seam.
 *
 * Separate from `src/lib/ipc.ts` on purpose: `MachDataSource` is the mail and
 * calendar contract and it is already large, while this is a handful of calls
 * that only exist inside a Tauri window. Outside one — a browser tab on the
 * fixture data source — [`nullBackend`] answers "no plugins, no sandbox", which
 * is the truth: there is no `plugin://` protocol to serve a guest from.
 */

import type { IpcTransport } from "@/lib/ipc";
import { toMachError } from "@/lib/ipc";
import type { ConformanceReport, ConsentLine, InstalledPlugin, PluginManifest } from "./types";

/** The event the Rust bridge asks the webview for work on. */
export const PLUGIN_INVOKE_EVENT = "plugin-invoke";

export interface SandboxAssets {
  workerSource: string;
  canarySource: string;
  csp: string;
  sandbox: string;
  verified: boolean;
  safeMode: boolean;
  platformLimits: [string, string][];
}

export interface InstallCandidate {
  manifest: PluginManifest;
  consent: ConsentLine[];
  addedCapabilities: string[];
  alreadyInstalled: boolean;
  install: "development" | "published";
}

/** One agent tool call, on its way to a plugin action. */
export interface InvokeRequest {
  requestId: number;
  pluginId: string;
  action: string;
  input: Record<string, unknown>;
  source: string;
}

export interface PluginBackend {
  readonly available: boolean;
  sandboxAssets(): Promise<SandboxAssets>;
  reportConformance(report: ConformanceReport): Promise<void>;
  list(): Promise<InstalledPlugin[]>;
  inspect(path: string, dev?: boolean): Promise<InstallCandidate>;
  install(path: string, dev?: boolean): Promise<InstalledPlugin>;
  remove(id: string): Promise<void>;
  setEnabled(id: string, enabled: boolean): Promise<void>;
  source(id: string): Promise<string>;
  onInvoke(handler: (request: InvokeRequest) => void): Promise<() => void>;
  answerInvoke(requestId: number, ok: boolean, value: unknown, error?: string): Promise<void>;
}

export function createPluginBackend(transport: IpcTransport): PluginBackend {
  async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    try {
      return await transport.invoke<T>(command, args);
    } catch (error) {
      throw toMachError(error);
    }
  }

  return {
    available: true,
    sandboxAssets: () => call<SandboxAssets>("plugin_sandbox"),
    reportConformance: (report) => call<void>("plugin_conformance", { report }),
    list: () => call<InstalledPlugin[]>("plugin_list"),
    inspect: (path, dev) => call<InstallCandidate>("plugin_inspect", { path, dev: dev ?? false }),
    install: (path, dev) => call<InstalledPlugin>("plugin_install", { path, dev: dev ?? false }),
    remove: (id) => call<void>("plugin_remove", { id }),
    setEnabled: (id, enabled) => call<void>("plugin_set_enabled", { id, enabled }),
    source: (id) => call<string>("plugin_source", { id }),
    onInvoke: (handler) => transport.listen<InvokeRequest>(PLUGIN_INVOKE_EVENT, handler),
    answerInvoke: (requestId, ok, value, error) =>
      call<void>("plugin_invoke_result", { requestId, ok, value, error }),
  };
}

/** Outside a Tauri window there is no protocol, so there are no plugins. */
export const nullBackend: PluginBackend = {
  available: false,
  async sandboxAssets() {
    throw new Error("plugins need the desktop app");
  },
  async reportConformance() {},
  async list() {
    return [];
  },
  async inspect() {
    throw new Error("plugins need the desktop app");
  },
  async install() {
    throw new Error("plugins need the desktop app");
  },
  async remove() {},
  async setEnabled() {},
  async source() {
    throw new Error("plugins need the desktop app");
  },
  async onInvoke() {
    return () => {};
  },
  async answerInvoke() {},
};
