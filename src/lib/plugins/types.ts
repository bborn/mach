/**
 * The plugin DTO family — deliberately narrower than the app's own models.
 *
 * `docs/plugins.md` §6 names four things as public API and this file is two of
 * them: the manifest, and the view vocabulary. Everything a plugin sees is
 * mapped into these shapes in exactly one place (`api.ts`), which is the whole
 * cost of being able to refactor `Thread`, `ThreadDetail` and the SQLite schema
 * freely. `src/lib/ipc.ts` already does this once between Rust rows and UI
 * types; this is the same move, one layer out.
 *
 * Rust is authoritative for the manifest: `src-tauri/src/plugins/manifest.rs`
 * parses and validates it, and the frontend receives the parsed value. When the
 * two disagree, this file is wrong.
 */

export type PluginId = string;

export interface PluginManifest {
  id: PluginId;
  name: string;
  version: string;
  machApi: string;
  description: string;
  author: string;
  homepage?: string | null;
  main: string;
  machApiProposed: string[];
  runtime: "sandbox" | "process";
  networkAccess?: { allowedDomains: string[]; reasoning?: string | null } | null;
  capabilities: PluginCapabilities;
  contributes: PluginContributes;
}

export interface PluginCapabilities {
  read: string[];
  commands: string[];
  ui: string[];
  events: string[];
  store: boolean;
  /**
   * Opt-*out*. Absent or `true` means every contributed action is an agent
   * tool; `false` means none; a list names the subset. Owner's decision,
   * 2026-08-08 — and the reason attribution and policy inheritance are
   * mandatory rather than nice to have.
   */
  agent: boolean | string[];
}

export interface PluginContributes {
  actions: PluginAction[];
  views: PluginView[];
}

export interface PluginAction {
  id: string;
  title: string;
  keywords?: string | null;
  /** In the app's own binding syntax: "alt+f", "shift+h", "g i". */
  key?: string | null;
  context: "threads" | "global" | "calendar" | "reading-pane";
  summary?: string | null;
  params: PluginActionParam[];
}

export interface PluginActionParam {
  name: string;
  type: "string" | "number" | "boolean";
  required: boolean;
  description: string;
}

export interface PluginView {
  id: string;
  surface: string;
}

/** Why a plugin is not running, when it is not. Mirrors `store::PluginStatus`. */
export type PluginStatus =
  | { state: "ready" }
  | { state: "disabled" }
  | { state: "safeMode" }
  | { state: "invalid"; detail: string }
  | { state: "changedWithoutVersionBump" }
  | { state: "needsReapproval"; detail: string[] };

export interface InstalledPlugin {
  id: PluginId;
  manifest: PluginManifest;
  status: PluginStatus;
  directory: string;
}

export interface ConsentLine {
  text: string;
  severity: "note" | "warning" | "danger";
}

/* -------------------------------------------------------------------------- */
/* The view vocabulary                                                         */
/* -------------------------------------------------------------------------- */

export type Tone = "default" | "muted" | "warning" | "danger";

/**
 * Eight node types. No layout control, no colours, no CSS, no arbitrary text
 * sizes: a plugin describes meaning and the host decides what it looks like.
 * This is the single biggest reason the app can be restyled without a flag day
 * for plugins — and the reason a plugin cannot draw a fake account-login box.
 */
export type ViewNode =
  | { type: "section"; title?: string; children: ViewNode[] }
  | { type: "text"; value: string; tone?: Tone }
  | { type: "row"; label: string; value: string; tone?: Tone }
  | { type: "badge"; value: string; tone?: Tone }
  | { type: "button"; label: string; action: string; params?: Record<string, unknown> }
  | {
      type: "list";
      items: { title: string; subtitle?: string; action?: string; params?: Record<string, unknown> }[];
    }
  | { type: "separator" }
  | { type: "spinner"; label?: string };

/* -------------------------------------------------------------------------- */
/* The channel                                                                 */
/* -------------------------------------------------------------------------- */

/** Host → guest. */
export type HostMessage =
  | { t: "boot"; workerSource: string }
  | { t: "load"; id: number; source: string; clockOffset?: number }
  | { t: "invoke"; id: number; kind: string; name: string; ctx: Record<string, unknown> }
  | { t: "reply"; id: number; ok: boolean; value?: unknown; error?: string }
  | { t: "guest-probe"; id: number; ctx: Record<string, unknown> }
  | { t: "terminate"; id: number };

/** Guest → host. */
export type GuestMessage =
  | { t: "ready"; origin: string }
  | { t: "booted"; origin: string }
  | { t: "fatal"; error: string }
  | { t: "terminated"; id: number }
  | { t: "result"; id: number; ok: boolean; value?: unknown; error?: string }
  | { t: "call"; id: number; method: string; args: unknown[] };

/** One escape attempt, and what happened. */
export interface ConformanceRow {
  scope: "guest" | "worker";
  name: string;
  allowed: boolean;
  detail: string;
}

/**
 * The A/B that stops the probe passing for the wrong reason.
 *
 * Every row below is a *negative*: something that must fail. A machine with no
 * network fails all of them too, and would be reported as a verified sandbox —
 * which is the single most dangerous way this test could be wrong. So the host
 * page fetches the very URL the guest was refused. If the host cannot reach it
 * either, nothing was proved and the report says so.
 */
export interface ConformanceControl {
  name: string;
  succeeded: boolean;
  detail: string;
}

export interface ConformanceReport {
  ok: boolean;
  at: number;
  appOrigin: string;
  guestOrigin: string;
  rows: ConformanceRow[];
  /** The positive control. A failed control makes the whole run inconclusive. */
  control?: ConformanceControl;
  /** The names of anything that was *not* blocked. Empty is the only pass. */
  failures: string[];
  /** Set when the sandbox could not even be stood up. */
  error?: string;
}
