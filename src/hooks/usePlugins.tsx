import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { getDataSource } from "@/lib/data";
import { createPluginBackend, nullBackend, type PluginBackend } from "@/lib/plugins/backend";
import { isTauri, tauriTransport } from "@/lib/ipc";
import { PluginManager } from "@/lib/plugins/manager";
import { setPaletteActions, clearPaletteActions } from "@/lib/plugins/palette";
import type { AskHost } from "@/lib/plugins/api";
import type { ConformanceReport, InstalledPlugin } from "@/lib/plugins/types";
import { useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";

/**
 * The plugin system's one React seam.
 *
 * It owns three things and delegates everything else:
 *
 *  1. **The manager's lifetime** — one per window, started once, torn down with
 *     the window.
 *  2. **The host-rendered prompts.** `mach.ask.*` is a promise on the plugin
 *     side and a dialog on this side, and the dialog is drawn by *us* with our
 *     components, attributed to the plugin by name. A plugin cannot draw a
 *     login box because a plugin cannot draw.
 *  3. **Registration into the two frontend registries** — the ⌘K resolver chain
 *     and the keymap. Both read static manifest text, so both work before a
 *     plugin has ever been activated.
 *
 * Plugin bindings register in a band **below every core binding** (priority
 * -10). A plugin can never take `e`, and if two plugins want ⌥F the keymap's
 * existing conflict reporting says so.
 */

const PLUGIN_KEY_PRIORITY = -10;

export interface AskRequest {
  kind: "pick" | "text" | "confirm";
  pluginName: string;
  title: string;
  body?: string;
  placeholder?: string;
  initial?: string;
  danger?: boolean;
  items?: { id: string; title: string; subtitle?: string; value: unknown }[];
  resolve: (value: unknown) => void;
}

interface PluginsValue {
  plugins: InstalledPlugin[];
  conformance: ConformanceReport | null;
  /** False until the sandbox has been verified in this window. */
  verified: boolean;
  backend: PluginBackend;
  ask: AskRequest | null;
  run: (pluginId: string, actionId: string) => void;
  /** Render one contributed view. Returns `null` when it has nothing to say. */
  view: (pluginId: string, viewId: string, ctx: { threadId: number | null }) => Promise<unknown>;
  refresh: () => Promise<void>;
}

const PluginsContext = createContext<PluginsValue | null>(null);

export function PluginProvider({ children }: { children: ReactNode }) {
  const { actions, ui, commandTargets } = useMach();
  const [plugins, setPlugins] = useState<InstalledPlugin[]>([]);
  const [conformance, setConformance] = useState<ConformanceReport | null>(null);
  const [ask, setAsk] = useState<AskRequest | null>(null);
  const managerRef = useRef<PluginManager | null>(null);

  const backend = useMemo(
    () => (isTauri() ? createPluginBackend(tauriTransport) : nullBackend),
    [],
  );

  // Actions read the live selection without rebuilding the manager, which owns
  // an iframe per plugin and must not be recreated on every keystroke.
  const targetsRef = useRef(commandTargets);
  targetsRef.current = commandTargets;
  const threadRef = useRef(ui.threadId);
  threadRef.current = ui.threadId;

  const askHost = useMemo<AskHost>(
    () => ({
      pick: (o) =>
        new Promise((resolve) =>
          setAsk({ kind: "pick", ...o, resolve: (v) => (setAsk(null), resolve(v)) }),
        ),
      text: (o) =>
        new Promise((resolve) =>
          setAsk({
            kind: "text",
            ...o,
            resolve: (v) => (setAsk(null), resolve(v as string | null)),
          }),
        ),
      confirm: (o) =>
        new Promise((resolve) =>
          setAsk({
            kind: "confirm",
            ...o,
            resolve: (v) => (setAsk(null), resolve(Boolean(v))),
          }),
        ),
    }),
    [],
  );

  useEffect(() => {
    const manager = new PluginManager({
      backend,
      source: getDataSource(),
      ask: askHost,
      notify: (message, tone) => actions.setStatus(message, tone),
      log: (...args) => console.info("[plugins]", ...args),
      // The grouped inverse: an action's several commands undo as one step,
      // labelled with the action's own title.
      onUndoGroup: (label, inverses) => actions.pushUndoGroup(label, inverses),
      onChange: () => setPlugins(manager.list().map((entry) => entry.installed)),
    });
    managerRef.current = manager;

    void manager
      .start()
      .then((report) => {
        setConformance(report);
        setPlugins(manager.list().map((entry) => entry.installed));
      })
      .catch((error) => console.warn("[plugins] could not start:", error));

    return () => {
      manager.destroy();
      managerRef.current = null;
      clearPaletteActions();
    };
    // Deliberately once per window: the manager owns iframes and a Tauri
    // listener, and rebuilding it on a state change would leak both.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backend]);

  const run = useCallback(
    (pluginId: string, actionId: string) => {
      const manager = managerRef.current;
      if (!manager) return;
      void manager
        .run(pluginId, actionId, {
          threadIds: targetsRef.current,
          threadId: threadRef.current,
        })
        .catch((error: unknown) =>
          actions.setStatus(error instanceof Error ? error.message : String(error), "error"),
        );
    },
    [actions],
  );

  // ⌘K. One resolver for every plugin, matching static manifest text.
  const paletteEntries = useMemo(
    () =>
      plugins
        .filter((plugin) => plugin.status.state === "ready")
        .flatMap((plugin) =>
          plugin.manifest.contributes.actions.map((action) => ({ plugin, action })),
        ),
    [plugins],
  );
  useEffect(() => {
    setPaletteActions(paletteEntries, run);
  }, [paletteEntries, run]);

  // The keymap. Below every core binding, so a plugin can never take `e`.
  const bindings = useMemo(
    () =>
      paletteEntries
        .filter((entry) => Boolean(entry.action.key))
        .map((entry) => ({
          keys: entry.action.key as string,
          description: `${entry.action.title} (${entry.plugin.manifest.name})`,
          group: "Plugins",
          priority: PLUGIN_KEY_PRIORITY,
          when: () => !ui.paletteOpen,
          handler: () => run(entry.plugin.id, entry.action.id),
        })),
    [paletteEntries, run, ui.paletteOpen],
  );
  useKeyBindings(bindings);

  const value: PluginsValue = {
    plugins,
    conformance,
    verified: conformance?.ok === true,
    backend,
    ask,
    run,
    view: useCallback(
      async (pluginId: string, viewId: string, ctx: { threadId: number | null }) =>
        (await managerRef.current?.view(pluginId, viewId, ctx)) ?? null,
      [],
    ),
    refresh: useCallback(async () => {
      await managerRef.current?.refresh();
      setPlugins(managerRef.current?.list().map((entry) => entry.installed) ?? []);
    }, []),
  };

  return <PluginsContext.Provider value={value}>{children}</PluginsContext.Provider>;
}

export function usePlugins(): PluginsValue {
  const value = useContext(PluginsContext);
  if (!value) throw new Error("usePlugins must be used inside <PluginProvider>");
  return value;
}
