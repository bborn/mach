import { useCallback, useEffect, useState } from "react";
import { ShieldAlert, ShieldCheck, TriangleAlert } from "lucide-react";
import { usePlugins } from "@/hooks/usePlugins";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import type { InstallCandidate } from "@/lib/plugins/backend";
import type { InstalledPlugin, PluginStatus } from "@/lib/plugins/types";
import { errorMessage } from "@/lib/ipc";
import { cn } from "@/lib/utils";
import { Overlay } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { BareInput } from "@/components/ui/input";

/** Opened from ⌘K, so the panel does not have to be a route or a rail item. */
export const PLUGINS_EVENT = "mach:plugins";

/**
 * The plugin list, and the install prompt.
 *
 * With tier 2 approved, **the install prompt is the entire security control**
 * for the plugins that have a network — so it is written to be read, not
 * clicked through:
 *
 *  - every capability is a sentence about a *consequence*, not an API name
 *    (Chrome MV3's most transferable habit), and the sentences come from Rust
 *    so the prompt and the enforcement cannot drift apart;
 *  - the bigger asks are visually louder, and the loudest is reserved for the
 *    one that actually matters — "this can send anything it reads to <host>";
 *  - a network justification is reproduced **verbatim**, in quotes, attributed
 *    to the author. Summarising it is how consent becomes a formality;
 *  - the buttons are named after what they do ("Install Quick File"), never
 *    "OK".
 *
 * And the list itself says why a plugin is not running, in the same words: a
 * plugin that is disabled, or that changed under an unchanged version number,
 * or that now asks for more than was approved, is exactly the thing the owner
 * needs to be able to see.
 */
export function PluginsPanel() {
  const { plugins, conformance, verified, backend, refresh } = usePlugins();
  const { actions } = useMach();
  const [open, setOpen] = useState(false);
  const [path, setPath] = useState("");
  const [dev, setDev] = useState(false);
  const [candidate, setCandidate] = useState<InstallCandidate | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const show = () => setOpen(true);
    window.addEventListener(PLUGINS_EVENT, show);
    return () => window.removeEventListener(PLUGINS_EVENT, show);
  }, []);

  useKeyBindings([
    {
      keys: "escape",
      priority: 125,
      allowInInput: true,
      when: () => open,
      handler: () => (candidate ? setCandidate(null) : setOpen(false)),
    },
  ]);

  const inspect = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      setCandidate(await backend.inspect(path.trim(), dev));
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  }, [backend, path, dev]);

  const install = useCallback(async () => {
    if (!candidate) return;
    setBusy(true);
    try {
      await backend.install(path.trim(), dev);
      setCandidate(null);
      setPath("");
      await refresh();
      actions.setStatus(`Installed ${candidate.manifest.name}`);
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  }, [backend, candidate, path, dev, refresh, actions]);

  if (!open) return null;

  return (
    <Overlay open onClose={() => setOpen(false)} labelledBy="plugins-title" className="max-w-2xl">
      <div className="flex max-h-[80vh] flex-col gap-4 overflow-y-auto p-5">
        <div className="flex items-baseline justify-between">
          <h2 id="plugins-title" className="text-sm font-medium">
            Plugins
          </h2>
          <SandboxBadge
            available={backend.available}
            verified={verified}
            detail={conformance?.failures.join(", ")}
          />
        </div>

        {/*
          Two different things, and conflating them would be a lie: a browser
          tab has no `plugin://` protocol to serve a guest from, which is not
          the same as a sandbox that was tested and failed.
        */}
        {!backend.available ? (
          <p className="rounded border p-3 text-sm text-muted-foreground">
            Plugins run in the desktop app. This window is on fixture data, so there is no
            sandbox to load them into.
          </p>
        ) : (
          !verified && (
            <p className="rounded border border-destructive/40 bg-destructive/10 p-3 text-sm">
              The plugin sandbox could not be verified in this window, so nothing will be
              loaded.{conformance?.error ? ` ${conformance.error}` : ""}
            </p>
          )
        )}

        {plugins.length === 0 ? (
          <p className="text-sm text-muted-foreground">Nothing installed.</p>
        ) : (
          <ul className="flex flex-col gap-2">
            {plugins.map((plugin) => (
              <PluginRow
                key={plugin.id}
                plugin={plugin}
                onToggle={async (enabled) => {
                  await backend.setEnabled(plugin.id, enabled);
                  await refresh();
                }}
                onRemove={async () => {
                  await backend.remove(plugin.id);
                  await refresh();
                }}
              />
            ))}
          </ul>
        )}

        <div className="flex flex-col gap-2 border-t pt-4">
          <p className="text-xs text-muted-foreground">
            Install from a directory containing <code>mach-plugin.json</code>. Nothing is
            executed until you accept the prompt.
          </p>
          <div className="flex items-center gap-2">
            <BareInput
              value={path}
              placeholder="/path/to/a/plugin"
              onChange={(e) => setPath(e.target.value)}
              className="flex-1 border-b pb-1 text-sm"
            />
            <label className="flex items-center gap-1 text-xs text-muted-foreground">
              <input type="checkbox" checked={dev} onChange={(e) => setDev(e.target.checked)} />
              development install
            </label>
            <Button disabled={!path.trim() || busy || !backend.available} onClick={() => void inspect()}>
              Review
            </Button>
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
      </div>

      {candidate && (
        <ConsentPrompt
          candidate={candidate}
          busy={busy}
          onCancel={() => setCandidate(null)}
          onAccept={() => void install()}
        />
      )}
    </Overlay>
  );
}

function SandboxBadge({
  available,
  verified,
  detail,
}: {
  available: boolean;
  verified: boolean;
  detail?: string;
}) {
  if (!available) {
    return <span className="text-xs text-muted-foreground">desktop app only</span>;
  }
  const Icon = verified ? ShieldCheck : ShieldAlert;
  return (
    <span
      title={detail}
      className={cn(
        "flex items-center gap-1 text-xs",
        verified ? "text-muted-foreground" : "text-destructive",
      )}
    >
      <Icon className="size-3.5" />
      {verified ? "sandbox verified" : "sandbox not verified"}
    </span>
  );
}

function PluginRow({
  plugin,
  onToggle,
  onRemove,
}: {
  plugin: InstalledPlugin;
  onToggle: (enabled: boolean) => void;
  onRemove: () => void;
}) {
  const status = describeStatus(plugin.status);
  return (
    <li className="flex items-start justify-between gap-3 rounded border p-3">
      <div className="min-w-0">
        <p className="text-sm font-medium">
          {plugin.manifest.name}{" "}
          <span className="text-xs font-normal text-muted-foreground">
            {plugin.manifest.version}
          </span>
        </p>
        <p className="text-xs text-muted-foreground">{plugin.manifest.description}</p>
        {status && (
          <p className="mt-1 flex items-center gap-1 text-xs text-amber-600 dark:text-amber-500">
            <TriangleAlert className="size-3" />
            {status}
          </p>
        )}
      </div>
      <div className="flex shrink-0 gap-2">
        <Button
          variant="ghost"
          onClick={() => onToggle(plugin.status.state === "disabled")}
        >
          {plugin.status.state === "disabled" ? "Enable" : "Disable"}
        </Button>
        <Button variant="ghost" onClick={onRemove}>
          Remove
        </Button>
      </div>
    </li>
  );
}

function describeStatus(status: PluginStatus): string | null {
  switch (status.state) {
    case "ready":
      return null;
    case "disabled":
      return "Disabled.";
    case "safeMode":
      return "Mach is in safe mode, so no plugin is running.";
    case "invalid":
      return status.detail;
    case "changedWithoutVersionBump":
      return "Its files changed but its version did not. Review it before enabling it again.";
    case "needsReapproval":
      return `It now asks for more than you approved: ${status.detail.join(", ")}.`;
  }
}

/**
 * The prompt. The one place a user decides what a stranger's code may do.
 */
function ConsentPrompt({
  candidate,
  busy,
  onCancel,
  onAccept,
}: {
  candidate: InstallCandidate;
  busy: boolean;
  onCancel: () => void;
  onAccept: () => void;
}) {
  const { manifest, consent, addedCapabilities, alreadyInstalled } = candidate;
  const dangerous = consent.some((line) => line.severity === "danger");

  return (
    <Overlay open onClose={onCancel} align="center" className="max-w-xl">
      <div className="flex flex-col gap-4 p-5">
        <div>
          <p className="text-xs text-muted-foreground">
            {candidate.install === "development" ? "Development install" : "Install"}
            {manifest.author ? ` · by ${manifest.author}` : ""}
          </p>
          <h2 className="text-base font-medium">
            {manifest.name} {manifest.version}
          </h2>
          <p className="text-sm text-muted-foreground">{manifest.description}</p>
        </div>

        {alreadyInstalled && addedCapabilities.length > 0 && (
          <div className="rounded border border-amber-500/40 bg-amber-500/10 p-3 text-sm">
            <p className="font-medium">This update asks for more than you approved:</p>
            <ul className="ml-4 list-disc">
              {addedCapabilities.map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          </div>
        )}

        <div>
          <p className="mb-1 text-sm font-medium">If you install it, it will be able to:</p>
          <ul className="flex flex-col gap-1.5">
            {consent.map((line, index) => (
              <li
                key={index}
                className={cn(
                  "rounded px-2 py-1.5 text-sm",
                  line.severity === "danger" && "bg-destructive/15 font-medium text-destructive",
                  line.severity === "warning" && "bg-amber-500/10",
                  line.severity === "note" && "text-muted-foreground",
                )}
              >
                {line.text}
              </li>
            ))}
          </ul>
        </div>

        {manifest.machApiProposed.length > 0 && (
          <p className="text-xs text-muted-foreground">
            Uses proposed APIs ({manifest.machApiProposed.join(", ")}), which work only in a
            development install and can change or vanish without notice.
          </p>
        )}

        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            variant={dangerous ? "danger" : "default"}
            disabled={busy}
            onClick={onAccept}
          >
            {alreadyInstalled ? "Update" : "Install"} {manifest.name}
          </Button>
        </div>
      </div>
    </Overlay>
  );
}
