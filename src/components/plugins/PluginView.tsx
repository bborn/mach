import { useEffect, useState } from "react";
import { usePlugins } from "@/hooks/usePlugins";
import type { ViewNode } from "@/lib/plugins/types";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";

/**
 * A plugin's declarative view, rendered with the app's own components.
 *
 * Eight node types, no HTML, no CSS, no layout control. A plugin describes
 * *meaning* and the host decides what it looks like — which is why the app can
 * be restyled without a flag day for plugins, and why a plugin cannot draw a
 * fake account-login box. The tone words are the only visual influence a plugin
 * has, and they map to the app's tokens rather than to colours.
 *
 * Anything unrecognised renders as nothing. A plugin written against a later
 * version of the vocabulary degrades instead of breaking the pane it is in.
 */
export function PluginViews({ surface, threadId }: { surface: string; threadId: number | null }) {
  const { plugins } = usePlugins();
  const views = plugins
    .filter((plugin) => plugin.status.state === "ready")
    .flatMap((plugin) =>
      plugin.manifest.contributes.views
        .filter((view) => view.surface === surface)
        .map((view) => ({ pluginId: plugin.id, name: plugin.manifest.name, viewId: view.id })),
    );

  if (views.length === 0) return null;

  return (
    <div className="flex flex-col gap-2">
      {views.map((view) => (
        <OneView
          key={`${view.pluginId}:${view.viewId}`}
          pluginId={view.pluginId}
          pluginName={view.name}
          viewId={view.viewId}
          threadId={threadId}
        />
      ))}
    </div>
  );
}

function OneView({
  pluginId,
  pluginName,
  viewId,
  threadId,
}: {
  pluginId: string;
  pluginName: string;
  viewId: string;
  threadId: number | null;
}) {
  const { run, view } = usePlugins();
  const [node, setNode] = useState<ViewNode | null>(null);

  useEffect(() => {
    let live = true;
    setNode(null);
    // A view is a pure function from context to a tree. It is called when the
    // surface is visible and re-called when its inputs change — never on a
    // render loop, and never while the list is painting.
    // A view that throws renders nothing. It is a decoration; it does not get
    // to break the conversation the user is reading.
    void view(pluginId, viewId, { threadId })
      .then((next) => {
        if (live) setNode((next as ViewNode | null) ?? null);
      })
      .catch(() => {
        if (live) setNode(null);
      });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pluginId, viewId, threadId]);

  if (!node) return null;

  return (
    <section
      className="rounded border border-border px-3 py-2"
      aria-label={`${pluginName} — plugin`}
    >
      {/* Attribution, always. A surface the user cannot trace back to whoever
          drew it is a surface that can impersonate the app. */}
      <p className="mb-1 text-micro uppercase tracking-wide text-faint-foreground">{pluginName}</p>
      <Node node={node} onAction={(action) => run(pluginId, action)} />
    </section>
  );

}

const TONES: Record<string, string> = {
  default: "",
  muted: "text-muted-foreground",
  warning: "text-amber-600 dark:text-amber-500",
  danger: "text-destructive",
};

function Node({ node, onAction }: { node: ViewNode; onAction: (action: string) => void }) {
  switch (node.type) {
    case "section":
      return (
        <div className="flex flex-col gap-1">
          {node.title && <p className="text-sm font-medium">{node.title}</p>}
          {node.children.map((child, index) => (
            <Node key={index} node={child} onAction={onAction} />
          ))}
        </div>
      );
    case "text":
      return <p className={cn("text-sm", TONES[node.tone ?? "default"])}>{node.value}</p>;
    case "row":
      return (
        <p className="flex items-baseline justify-between gap-3 text-sm">
          <span className="text-muted-foreground">{node.label}</span>
          <span className={TONES[node.tone ?? "default"]}>{node.value}</span>
        </p>
      );
    case "badge":
      return (
        <span
          className={cn(
            "inline-flex w-fit rounded bg-accent px-1.5 py-0.5 text-micro",
            TONES[node.tone ?? "default"],
          )}
        >
          {node.value}
        </span>
      );
    case "button":
      return (
        <Button variant="subtle" className="w-fit" onClick={() => onAction(node.action)}>
          {node.label}
        </Button>
      );
    case "list":
      return (
        <ul className="flex flex-col gap-0.5">
          {node.items.map((item, index) => (
            <li key={index}>
              <button
                type="button"
                disabled={!item.action}
                onClick={() => item.action && onAction(item.action)}
                className="w-full rounded px-1 py-0.5 text-left text-sm hover:bg-accent disabled:hover:bg-transparent"
              >
                {item.title}
                {item.subtitle && (
                  <span className="ml-2 text-xs text-muted-foreground">{item.subtitle}</span>
                )}
              </button>
            </li>
          ))}
        </ul>
      );
    case "separator":
      return <hr className="border-border" />;
    case "spinner":
      return <p className="text-sm text-muted-foreground">{node.label ?? "Working…"}</p>;
    default:
      // A node type this build does not know: render nothing rather than throw.
      return null;
  }
}
