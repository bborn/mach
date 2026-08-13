/**
 * The filter field, tested as the pure functions it is built from.
 *
 * `buildGroups`/`filterGroups` are exported specifically so this file does
 * not need jsdom or `@testing-library/react` to prove the matching rules —
 * see `Toast.test.tsx` for the same move. `renderToStaticMarkup` covers the
 * one thing the pure functions can't: that the field is actually the markup
 * the overlay's focus trap will land on, and that the corner still says
 * "Esc to close".
 */

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { formatBinding, type KeyBinding } from "@/lib/keymap";
import { buildGroups, filterGroups, ShortcutSheet } from "./ShortcutSheet";

function binding(over: Partial<KeyBinding> & { keys: string }): KeyBinding {
  return { description: undefined, group: undefined, order: 0, handler: () => {}, ...over };
}

const BINDINGS: KeyBinding[] = [
  binding({ keys: "?", description: "Keyboard shortcuts", group: "Global", order: 0 }),
  binding({ keys: "e", description: "Archive", group: "Actions", order: 1 }),
  binding({ keys: "g i", description: "Go to inbox", group: "Go to", order: 2 }),
  binding({ keys: "mod+alt+right", description: "Next account", group: "Accounts", order: 3 }),
];

describe("buildGroups", () => {
  it("drops bindings with no description, so unlisted keys stay unlisted", () => {
    const groups = buildGroups([...BINDINGS, binding({ keys: "x" })]);
    const allKeys = groups.flatMap(([, rows]) => rows.flatMap((r) => r.keys));
    expect(allKeys).not.toContain("x");
  });

  it("formats the key label the same way `Kbd` would draw it", () => {
    const groups = buildGroups(BINDINGS);
    const goTo = groups.find(([group]) => group === "Go to")?.[1];
    expect(goTo?.[0].keyLabel).toBe("G then I");
  });

  it("draws every key `alsoKeys` names as a chip on the same row", () => {
    // The shape the calendar's arrows now register in: one described binding
    // that names its partner, and the partner registered beside it with no
    // description so it does not also print a row of its own.
    const groups = buildGroups([
      binding({
        keys: "up",
        alsoKeys: ["shift+tab"],
        description: "Previous event",
        group: "Calendar",
        order: 0,
      }),
      binding({ keys: "shift+tab", order: 1 }),
    ]);
    expect(groups).toEqual([
      [
        "Calendar",
        [{ keys: ["up", "shift+tab"], description: "Previous event", keyLabel: "↑ ⇧⇥" }],
      ],
    ]);
  });
});

describe("filterGroups", () => {
  const groups = buildGroups(BINDINGS);

  it("matches on the description", () => {
    // "how do I archive" → someone types "arch".
    const filtered = filterGroups(groups, "arch");
    expect(filtered).toEqual([["Actions", [expect.objectContaining({ description: "Archive" })]]]);
  });

  it("matches on the formatted key label, not the internal token", () => {
    // The raw token is "g i" — no "then" in it anywhere. Only the formatted
    // label ("G then I") contains that substring, so a match here proves
    // filtering reads what `Kbd` draws rather than the registry's token.
    const filtered = filterGroups(groups, "then");
    expect(filtered).toEqual([["Go to", [expect.objectContaining({ keys: ["g i"] })]]]);
  });

  it("finds a row by a key that is not the one carrying the description", () => {
    // ⇧⇥ has no description of its own — it rides on ↑'s row. Someone who has
    // just pressed it and wants to know what it was still has to be able to
    // type it into the field and land on that row.
    const paired = buildGroups([
      binding({
        keys: "up",
        alsoKeys: ["shift+tab"],
        description: "Previous event",
        group: "Calendar",
        order: 0,
      }),
    ]);
    expect(filterGroups(paired, "⇧⇥")).toEqual([
      ["Calendar", [expect.objectContaining({ description: "Previous event" })]],
    ]);
  });

  it("matches a modifier chip the same way: by what's drawn, not the raw token", () => {
    // Raw token "mod+alt+right" contains the literal word "right"; the
    // rendered chip (platform-dependent glyphs, an arrow instead of a word)
    // does not. Searching "right" finds nothing; searching a piece of what
    // `Kbd` actually draws does.
    const label = formatBinding("mod+alt+right");
    expect(label).not.toContain("right");
    expect(filterGroups(groups, "right")).toEqual([]);

    const filtered = filterGroups(groups, label.slice(-1));
    expect(filtered).toEqual([
      ["Accounts", [expect.objectContaining({ description: "Next account" })]],
    ]);
  });

  it("keeps a whole group when its heading matches", () => {
    // "Go to inbox" is the only row that says "go to"; the heading is why all
    // of that group answers. Rows stopped repeating their heading when
    // "Delete the event" became "Delete", and this is what keeps them findable
    // by the word the heading still carries.
    const filtered = filterGroups(groups, "go to");
    expect(filtered.map(([group]) => group)).toEqual(["Go to"]);
    expect(filtered[0]?.[1]).toHaveLength(1);
  });

  it("returns nothing for a query that matches no row", () => {
    expect(filterGroups(groups, "zzz-nonexistent")).toEqual([]);
  });

  it("drops a group entirely once every one of its rows is filtered out", () => {
    const filtered = filterGroups(groups, "archive");
    const groupNames = filtered.map(([group]) => group);
    expect(groupNames).toEqual(["Actions"]);
    expect(groupNames).not.toContain("Go to");
    expect(groupNames).not.toContain("Accounts");
    expect(groupNames).not.toContain("Global");
  });

  it("is case-insensitive and ignores surrounding whitespace", () => {
    expect(filterGroups(groups, "  ARCH  ")).toEqual(filterGroups(groups, "arch"));
  });

  it("returns every group unchanged for a blank query", () => {
    expect(filterGroups(groups, "   ")).toEqual(groups);
  });
});

describe("the markup", () => {
  it("puts the filter field first, so the overlay's focus trap lands on it", () => {
    const html = renderToStaticMarkup(
      <KeymapProvider>
        <ShortcutSheet open bindings={BINDINGS} onClose={() => {}} />
      </KeymapProvider>,
    );
    const inputAt = html.indexOf("<input");
    const firstRowAt = html.indexOf("Archive");
    expect(inputAt).toBeGreaterThan(-1);
    expect(inputAt).toBeLessThan(firstRowAt);
    expect(html).toContain('placeholder="Filter shortcuts"');
  });

  it("keeps advertising Esc to close, since Escape still always closes", () => {
    const html = renderToStaticMarkup(
      <KeymapProvider>
        <ShortcutSheet open bindings={BINDINGS} onClose={() => {}} />
      </KeymapProvider>,
    );
    expect(html).toContain("Esc to close");
  });

  it("draws one chip per key on the row, in the order the binding named them", () => {
    const html = renderToStaticMarkup(
      <KeymapProvider>
        <ShortcutSheet
          open
          bindings={[
            binding({
              keys: "up",
              alsoKeys: ["shift+tab"],
              description: "Previous event",
              group: "Calendar",
              order: 0,
            }),
          ]}
          onClose={() => {}}
        />
      </KeymapProvider>,
    );
    expect(html.indexOf(">↑<")).toBeGreaterThan(-1);
    expect(html.indexOf(">↑<")).toBeLessThan(html.indexOf(">⇧⇥<"));
    expect(html.indexOf(">⇧⇥<")).toBeLessThan(html.indexOf("Previous event"));
  });

  it("renders nothing when the sheet is closed", () => {
    const html = renderToStaticMarkup(
      <KeymapProvider>
        <ShortcutSheet open={false} bindings={BINDINGS} onClose={() => {}} />
      </KeymapProvider>,
    );
    expect(html).toBe("");
  });
});

/**
 * The one rule this sheet exists to keep, checked against the source rather
 * than against a fixture.
 *
 * A description that names a key is a row that should have been several — the
 * calendar shipped "Next event ↓ ↑, nearest event on another day ← →" against
 * a single `↓` chip, four behaviours in one sentence, and nobody could read it.
 * A fixture cannot catch that: the defect is in what the *features* register,
 * so this reads what they register. `alsoKeys` is the way to say "and this key
 * too", and it puts the key in the column the eye is already running down.
 *
 * Only the glyphs `formatBinding` actually draws are banned, plus its "then"
 * for sequences. A description is free to say "day" or "period".
 */
describe("descriptions in the registry", () => {
  const GLYPHS = ["⌘", "⌃", "⌥", "⇧", "↑", "↓", "←", "→", "⇥", "⌫", "↩"];

  function sources(dir: string): string[] {
    const out: string[] = [];
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) out.push(...sources(path));
      else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)) out.push(path);
    }
    return out;
  }

  it("never names a key that should have been a chip", () => {
    const root = new URL("../..", import.meta.url).pathname;
    const offenders: string[] = [];
    let seen = 0;

    for (const file of sources(root)) {
      const text = readFileSync(file, "utf8");
      for (const match of text.matchAll(/description:\s*"([^"]*)"/g)) {
        const description = match[1] ?? "";
        seen += 1;
        if (GLYPHS.some((glyph) => description.includes(glyph)) || / then /.test(description)) {
          offenders.push(`${file.slice(root.length)}: "${description}"`);
        }
      }
    }

    expect(offenders).toEqual([]);
    // A scan that walked the wrong directory would pass on nothing at all.
    expect(seen).toBeGreaterThan(50);
  });
});
