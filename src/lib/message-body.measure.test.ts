import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import {
  frameDocument,
  MESSAGE_COLUMN_MAX,
  MESSAGE_COLUMN_PADDING,
  TEXT_MEASURE,
  type BodyFormat,
} from "@/lib/message-body";

/**
 * The column a plain-text body is rendered in.
 *
 * The bug this pins down: a `text/plain` body arrives with its column already
 * chosen by the sender's generator, and Mach was handing it a narrower one. The
 * reading pane capped the message at `max-w-[72ch]`, where `ch` is the width of
 * a zero in the *app's* font — 645px, of which the body inside got 605px after
 * padding. An automated digest wrapped at about ninety columns needs about
 * 625px, so every line in it spilled its last word onto a line of its own, at
 * column zero. That is what was reported as "all ragged".
 *
 * The two numbers therefore have to stay in step, and they live in two files.
 */
describe("the plain-text measure", () => {
  const styles = (format: BodyFormat): string =>
    frameDocument({ html: "<div>hi</div>", allowRemoteImages: false, format });

  it("gives a body we turned into HTML a column of its own", () => {
    expect(styles("text")).toContain(`body{max-width:${TEXT_MEASURE}}`);
  });

  it("gives a snippet the same column", () => {
    // A snippet goes through the same text path and is rendered by the same
    // stylesheet; there is no reason for it to be laid out differently.
    expect(styles("snippet")).toContain(`body{max-width:${TEXT_MEASURE}}`);
  });

  it("leaves a sender's own HTML alone", () => {
    // The sender chose their layout. Imposing a measure on it would be the
    // same category of mistake as imposing the app's colours on it — see the
    // frame's ground.
    expect(styles("html")).not.toContain("max-width:" + TEXT_MEASURE);
    expect(styles("html")).not.toMatch(/body\{max-width:/);
  });

  /**
   * The measure is only real if the frame can reach it. `ReadingPane` sizes its
   * column from these constants rather than from a number of its own, and this
   * is what stops the two drifting apart again — the previous number was
   * written down once, in Tailwind, in a unit that meant something else.
   */
  it("is what ReadingPane sizes its column from", () => {
    const source = readFileSync(
      new URL("../components/mail/ReadingPane.tsx", import.meta.url),
      "utf8",
    );
    expect(source).toContain("MESSAGE_COLUMN_MAX");
    expect(source, "the old cap is still there").not.toContain("max-w-[72ch]");
    // The padding the constant accounts for has to be the padding actually
    // applied, or the body lands somewhere other than the measure.
    expect(source).toMatch(/className="[^"]*\bpx-5\b/);
    expect(MESSAGE_COLUMN_PADDING).toBe("2.5rem");
    expect(MESSAGE_COLUMN_MAX).toBe(`calc(${TEXT_MEASURE} + ${MESSAGE_COLUMN_PADDING})`);
  });

  /**
   * The number itself. 40rem is 640px, which holds the 78 characters RFC 5322
   * asks senders to stay within, and the ninety-column lines real bookkeeping
   * software emits anyway.
   *
   * A smaller measure does not make a hard-wrapped body tidier — it only
   * changes which word gets orphaned — so this is a floor as much as a ceiling.
   */
  it("is wide enough for the columns mail is actually wrapped at", () => {
    const rem = Number(TEXT_MEASURE.replace("rem", ""));
    expect(TEXT_MEASURE).toMatch(/^[\d.]+rem$/);
    // At the frame's 15px body font, an average character is about 0.44em.
    const charactersAtWidest = (rem * 16) / (15 * 0.47);
    expect(charactersAtWidest).toBeGreaterThan(80);
  });
});
