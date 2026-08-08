import { useEffect, useMemo, useState } from "react";
import { usePlugins } from "@/hooks/usePlugins";
import { useKeyBindings } from "@/hooks/useKeymap";
import { fuzzyScore } from "@/lib/palette/score";
import { cn } from "@/lib/utils";
import { Overlay } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { BareInput } from "@/components/ui/input";

/**
 * `mach.ask.*`, drawn by the host.
 *
 * A plugin cannot render HTML, so this is the only shape a plugin-initiated
 * prompt can take — and that is a security property, not an ergonomic
 * compromise. It is why a plugin cannot draw a convincing "re-authorize your
 * Google account" box: the frame, the chrome and the attribution line are ours,
 * and the only thing the plugin supplies is text inside them.
 *
 * The attribution is always present and always says the plugin's name. A prompt
 * the user cannot trace back to whoever asked is a prompt that can impersonate
 * the app.
 */
export function PluginAskDialog() {
  const { ask } = usePlugins();
  const [query, setQuery] = useState("");
  const [text, setText] = useState("");
  const [cursor, setCursor] = useState(0);

  useEffect(() => {
    setQuery("");
    setCursor(0);
    setText(ask?.initial ?? "");
  }, [ask]);

  const items = useMemo(() => {
    const all = ask?.items ?? [];
    const q = query.trim();
    if (!q) return all.slice(0, 200);
    return all
      .map((item) => ({
        item,
        score: Math.max(fuzzyScore(item.title, q), fuzzyScore(item.subtitle ?? "", q) * 0.6),
      }))
      .filter((scored) => scored.score > 0)
      .sort((a, b) => b.score - a.score)
      .slice(0, 200)
      .map((scored) => scored.item);
  }, [ask, query]);

  // Dismissal is a *value* — `null` for pick and text, `false` for confirm —
  // because a plugin has to be able to tell "cancelled" from "chose nothing".
  const dismiss = () => ask?.resolve(ask.kind === "confirm" ? false : null);

  useKeyBindings([
    {
      keys: "escape",
      priority: 130,
      allowInInput: true,
      when: () => ask !== null,
      handler: dismiss,
    },
    {
      keys: "enter",
      priority: 130,
      allowInInput: true,
      when: () => ask !== null,
      handler: () => {
        if (!ask) return;
        if (ask.kind === "confirm") return ask.resolve(true);
        if (ask.kind === "text") return ask.resolve(text);
        const chosen = items[cursor];
        if (chosen) ask.resolve(chosen.value);
      },
    },
    {
      keys: "down",
      priority: 130,
      allowInInput: true,
      when: () => ask?.kind === "pick",
      handler: () => setCursor((c) => Math.min(c + 1, Math.max(items.length - 1, 0))),
    },
    {
      keys: "up",
      priority: 130,
      allowInInput: true,
      when: () => ask?.kind === "pick",
      handler: () => setCursor((c) => Math.max(c - 1, 0)),
    },
  ]);

  if (!ask) return null;

  return (
    <Overlay open onClose={dismiss} labelledBy="plugin-ask-title" className="max-w-lg">
      <div className="flex flex-col gap-3 p-4">
        <div>
          <p className="text-xs text-muted-foreground">{ask.pluginName}</p>
          <h2 id="plugin-ask-title" className="text-sm font-medium">
            {ask.title}
          </h2>
        </div>

        {ask.kind === "pick" && (
          <>
            <BareInput
              autoFocus
              value={query}
              placeholder="Filter…"
              onChange={(e) => {
                setQuery(e.target.value);
                setCursor(0);
              }}
              className="border-b pb-2 text-sm"
            />
            <div className="max-h-72 overflow-y-auto" role="listbox">
              {items.map((item, index) => (
                <button
                  key={item.id}
                  type="button"
                  role="option"
                  aria-selected={index === cursor}
                  onMouseEnter={() => setCursor(index)}
                  onClick={() => ask.resolve(item.value)}
                  className={cn(
                    "flex w-full flex-col items-start rounded px-2 py-1.5 text-left text-sm",
                    index === cursor ? "bg-accent" : "hover:bg-accent/50",
                  )}
                >
                  <span>{item.title}</span>
                  {item.subtitle && (
                    <span className="text-xs text-muted-foreground">{item.subtitle}</span>
                  )}
                </button>
              ))}
              {items.length === 0 && (
                <p className="px-2 py-3 text-sm text-muted-foreground">Nothing matches.</p>
              )}
            </div>
          </>
        )}

        {ask.kind === "text" && (
          <BareInput
            autoFocus
            value={text}
            placeholder={ask.placeholder}
            onChange={(e) => setText(e.target.value)}
            className="border-b pb-2 text-sm"
          />
        )}

        {ask.kind === "confirm" && ask.body && <p className="text-sm">{ask.body}</p>}

        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={dismiss}>
            Cancel
          </Button>
          {ask.kind !== "pick" && (
            <Button
              variant={ask.danger ? "danger" : "default"}
              onClick={() => ask.resolve(ask.kind === "confirm" ? true : text)}
            >
              {ask.kind === "confirm" ? "Confirm" : "Save"}
            </Button>
          )}
        </div>
      </div>
    </Overlay>
  );
}
