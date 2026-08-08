/**
 * The plugin host: what is loaded, what is activated, and who is allowed to ask.
 *
 * # Lazy, because contributions are static
 *
 * The manifest says what a plugin adds — actions, their keys, their keywords —
 * and the host reads that without executing a line of plugin code. So ⌘K lists
 * a plugin's action and the keymap answers its binding before the plugin has
 * ever run. The iframe is created on the **first invocation**, not at boot.
 * VS Code's activation-events idea reduced to its useful core: a plugin costs
 * nothing until it is used.
 *
 * # Three ways in, one way through
 *
 * A palette entry, a keybinding and an agent tool all land on
 * [`PluginManager.run`]. There is no second path, so the capability check, the
 * timeout, the undo grouping and the attribution happen once rather than three
 * times.
 *
 * # The gate
 *
 * Nothing loads until [`PluginManager.start`] has run the conformance probe and
 * it has passed. A failure is not a warning: `plugins` stays empty, the reason
 * is reported, and the app carries on being a mail client.
 */

import type { Command, CommandResult, MachDataSource } from "@/lib/data";
import type { InstalledPlugin, PluginAction, PluginId, ConformanceReport } from "./types";
import type { PluginBackend, InvokeRequest } from "./backend";
import { PluginSandbox, iframeTransport } from "./sandbox";
import { createHostApi, localPluginStore, type AskHost } from "./api";
import { runConformance, describeConformance } from "./conformance";

export interface PluginManagerOptions {
  backend: PluginBackend;
  source: MachDataSource;
  ask: AskHost;
  notify: (message: string, tone: "info" | "error") => void;
  log?: (...args: unknown[]) => void;
  /** Called when an action finishes, with the inverses it accumulated. */
  onUndoGroup?: (label: string, inverses: Command[]) => void;
  /** Told when the set of installed plugins changes, so the UI can re-read. */
  onChange?: () => void;
}

/** What a plugin action is handed. */
export interface ActionContext {
  threadIds?: number[];
  threadId?: number | null;
  eventId?: number | null;
  params?: Record<string, unknown>;
}

export interface LoadedPlugin {
  installed: InstalledPlugin;
  sandbox: PluginSandbox | null;
  /** Set once the module has been imported in the worker. */
  activated: boolean;
  /**
   * Where the inverses of the currently-running action are collected.
   *
   * Held on the entry rather than passed into the sandbox, because the sandbox
   * outlives any one invocation — it is created on first use and reused. Two
   * invocations of the *same* plugin overlapping would share a group, which is
   * the right answer often enough and is never wrong in a way that loses an
   * inverse.
   */
  collecting: ((command: Command, result: CommandResult) => void) | null;
  error?: string;
}

export class PluginManager {
  private readonly options: PluginManagerOptions;
  private loaded = new Map<PluginId, LoadedPlugin>();
  private assets: { workerSource: string; canarySource: string } | null = null;
  private report: ConformanceReport | null = null;
  private unlistenInvoke: (() => void) | null = null;

  constructor(options: PluginManagerOptions) {
    this.options = options;
  }

  /**
   * Verify the sandbox, then read what is installed.
   *
   * In that order, and it matters: the conformance probe is the only reason to
   * believe the boundary holds, and a plugin loaded before it passed is a
   * plugin that ran unverified.
   */
  async start(): Promise<ConformanceReport | null> {
    if (!this.options.backend.available) return null;

    const assets = await this.options.backend.sandboxAssets();
    this.assets = assets;

    const report = await runConformance({
      workerSource: assets.workerSource,
      canarySource: assets.canarySource,
    });
    this.report = report;
    await this.options.backend.reportConformance(report);

    if (!report.ok) {
      this.options.log?.(describeConformance(report));
      this.options.notify(
        "Plugins are disabled: the sandbox could not be verified in this window.",
        "error",
      );
      return report;
    }

    await this.refresh();
    this.unlistenInvoke = await this.options.backend.onInvoke((request) => {
      void this.answerAgent(request);
    });
    return report;
  }

  /** Re-read the plugin list. Cheap: the manifests are already parsed in Rust. */
  async refresh(): Promise<void> {
    if (!this.verified()) {
      this.loaded = new Map();
      this.options.onChange?.();
      return;
    }
    const installed = await this.options.backend.list();
    const next = new Map<PluginId, LoadedPlugin>();
    for (const plugin of installed) {
      const existing = this.loaded.get(plugin.id);
      // A plugin whose files changed has to be re-imported, so an existing
      // sandbox is only reused while the manifest version is the same.
      const reusable =
        existing && existing.installed.manifest.version === plugin.manifest.version;
      next.set(plugin.id, {
        installed: plugin,
        sandbox: reusable ? existing.sandbox : null,
        activated: reusable ? existing.activated : false,
        collecting: null,
      });
      if (existing && !reusable) existing.sandbox?.destroy();
    }
    for (const [id, existing] of this.loaded) {
      if (!next.has(id)) existing.sandbox?.destroy();
    }
    this.loaded = next;
    this.options.onChange?.();
  }

  verified(): boolean {
    return this.report?.ok === true;
  }

  conformance(): ConformanceReport | null {
    return this.report;
  }

  /** Everything installed, in manifest order. Includes the ones that cannot run. */
  list(): LoadedPlugin[] {
    return [...this.loaded.values()];
  }

  /** Every action the app should offer right now, with its plugin. */
  actions(): { plugin: InstalledPlugin; action: PluginAction }[] {
    return this.list()
      .filter((entry) => entry.installed.status.state === "ready")
      .flatMap((entry) =>
        entry.installed.manifest.contributes.actions.map((action) => ({
          plugin: entry.installed,
          action,
        })),
      );
  }

  /**
   * Run one action. The only way anything a plugin exports is ever called.
   *
   * Every `mach.run` the action makes is collected, and the inverses are handed
   * back as **one group** labelled with the action's title. That is the whole of
   * "undo comes free": a plugin never constructs an inverse, and ⌘Z takes back
   * the label *and* the archive in one step because both went through the
   * command layer.
   */
  async run(pluginId: PluginId, actionId: string, ctx: ActionContext = {}): Promise<unknown> {
    const entry = this.loaded.get(pluginId);
    if (!entry) throw new Error(`there is no plugin called ${pluginId}`);
    if (entry.installed.status.state !== "ready") {
      throw new Error(`${pluginId} is not running: ${entry.installed.status.state}`);
    }
    const action = entry.installed.manifest.contributes.actions.find((a) => a.id === actionId);
    if (!action) throw new Error(`${pluginId} has no action called "${actionId}"`);

    const inverses: Command[] = [];
    const sandbox = await this.activate(entry);
    entry.collecting = (_command, result) => {
      if (result.undo && result.applied.length > 0) inverses.push(result.undo);
    };

    try {
      return await sandbox.invoke("actions", actionId, { ...ctx });
    } finally {
      entry.collecting = null;
      // Even a throw halfway through leaves real writes behind, and those have
      // to be undoable. The group is pushed for what actually happened.
      if (inverses.length > 0) {
        this.options.onUndoGroup?.(action.title.replace(/…$/, ""), inverses);
      }
    }
  }

  /** Render one of a plugin's views. Returns `null` when it has nothing to say. */
  async view(pluginId: PluginId, viewId: string, ctx: ActionContext): Promise<unknown> {
    const entry = this.loaded.get(pluginId);
    if (!entry || entry.installed.status.state !== "ready") return null;
    const sandbox = await this.activate(entry);
    return sandbox.invoke("views", viewId, { ...ctx });
  }

  destroy(): void {
    this.unlistenInvoke?.();
    for (const entry of this.loaded.values()) entry.sandbox?.destroy();
    this.loaded = new Map();
  }

  // ------------------------------------------------------------------ private

  /** Create the iframe and import the module, once, on first use. */
  private async activate(entry: LoadedPlugin): Promise<PluginSandbox> {
    if (entry.sandbox && entry.activated) return entry.sandbox;
    if (!this.assets) throw new Error("the plugin sandbox has not been started");

    const id = entry.installed.id;
    const manifest = entry.installed.manifest;

    const sandbox = new PluginSandbox({
      manifest,
      transport: iframeTransport(id),
      workerSource: this.assets.workerSource,
      api: createHostApi({
        id,
        name: manifest.name,
        source: this.options.source,
        ask: this.options.ask,
        notify: (message, tone) => this.options.notify(`${manifest.name}: ${message}`, tone),
        log: (...args) => this.options.log?.(`[${id}]`, ...args),
        onRun: (command, result) => entry.collecting?.(command, result),
        store: localPluginStore(id),
      }),
      onDisabled: (reason) => {
        this.options.notify(reason, "error");
        void this.options.backend.setEnabled(id, false).then(() => this.refresh());
      },
      onLog: (...args) => this.options.log?.(`[${id}]`, ...args),
    });
    const source = await this.options.backend.source(id);
    await sandbox.load(source);

    entry.sandbox = sandbox;
    entry.activated = true;
    return sandbox;
  }

  /**
   * The agent asking for an action, through the Rust bridge.
   *
   * Nothing here decides whether the agent was *allowed* to ask: that is the
   * tool list's job, on the Rust side, where `capabilities.agent` and the
   * inherited approval policy live. This end runs it and reports.
   */
  private async answerAgent(request: InvokeRequest): Promise<void> {
    try {
      const value = await this.run(request.pluginId, request.action, {
        params: request.input ?? {},
        threadIds: Array.isArray((request.input as { threadIds?: number[] })?.threadIds)
          ? (request.input as { threadIds: number[] }).threadIds
          : [],
      });
      await this.options.backend.answerInvoke(request.requestId, true, value ?? null);
    } catch (error) {
      await this.options.backend.answerInvoke(
        request.requestId,
        false,
        null,
        error instanceof Error ? error.message : String(error),
      );
    }
  }
}
