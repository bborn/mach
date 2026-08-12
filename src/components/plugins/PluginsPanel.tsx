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
            Plugins run in the desktop app.
          </p>
        ) : (
          !verified && (
            <p className="rounded border border-destructive/40 bg-destructive/10 p-3 text-sm">
              Sandbox not verified — no plugin will load.{conformance?.error ? ` ${conformance.error}` : ""}
            </p>
          )
        )}

        {plugins.length === 0 ? (
          <p className="text-sm text-muted-foreground">Nothing installed</p>
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
                onFailed={(message) => {
                  setError(message);
                  actions.setStatus(message, "error");
                }}
              />
            ))}
          </ul>
        )}

        <div className="flex flex-col gap-2 border-t pt-4">
          <p className="text-xs text-muted-foreground">
            Install from a directory containing <code>mach-plugin.json</code>.
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

/**
 * One installed plugin, and the two buttons that change its life.
 *
 * Both of them used to do nothing on screen and swallow their failures at the
 * same time: `onToggle` and `onRemove` were bare `await`s with no busy state,
 * no optimistic flip and no `catch`, so "Enable" went on saying Enable through
 * the round trip and a refusal was an unhandled rejection nobody would ever
 * see. Enabling a plugin is exactly the kind of thing that has to say whether
 * it worked.
 *
 * The label flips now — the optimistic claim — and the row goes quiet while
 * the call is out. A refusal puts the label back and reports; there is no
 * third state to invent, because the button has only ever had two.
 */
function PluginRow({
  plugin,
  onToggle,
  onRemove,
  onFailed,
}: {
  plugin: InstalledPlugin;
  onToggle: (enabled: boolean) => Promise<void>;
  onRemove: () => Promise<void>;
  onFailed: (message: string) => void;
}) {
  const status = describeStatus(plugin.status);
  const disabled = plugin.status.state === "disabled";
  /** What this row claims about itself, until the list is refetched. */
  const [claimed, setClaimed] = useState<{ enabled: boolean } | null>(null);
  const [removed, setRemoved] = useState(false);
  const [working, setWorking] = useState(false);
  const enabled = claimed ? claimed.enabled : !disabled;

  // The list is authoritative again the moment it says the same thing.
  useEffect(() => {
    if (claimed && claimed.enabled === !disabled) setClaimed(null);
  }, [claimed, disabled]);

  if (removed) return null;
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
          disabled={working}
          onClick={() => {
            const next = !enabled;
            setClaimed({ enabled: next });
            setWorking(true);
            void onToggle(next)
              .catch((e: unknown) => {
                setClaimed(null);
                onFailed(errorMessage(e));
              })
              .finally(() => setWorking(false));
          }}
        >
          {enabled ? "Disable" : "Enable"}
        </Button>
        <Button
          variant="ghost"
          disabled={working}
          onClick={() => {
            setRemoved(true);
            void onRemove().catch((e: unknown) => {
              setRemoved(false);
              onFailed(errorMessage(e));
            });
          }}
        >
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
      return "Disabled";
    case "safeMode":
      return "Safe mode";
    case "invalid":
      return status.detail;
    case "changedWithoutVersionBump":
      return "Files changed, version did not";
    case "needsReapproval":
      return `Asks for more than you approved: ${status.detail.join(", ")}`;
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
          <p className="mb-1 text-sm font-medium">It will be able to:</p>
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
            Proposed APIs ({manifest.machApiProposed.join(", ")}) — development installs only,
            and unstable.
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
