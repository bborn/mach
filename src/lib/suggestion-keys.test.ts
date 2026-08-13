/**
 * No binding may quietly take a stance key.
 *
 * # Why this is a source scan, like `composer-keys.test.ts`
 *
 * The stance row's keys are bare digits — `1`, `2`, `3` and `0` — chosen because
 * the digit is already on the button, so the gesture needs no learning. Bare
 * letters and digits are also the cheapest keys in the app to take by accident:
 * a feature adds one, gates it on a condition that overlaps, and the two resolve
 * by *whichever component mounted last* (see `conflicts()` in `keymap.ts`, and
 * the ⌘1 bug it documents). Nothing throws. The row simply stops answering, in
 * a way that looks like the suggestion never arrived.
 *
 * A behaviour test cannot catch the next one, because the next one is a binding
 * nobody has written yet in a file this test does not know about. What it *can*
 * do is enumerate every place a digit is bound and require each to be one of the
 * three shapes that cannot be live at the same moment as the row:
 *
 *  * the stance row's own file;
 *  * a binding at [[OVERLAY_KEY_FLOOR]] — it exists only while a surface holds
 *    the keyboard, and nothing is behind one of those but its own dialog;
 *  * a binding in a mode the stance row is not (the calendar's `1`/`2`/`3`).
 *
 * Anything else fails here, by name, before it reaches anybody's hands.
 *
 * The second test is the other half: it checks the row's own bindings still
 * carry the gate that makes them safe, so the invariant cannot be satisfied by
 * quietly widening the row instead.
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { MAX_KEYED_STANCES, SUGGESTION_KEYS } from "./suggestions";

/** Every key the stance row claims. */
const STANCE_KEYS = new Set<string>([
  SUGGESTION_KEYS.mine,
  ...Array.from({ length: MAX_KEYED_STANCES }, (_, i) => SUGGESTION_KEYS.stance(i)),
]);

/**
 * Files whose digit bindings are allowed to exist.
 *
 * Each is a claim about *why*, not an exemption from thought — adding one means
 * saying which of the three shapes above it is.
 */
const MAY_CLAIM = new Map<string, string>([
  // The row itself, and the module that names its keys.
  ["components/mail/ComposerDock.tsx", "the stance row's own bindings"],
  ["components/mail/StanceRow.tsx", "the buttons the keys mirror"],
  ["lib/suggestions.ts", "where the keys are defined"],
  // A different mode. `1`/`2`/`3` switch calendar views and are gated on the
  // calendar being on screen; the stance row is gated on mail mode.
  ["components/calendar/CalendarMode.tsx", "calendar mode, not mail mode"],
  // The snooze picker's digits are at OVERLAY_KEY_FLOOR and live only while its
  // overlay holds the keyboard.
  ["components/mail/mail-bindings.ts", "an overlay that claims the keyboard"],
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
 * Every binding in a file, as its literal `keys` plus the text between it and
 * its handler — which is where the gate, the priority and `allowInInput` sit.
 *
 * Templated keys (`` `alt+${n}` ``) are captured too, so a binding that builds
 * a bare digit at runtime is visible here rather than hiding behind a template.
 */
function bindings(source: string): { keys: string; body: string }[] {
  const out: { keys: string; body: string }[] = [];
  const pattern = /keys:\s*(?:"([^"]*)"|`([^`]*)`|String\(([^)]*)\))/g;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(source)) !== null) {
    const keys = match[1] ?? match[2] ?? (match[3] !== undefined ? "<computed>" : "");
    const rest = source.slice(match.index, match.index + 900);
    const end = rest.indexOf("handler");
    out.push({ keys, body: end === -1 ? rest : rest.slice(0, end) });
  }
  return out;
}

const ROOT = fileURLToPath(new URL("..", import.meta.url));

describe("the stance keys", () => {
  it("are not bound anywhere that could be live at the same time", () => {
    const offenders: string[] = [];

    for (const path of sources(ROOT)) {
      const relative = path.slice(ROOT.length).replace(/\\/g, "/");
      if (MAY_CLAIM.has(relative)) continue;
      const source = readFileSync(path, "utf8");

      for (const binding of bindings(source)) {
        // A computed key that could be a bare digit is as dangerous as a
        // literal one, and it is not visible to a plain string comparison.
        const claims =
          STANCE_KEYS.has(binding.keys) ||
          (binding.keys === "<computed>" && !/\+/.test(binding.body));
        if (!claims) continue;
        offenders.push(`${relative} binds ${binding.keys}, which the stance row needs`);
      }
    }

    expect(offenders).toEqual([]);
  });

  /*
   * The files that are allowed to bind a digit, named with the reason. This is
   * the part that stops the allowlist from growing by habit: a new entry has to
   * be added here as well, which is a sentence somebody has to write.
   */
  it("are only claimed by a mode, an overlay, or the row itself", () => {
    const calendar = readFileSync(join(ROOT, "components/calendar/CalendarMode.tsx"), "utf8");
    // The calendar's view digits are gated on the calendar being active.
    expect(calendar).toMatch(/keys:\s*view\.keys[\s\S]{0,200}when:\s*\(\)\s*=>\s*active/);

    const picker = readFileSync(join(ROOT, "components/mail/mail-bindings.ts"), "utf8");
    // The snooze picker's are at the overlay floor, so they exist only while it
    // holds the keyboard.
    const digits = picker.slice(picker.indexOf("const digits ="), picker.indexOf("const digits =") + 500);
    expect(digits).toContain("OVERLAY_KEY_FLOOR");
  });

  it("stand down whenever a composer is on screen for this conversation", () => {
    const dock = readFileSync(join(ROOT, "components/mail/ComposerDock.tsx"), "utf8");
    // The gate that makes bare digits safe: mail mode, no composer, no draft,
    // and something to press. Named once and used by both the keys and the row.
    expect(dock).toMatch(
      /const stanceRowLive =\s*\n?\s*active && visible === null && stanceKeys\.length > 0 && !threadDraft;/,
    );
    for (const key of ["SUGGESTION_KEYS.stance(index)", "SUGGESTION_KEYS.mine"]) {
      const at = dock.indexOf(`keys: ${key}`);
      expect(at, `${key} should be bound in the dock`).toBeGreaterThan(-1);
      const window = dock.slice(at, at + 400);
      expect(window).toContain("when: () => stanceRowLive");
      // Never live while typing: a digit in the search box is a character.
      expect(window).not.toContain("allowInInput");
    }
  });
});
