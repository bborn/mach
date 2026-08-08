import { useMemo } from "react";
import { Overlay } from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import type { KeyBinding } from "@/lib/keymap";

/**
 * `?` — the bindings that were live at the moment it was pressed.
 *
 * The list is a *snapshot* taken by the caller rather than read from the
 * registry here, because opening the sheet is itself a mode change: every
 * calendar binding is gated on the sheet being closed, so asking the registry
 * what is live while the sheet is open answers "almost nothing".
 *
 * Google's shortcuts are off by default and buried in settings, which §8 names
 * as a failure mode. Mach's are on, and one key away.
 */
export function ShortcutSheet({
  open,
  bindings,
  onClose,
}: {
  open: boolean;
  bindings: readonly KeyBinding[];
  onClose: () => void;
}) {
  const groups = useMemo(() => {
    const byGroup = new Map<string, { keys: string; description: string }[]>();
    for (const binding of bindings) {
      if (!binding.description) continue;
      const group = binding.group ?? "Other";
      const list = byGroup.get(group) ?? [];
      if (!list.some((item) => item.keys === binding.keys)) {
        list.push({ keys: binding.keys, description: binding.description });
      }
      byGroup.set(group, list);
    }
    return [...byGroup.entries()].sort(([a], [b]) => a.localeCompare(b));
  }, [bindings]);

  return (
    <Overlay open={open} onClose={onClose} align="center" className="max-w-[32rem]">
      <div className="flex items-baseline justify-between border-b border-border px-3 py-2">
        <h2 className="text-list font-medium text-foreground">Keyboard</h2>
        <span className="text-micro text-faint-foreground">Esc to close</span>
      </div>
      <div className="grid grid-cols-2 items-start gap-x-4 overflow-y-auto p-3">
        {groups.map(([group, list]) => (
          <section key={group} className="mb-3">
            <h3 className="mb-1 text-micro uppercase tracking-[0.06em] text-faint-foreground">
              {group}
            </h3>
            <ul className="flex flex-col gap-1">
              {list.map((binding) => (
                <li key={binding.keys} className="flex items-baseline gap-2">
                  <Kbd keys={binding.keys} />
                  <span className="min-w-0 flex-1 truncate text-micro text-muted-foreground">
                    {binding.description}
                  </span>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </Overlay>
  );
}
