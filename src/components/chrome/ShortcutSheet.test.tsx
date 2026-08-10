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
    const allKeys = groups.flatMap(([, rows]) => rows.map((r) => r.keys));
    expect(allKeys).not.toContain("x");
  });

  it("formats the key label the same way `Kbd` would draw it", () => {
    const groups = buildGroups(BINDINGS);
    const goTo = groups.find(([group]) => group === "Go to")?.[1];
    expect(goTo?.[0].keyLabel).toBe("G then I");
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
    expect(filtered).toEqual([["Go to", [expect.objectContaining({ keys: "g i" })]]]);
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

  it("renders nothing when the sheet is closed", () => {
    const html = renderToStaticMarkup(
      <KeymapProvider>
        <ShortcutSheet open={false} bindings={BINDINGS} onClose={() => {}} />
      </KeymapProvider>,
    );
    expect(html).toBe("");
  });
});
