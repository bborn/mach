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
 *
 * # Why this reads the way it does
 *
 * It is a reference card. You do not read a reference card, you *find* one row
 * on it, and everything here serves that:
 *
 *   * **Order is chosen, not incidental.** `active()` hands bindings back in
 *     precedence order — recency first, with priority jumping the queue — which
 *     put Mail's twenty keys on the page as "Esc, Forward, Reply all, Reply,
 *     All accounts, …". Groups run in the order below; rows inside a group run
 *     in the order the feature declared them, which is the order somebody
 *     chose. See `KeyBinding.order`.
 *   * **The key column is fixed.** Chips are between one and eight characters
 *     wide (`?` … `G then I`), so a chip-then-text row leaves the descriptions
 *     on a ragged edge that the eye cannot run down. The chips are right-set in
 *     a column of their own and every description starts at the same x.
 *   * **Columns balance by content, not by group.** Two columns of sections,
 *     laid out with CSS multi-column so the browser splits them by height:
 *     Global's five rows no longer sit beside Mail's twenty.
 *   * **Nothing is truncated.** A description too long for the column wraps
 *     under itself. Truncation on a reference card hides the one word you
 *     opened it for.
 */

/**
 * The order the groups print in: what you are looking at, where you go, what
 * you do there, and then the surfaces you are not in.
 *
 * Anything not named here sorts alphabetically after everything that is, so a
 * new group — a plugin's, say — appears rather than disappearing.
 */
const GROUP_ORDER = [
  "Global",
  "Go to",
  "Mail",
  "Actions",
  "Selection",
  "Write",
  "Composer",
  "Accounts",
  "Sidebar",
  "Calendar",
  "Event",
  "Calendars",
];

interface SheetRow {
  keys: string;
  description: string;
}

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
    const byGroup = new Map<string, SheetRow[]>();

    // Registration order, not precedence order. `order` is stamped by the
    // registry; a binding without one sorts last rather than sorting randomly.
    const declared = [...bindings].sort(
      (a, b) => (a.order ?? Number.MAX_SAFE_INTEGER) - (b.order ?? Number.MAX_SAFE_INTEGER),
    );

    for (const binding of declared) {
      if (!binding.description) continue;
      const group = binding.group ?? "Other";
      const list = byGroup.get(group) ?? [];
      if (!list.some((item) => item.keys === binding.keys)) {
        list.push({ keys: binding.keys, description: binding.description });
      }
      byGroup.set(group, list);
    }

    const rank = (name: string) => {
      const index = GROUP_ORDER.indexOf(name);
      return index === -1 ? GROUP_ORDER.length : index;
    };
    return [...byGroup.entries()].sort(
      ([a], [b]) => rank(a) - rank(b) || a.localeCompare(b),
    );
  }, [bindings]);

  return (
    <Overlay open={open} onClose={onClose} align="center" className="max-w-[42rem] max-h-[84vh]">
      <div className="flex items-baseline justify-between border-b border-border px-4 py-2.5">
        <h2 className="text-list font-medium text-foreground">Keyboard</h2>
        <span className="text-micro text-faint-foreground">Esc to close</span>
      </div>

      {/*
        `columns` rather than a grid: a grid would need to be told which group
        goes in which column, and the answer changes with the mode and with how
        many accounts are connected. The browser balances by height for free.
        One column when the window is too narrow to give each two a description
        wide enough to read.
      */}
      <div className="overflow-y-auto px-4 py-3">
        <div className="columns-1 gap-x-8 min-[36rem]:columns-2">
          {groups.map(([group, list]) => (
            <section key={group} className="mb-4 break-inside-avoid last:mb-0">
              <h3 className="mb-1.5 text-micro font-medium uppercase tracking-[0.08em] text-faint-foreground">
                {group}
              </h3>
              <ul className="flex flex-col gap-1">
                {list.map((row) => (
                  <li key={row.keys} className="flex items-baseline gap-2.5">
                    {/*
                      Fixed width, right-set. The chip hugs the gutter and every
                      description below it starts on the same line, which is the
                      whole reason the column exists. `G then I` is the widest
                      thing `formatBinding` produces and it fits.
                    */}
                    <span className="flex w-[4.75rem] shrink-0 justify-end">
                      <Kbd keys={row.keys} />
                    </span>
                    <span className="min-w-0 flex-1 text-micro leading-[1.45] text-muted-foreground">
                      {row.description}
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>
      </div>
    </Overlay>
  );
}
