/**
 * No binding may quietly take a key the composer needs.
 *
 * # Why this is a source scan rather than a behaviour test
 *
 * Every composer fault this week was the same fault, and none of them was in the
 * editor:
 *
 *  * ⏎ did nothing in a reply opened from a search result. `SearchView` binds
 *    Enter `allowInInput: true` so it works in the search field — and that made
 *    it work inside the message too, where it opened a result instead of making
 *    a line. ⇧⏎ is a different token, missed the binding, and worked, which is
 *    what made it look like the editor's fault.
 *  * ↑ and ↓ moved the search cursor while the caret should have been moving
 *    through the message. Same three bindings, same cause, never reported
 *    because ⏎ was the one that hurt.
 *  * ⌘⌫ threw the whole draft away instead of deleting to the start of the
 *    line, because the composer's discard was `allowInInput` on macOS's own
 *    editing key. Twice, and the second press confirmed it.
 *
 * A behaviour test cannot catch the *next* one, because the next one is a
 * binding nobody has written yet in a file this test does not know about. What
 * these have in common is visible in the source: `allowInInput: true` on a key
 * that means something to a text editor, with nothing standing it down while
 * the caret is in a composer.
 *
 * So: a binding may claim an editing key inside a field only if it is the
 * composer's own, or a surface that takes the keyboard outright, or it is gated
 * on `keyboardInComposer`. Anything else fails here, by name, before it reaches
 * anybody's hands.
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * Keys a text editor owns. Bare tokens and modified ones both: ⌘⌫ is macOS's
 * "delete to the start of the line" and was taken once already.
 */
const EDITING_KEYS = new Set([
  "enter",
  "backspace",
  "delete",
  "up",
  "down",
  "left",
  "right",
  "home",
  "end",
  "pageup",
  "pagedown",
  "space",
  "mod+backspace",
  "mod+delete",
  "mod+a",
  "mod+left",
  "mod+right",
  "mod+up",
  "mod+down",
  "alt+backspace",
  "alt+left",
  "alt+right",
]);

/**
 * Files whose bindings are allowed to claim an editing key while typing.
 *
 * Two kinds, and the distinction is the whole rule. The composer's own files
 * are the editor — ⌘⏎ sends, and it has to reach the keyboard from inside the
 * message. The rest are surfaces that *take* the keyboard when they open: a
 * dialog, the palette, an approval. They are modal, so there is no composer
 * underneath them expecting the key.
 *
 * A file being on this list is a claim about it, not an exemption from thought.
 * Adding one means saying which of those two it is.
 */
const MAY_CLAIM = new Set([
  // The composer itself.
  "components/mail/Composer.tsx",
  "components/mail/ComposerDock.tsx",
  "components/mail/RichTextEditor.tsx",
  // Modal surfaces: they claim the keyboard, and nothing is being typed behind
  // them that is not theirs.
  "components/palette/CommandPalette.tsx",
  "components/handoff/HandoffDialog.tsx",
  "components/handoff/SessionPane.tsx",
  "components/plugins/PluginAskDialog.tsx",
  "components/calendar/EventModal.tsx",
  "components/ui/dialog.tsx",
  "components/ui/select.tsx",
  "components/ui/address-typeahead.tsx",
]);

function sources(dir: string, found: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) sources(path, found);
    else if (/\.tsx?$/.test(entry) && !/\.test\.tsx?$/.test(entry)) found.push(path);
  }
  return found;
}

/**
 * Every binding in a file, as `keys` plus the text between it and its handler.
 *
 * `keys` and `handler` bracket a binding's metadata in every one of these — the
 * gate, the priority and `allowInInput` all sit between them — so the window is
 * exactly the thing to read without parsing TypeScript.
 */
function bindings(source: string): { keys: string; body: string }[] {
  const out: { keys: string; body: string }[] = [];
  const pattern = /keys:\s*(?:"([^"]+)"|`([^`]+)`)/g;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(source)) !== null) {
    const keys = match[1] ?? match[2] ?? "";
    const rest = source.slice(match.index, match.index + 900);
    const end = rest.indexOf("handler");
    out.push({ keys, body: end === -1 ? rest : rest.slice(0, end) });
  }
  return out;
}

const ROOT = fileURLToPath(new URL("..", import.meta.url));

describe("bindings that are live while the caret is in a field", () => {
  it("do not take a key the composer needs", () => {
    const offenders: string[] = [];

    for (const path of sources(ROOT)) {
      const relative = path.slice(ROOT.length).replace(/\\/g, "/");
      if (MAY_CLAIM.has(relative)) continue;
      const source = readFileSync(path, "utf8");

      for (const binding of bindings(source)) {
        if (!EDITING_KEYS.has(binding.keys.toLowerCase())) continue;
        if (!/allowInInput:\s*true/.test(binding.body)) continue;
        // Standing down inside a composer is the whole remedy.
        if (binding.body.includes("keyboardInComposer")) continue;
        /*
         * A binding at the overlay floor only exists while a surface has
         * claimed the keyboard — a picker, a dialog, an approval. Nothing is
         * being typed behind one of those that is not its own, so Enter there
         * is the surface's to take. See `claimKeyboard` in `keymap.ts`.
         */
        if (binding.body.includes("OVERLAY_KEY_FLOOR")) continue;
        offenders.push(`${relative} claims ${binding.keys} while typing`);
      }
    }

    expect(offenders).toEqual([]);
  });

  /*
   * The three that were wrong, named. The scan above would catch them again on
   * its own; this says which they were, so a future reader knows the rule was
   * paid for rather than guessed at.
   */
  it("stands the search view down inside a composer", () => {
    const source = readFileSync(join(ROOT, "components/mail/SearchView.tsx"), "utf8");
    for (const keys of ["enter", "up", "down"]) {
      const binding = bindings(source).find((b) => b.keys === keys);
      expect(binding, `SearchView should still bind ${keys}`).toBeDefined();
      expect(binding?.body).toContain("keyboardInComposer");
    }
  });
});
