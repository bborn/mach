import { Search } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { Overlay } from "@/components/ui/dialog";
import { BareInput } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { formatBinding, type KeyBinding } from "@/lib/keymap";

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
 *   * **Every key the row talks about is a chip in that column.** A row may
 *     carry more than one — `↓ ⇥` for two spellings of the same step, `⇧↑ ⇧↓`
 *     for a pair that is one idea and two directions — and the description
 *     stays a label either way. See `KeyBinding.alsoKeys`. The rule this
 *     enforces is that a description never names a key: the sheet used to
 *     print "Next event ↓ ↑, nearest event on another day ← →" against a
 *     single `↓`, and a reader could not tell what any of the four keys did
 *     without reading a sentence to the end.
 *   * **Columns balance by content, not by group.** Two columns of sections,
 *     laid out with CSS multi-column so the browser splits them by height:
 *     Global's five rows no longer sit beside Mail's twenty.
 *   * **Nothing is truncated.** A description too long for the column wraps
 *     under itself. Truncation on a reference card hides the one word you
 *     opened it for.
 *   * **Filtering matches what is drawn, not what is registered.** The field
 *     tests each row's description, its formatted key label — "G then I", not
 *     "g i" — and the group's heading, because those three are what is on
 *     screen to search for.
 *   * **Escape always closes, even with text in the field.** `App.tsx` binds
 *     it unconditionally to `setShortcuts(null)`. Finding the row is the
 *     terminal action here, not the filter text — a first Escape that only
 *     cleared the field would leave the corner's "Esc to close" needing a
 *     second press to be true.
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
  "Visibility",
];

export interface SheetRow {
  /**
   * Every chip on the row, left to right — usually one, and more when the
   * binding named others through `alsoKeys`.
   */
  keys: string[];
  description: string;
  /**
   * What `Kbd` actually draws — "G then I", "⌘⌥→" — as opposed to `keys`,
   * which is the registry's internal token ("g i", "mod+alt+right"). The
   * filter matches against this, not against `keys`: matching the token
   * would mean a search that finds things the row never displayed. Every chip
   * on the row is in it, so a search for "↑" finds the row that draws one
   * whichever of its keys carried the description.
   */
  keyLabel: string;
}

export type SheetGroup = readonly [group: string, rows: SheetRow[]];

/** Registration order, deduped, grouped — nothing filtered yet. */
export function buildGroups(bindings: readonly KeyBinding[]): SheetGroup[] {
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
    const keys = [binding.keys, ...(binding.alsoKeys ?? [])];
    if (!list.some((item) => item.keys[0] === binding.keys)) {
      list.push({
        keys,
        description: binding.description,
        keyLabel: keys.map((key) => formatBinding(key)).join(" "),
      });
    }
    byGroup.set(group, list);
  }

  const rank = (name: string) => {
    const index = GROUP_ORDER.indexOf(name);
    return index === -1 ? GROUP_ORDER.length : index;
  };
  return [...byGroup.entries()].sort(([a], [b]) => rank(a) - rank(b) || a.localeCompare(b));
}

/**
 * Rows whose description or displayed key contains `query`, plus every row of
 * a group whose *heading* does. A group left with nothing is dropped rather
 * than printed with an empty body — an uppercase heading over nothing is
 * exactly the noise the filter exists to cut.
 *
 * The heading counts because it is on screen and because the rows under it
 * stopped repeating it. "Delete the event" and "Copy the event" said "event"
 * four times under a heading that already said EVENT; they say "Delete" and
 * "Copy" now, and without this a search for "event" would answer with nothing
 * from the group named after it.
 */
export function filterGroups(groups: readonly SheetGroup[], query: string): SheetGroup[] {
  const q = query.trim().toLowerCase();
  if (!q) return groups.slice();

  const result: SheetGroup[] = [];
  for (const [group, rows] of groups) {
    if (group.toLowerCase().includes(q)) {
      result.push([group, rows]);
      continue;
    }
    const matched = rows.filter(
      (row) =>
        row.description.toLowerCase().includes(q) || row.keyLabel.toLowerCase().includes(q),
    );
    if (matched.length > 0) result.push([group, matched]);
  }
  return result;
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
  const [query, setQuery] = useState("");

  // A reference card starts blank every time it's opened — a leftover filter
  // from the last visit would hide rows for no reason visible on screen.
  useEffect(() => {
    if (!open) setQuery("");
  }, [open]);

  const groups = useMemo(() => buildGroups(bindings), [bindings]);
  const filtered = useMemo(() => filterGroups(groups, query), [groups, query]);

  return (
    <Overlay open={open} onClose={onClose} align="center" className="max-w-[42rem] max-h-[84vh]">
      <div className="flex items-baseline justify-between border-b border-border px-4 py-2.5">
        <h2 className="text-list font-medium text-foreground">Keyboard</h2>
        <span className="text-micro text-faint-foreground">Esc to close</span>
      </div>

      {/*
        The one field in the sheet, so it is also where the overlay's focus
        trap lands on open — no `autoFocus`, no `initialFocus` ref, just being
        the first focusable element in the panel. Typing filters immediately
        because nothing else here takes text.
      */}
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border px-4">
        <Search size={13} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
        <BareInput
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Filter shortcuts"
          aria-label="Filter shortcuts"
        />
      </div>

      {/*
        `columns` rather than a grid: a grid would need to be told which group
        goes in which column, and the answer changes with the mode and with how
        many accounts are connected. The browser balances by height for free.
        One column when the window is too narrow to give each two a description
        wide enough to read.
      */}
      <div className="overflow-y-auto px-4 py-3">
        {filtered.length === 0 ? (
          <div className="px-1 py-6 text-center text-list text-faint-foreground">No matches</div>
        ) : (
          <div className="columns-1 gap-x-8 min-[36rem]:columns-2">
            {filtered.map(([group, list]) => (
              <section key={group} className="mb-4 break-inside-avoid last:mb-0">
                <h3 className="mb-1.5 text-micro font-medium uppercase tracking-[0.08em] text-faint-foreground">
                  {group}
                </h3>
                <ul className="flex flex-col gap-1">
                  {list.map((row) => (
                    <li key={row.keys.join(" ")} className="flex items-baseline gap-2.5">
                      {/*
                        Fixed width, right-set. The chips hug the gutter and every
                        description below them starts on the same line, which is the
                        whole reason the column exists. `G then I` is the widest
                        single chip `formatBinding` produces and it fits; so does the
                        widest pair the sheet draws, `⌥⇧↑ ⌥⇧↓`.
                      */}
                      <span className="flex w-[4.75rem] shrink-0 justify-end gap-1">
                        {row.keys.map((key) => (
                          <Kbd key={key} keys={key} />
                        ))}
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
        )}
      </div>
    </Overlay>
  );
}
