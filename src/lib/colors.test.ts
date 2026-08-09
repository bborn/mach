import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  assignCalendarColors,
  contrastRatio,
  fitLightness,
  fromOklch,
  isColor,
  nearestRampSlot,
  oklchHue,
  readableInk,
  relativeLuminance,
  toOklch,
} from "./colors";
import {
  assignHues,
  calendarFill,
  calendarInk,
  CONTRAST_TARGET,
  FALLBACK_FILLS,
  paintFor,
  PAGE,
} from "./calendar-palette";

/**
 * Every colour Google will hand back for a calendar or an event.
 *
 * All three generations of the palette: the 24 `colorId`s of `calendarList`,
 * the 11 of `events`, and the eleven modern names ("Tomato" … "Graphite") the
 * web UI offers. The point of the list is that the guarantees below are
 * *properties*, checked against the whole input space rather than against three
 * colours somebody found convenient.
 */
const GOOGLE_PALETTE = [
  "#ac725e", "#d06b64", "#f83a22", "#fa573c", "#ff7537", "#ffad46",
  "#42d692", "#16a765", "#7bd148", "#b3dc6c", "#fbe983", "#fad165",
  "#92e1c0", "#9fe1e7", "#9fc6e7", "#4986e7", "#9a9cff", "#b99aff",
  "#c2c2c2", "#cabdbf", "#cca6ac", "#f691b2", "#cd74e6", "#a47ae2",
  "#a4bdfc", "#7ae7bf", "#dbadff", "#ff887c", "#fbd75b", "#ffb878",
  "#46d6db", "#e1e1e1", "#5484ed", "#51b749", "#dc2127",
  "#d50000", "#e67c73", "#f4511e", "#f6bf26", "#33b679", "#0b8043",
  "#039be5", "#3f51b5", "#7986cb", "#8e24aa", "#616161",
];

/** The colours actually on the author's account, named. */
const HIS = {
  "bruno.bornsztein@gmail.com": "#f83a22",
  Training: "#ffad46",
  "Jewish Holidays": "#16a765",
  "Dad/Ben Schedule": "#ec00af",
  "bruno@bornsztein.com": "#9fc6e7",
  "Alicia & Bruno": "#c56bda",
  "bruno.bornsztein@clickfunnels.com": "#b99aff",
};

/** A coarse sweep of the whole cube — 32³ colours, every corner and middle. */
function* everyColor(): Generator<string> {
  for (let r = 0; r < 256; r += 8)
    for (let g = 0; g < 256; g += 8)
      for (let b = 0; b < 256; b += 8) {
        yield `#${[r, g, b].map((c) => c.toString(16).padStart(2, "0")).join("")}`;
      }
}

describe("reading a colour", () => {
  it("accepts the shorthand and ignores the leading hash", () => {
    expect(relativeLuminance("#f00")).toBeCloseTo(relativeLuminance("#ff0000")!, 10);
    expect(relativeLuminance("ff0000")).toBeCloseTo(relativeLuminance("#ff0000")!, 10);
  });

  it("refuses anything that is not a colour", () => {
    expect(isColor("")).toBe(false);
    expect(isColor("rebeccapurple")).toBe(false);
    expect(isColor("#12345")).toBe(false);
    expect(isColor(undefined)).toBe(false);
    expect(isColor("#c2c2c2")).toBe(true);
  });

  it("puts the primaries where a person would put them", () => {
    // OKLCH hue angles, not HSL ones: red sits near 29°, green near 142°, blue
    // near 264°. These are the anchors everything else is judged against.
    expect(oklchHue("#ff0000")).toBeCloseTo(29.2, 0);
    expect(oklchHue("#00ff00")).toBeCloseTo(142.5, 0);
    expect(oklchHue("#0000ff")).toBeCloseTo(264.1, 0);
  });

  it("refuses to invent a hue for a neutral", () => {
    expect(oklchHue("#808080")).toBeNull();
    expect(oklchHue("#ffffff")).toBeNull();
    expect(oklchHue("#000000")).toBeNull();
  });

  it("round-trips OKLCH through sRGB", () => {
    for (const hex of GOOGLE_PALETTE) {
      expect(fromOklch(toOklch(hex)!)).toBe(hex);
    }
  });

  it("holds hue and lightness when a colour will not fit sRGB", () => {
    // A chroma no display can show. Clipping the channels would drag the hue;
    // giving up saturation is the mapping CSS Color 4 specifies.
    const wanted = { l: 0.6, c: 0.4, h: 150 };
    const got = toOklch(fromOklch(wanted))!;
    expect(got.h).toBeCloseTo(wanted.h, 0);
    expect(got.l).toBeCloseTo(wanted.l, 2);
    expect(got.c).toBeLessThan(wanted.c);
  });
});

describe("the contrast guarantee", () => {
  /*
   * This is the property the whole design rests on, and the reason a calendar
   * can now be painted in the colour its owner chose: for *any* sRGB colour,
   * the better of black and white clears 4.5:1. It is not a fact about Google's
   * palette — it is a fact about the WCAG formula, whose two ratios cross at
   * relative luminance 0.179 and are both 4.58:1 there.
   */
  it("clears 4.5:1 on every colour in sRGB", () => {
    let worst = Infinity;
    let worstAt = "";
    for (const hex of everyColor()) {
      const ratio = contrastRatio(hex, readableInk(hex))!;
      if (ratio < worst) {
        worst = ratio;
        worstAt = hex;
      }
    }
    expect(worst).toBeGreaterThanOrEqual(CONTRAST_TARGET);
    // Named so a future change that erodes the margin shows up as a diff rather
    // than as a still-passing test.
    expect(worst).toBeCloseTo(4.58, 1);
    expect(worstAt).toBeTruthy();
  });

  it("picks whichever of black and white is actually better", () => {
    for (const hex of GOOGLE_PALETTE) {
      const chosen = contrastRatio(hex, readableInk(hex))!;
      const other = Math.max(contrastRatio(hex, "#000000")!, contrastRatio(hex, "#ffffff")!);
      expect(chosen).toBeCloseTo(other, 6);
    }
    // Both directions are reachable: Banana takes black, Grape takes white.
    expect(readableInk("#fbd75b")).toBe("#000000");
    expect(readableInk("#8e24aa")).toBe("#ffffff");
  });

  it("does not trust Google's stored foregroundColor", () => {
    // Every row in a real store says `#000000`, including on fills where black
    // loses. Blueberry is one of them: black manages 3.06:1 there.
    expect(contrastRatio("#3f51b5", "#000000")!).toBeLessThan(CONTRAST_TARGET);
    expect(readableInk("#3f51b5")).toBe("#ffffff");
  });
});

describe("painting a calendar's colour", () => {
  it("uses the light theme fill verbatim", () => {
    for (const hex of Object.values(HIS)) {
      expect(calendarFill(hex, false)).toBe(hex);
      expect(paintFor(hex, "solid", { dark: false }).background).toBe(hex);
    }
  });

  it("keeps a calendar recognisably its own colour in the dark theme", () => {
    for (const hex of GOOGLE_PALETTE) {
      const dark = calendarFill(hex, true);
      const before = toOklch(hex)!;
      const after = toOklch(dark)!;
      // Only lightness moves, and only into the band. The 1.5° tolerance is
      // 8-bit quantisation, not drift: a near-neutral like `#9fc6e7` has so
      // little chroma that one code value of rounding swings its angle.
      if (before.c > 0.02) expect(Math.abs(after.h - before.h)).toBeLessThan(1.5);
      expect(after.c).toBeCloseTo(before.c, 2);
      expect(after.l).toBeGreaterThanOrEqual(0.51);
      expect(after.l).toBeLessThanOrEqual(0.79);
    }
  });

  it("moves the palette by an amount nobody would call a recolour", () => {
    // A clamp, not a remap: it catches the ends and passes the middle through.
    const deltas = GOOGLE_PALETTE.map((hex) =>
      Math.abs(toOklch(calendarFill(hex, true))!.l - toOklch(hex)!.l),
    );
    const unchanged = deltas.filter((d) => d < 1e-6).length;
    const mean = deltas.reduce((a, b) => a + b, 0) / deltas.length;
    expect(unchanged).toBeGreaterThanOrEqual(25);
    expect(mean).toBeLessThan(0.03);
    expect(Math.max(...deltas)).toBeLessThan(0.16);
  });

  it("makes every fill visible against the page it is drawn on", () => {
    // WCAG 1.4.11: a non-text element needs 3:1 to have a findable boundary.
    // This is the defect the dark-theme floor exists for — Blueberry, Grape and
    // Graphite all sink below it untouched.
    for (const hex of GOOGLE_PALETTE) {
      expect(contrastRatio(calendarFill(hex, true), PAGE.dark)!).toBeGreaterThanOrEqual(3);
    }
    expect(contrastRatio("#3f51b5", PAGE.dark)!).toBeLessThan(3);
  });

  it("keeps text on a solid block over 4.5:1 in both themes", () => {
    for (const hex of GOOGLE_PALETTE) {
      for (const dark of [false, true]) {
        const paint = paintFor(hex, "solid", { dark });
        expect(contrastRatio(paint.background, paint.color)!).toBeGreaterThanOrEqual(
          CONTRAST_TARGET,
        );
      }
    }
  });

  it("keeps an outlined block's border and time readable on the page", () => {
    for (const hex of GOOGLE_PALETTE) {
      for (const dark of [false, true]) {
        const page = dark ? PAGE.dark : PAGE.light;
        expect(contrastRatio(calendarInk(hex, dark), page)!).toBeGreaterThanOrEqual(
          CONTRAST_TARGET,
        );
      }
    }
  });

  it("moves an ink colour no further than it has to", () => {
    // Already dark enough on a white page: left exactly as it is.
    expect(fitLightness("#0b8043", PAGE.light, CONTRAST_TARGET)).toBe("#0b8043");
    // Too light: darkened, and only just past the line.
    const fitted = fitLightness("#fbd75b", PAGE.light, CONTRAST_TARGET);
    expect(contrastRatio(fitted, PAGE.light)!).toBeGreaterThanOrEqual(CONTRAST_TARGET);
    expect(contrastRatio(fitted, PAGE.light)!).toBeLessThan(CONTRAST_TARGET + 0.1);
    expect(toOklch(fitted)!.h).toBeCloseTo(toOklch("#fbd75b")!.h, 0);
  });

  it("survives a colour it cannot read", () => {
    expect(calendarFill("not a colour", false)).toBe(FALLBACK_FILLS[0]);
    expect(paintFor("", "solid", { dark: false }).background).toBe(FALLBACK_FILLS[0]);
  });
});

describe("the selection cursor's inner band", () => {
  /*
   * The cursor is 2px of `selectionGap` then 3px of accent. The old design used
   * the page background for the gap, which was safe only while every fill sat
   * at one lightness. The gap is now the block's own ink, so the step at the
   * block's edge is the same 4.5:1 the text gets — on every colour, in both
   * themes. This is the test that stops someone reverting it to
   * `var(--background)` for tidiness.
   */
  it("clears 4.5:1 against the fill it is drawn on", () => {
    for (const hex of GOOGLE_PALETTE) {
      for (const dark of [false, true]) {
        const paint = paintFor(hex, "solid", { dark });
        expect(contrastRatio(paint.background, paint.selectionGap)!).toBeGreaterThanOrEqual(
          CONTRAST_TARGET,
        );
      }
    }
  });

  it("agrees with the page background wherever the old rule worked", () => {
    // A dark fill in light mode, and a light fill in dark mode: the two cases
    // the luminance sandwich was designed around. The gap is unchanged there.
    expect(paintFor("#8e24aa", "solid", { dark: false }).selectionGap).toBe(PAGE.light);
    expect(paintFor("#fbd75b", "solid", { dark: true }).selectionGap).toBe("#000000");
  });

  it("has nothing to separate on an outlined block, and says so", () => {
    expect(paintFor("#f83a22", "outline", { dark: false }).selectionGap).toBe("var(--background)");
  });
});

describe("assigning colours across a set of calendars", () => {
  it("gives a calendar the colour its owner chose", () => {
    const colors = assignCalendarColors(
      [
        { id: "family", backgroundColor: "#d50000" },
        { id: "work", backgroundColor: "#0b8043" },
      ],
      FALLBACK_FILLS,
      assignHues,
    );
    expect(colors.get("family")).toBe("#d50000");
    expect(colors.get("work")).toBe("#0b8043");
  });

  it("keeps a calendar somebody deliberately made grey", () => {
    // The old code asked for a hue, got `null` for a neutral, and sent the
    // calendar to the hashed fallback — so a grey calendar came out red.
    const colors = assignCalendarColors(
      [{ id: "muted", backgroundColor: "#c2c2c2" }],
      FALLBACK_FILLS,
      assignHues,
    );
    expect(colors.get("muted")).toBe("#c2c2c2");
  });

  it("lets two calendars share a colour when the user gave them one", () => {
    // Second-guessing this would move a calendar off the colour it was set to,
    // which is a worse answer than two calendars looking alike — Google itself
    // draws them alike.
    const colors = assignCalendarColors(
      [
        { id: "a", backgroundColor: "#d50000" },
        { id: "b", backgroundColor: "#d50000" },
      ],
      FALLBACK_FILLS,
      assignHues,
    );
    expect(colors.get("a")).toBe(colors.get("b"));
  });

  it("falls back to the hashed ramp for a calendar with no colour", () => {
    const colors = assignCalendarColors([{ id: "team" }], FALLBACK_FILLS, assignHues);
    expect(colors.get("team")).toBe(FALLBACK_FILLS[assignHues(["team"]).get("team")!]);
  });

  it("treats an unreadable colour as no colour at all", () => {
    const colors = assignCalendarColors(
      [{ id: "broken", backgroundColor: "chartreuse" }],
      FALLBACK_FILLS,
      assignHues,
    );
    expect(colors.get("broken")).toBe(FALLBACK_FILLS[assignHues(["broken"]).get("broken")!]);
  });

  it("keeps an uncoloured calendar off a slot a coloured one has taken", () => {
    // The uncoloured one moves, never the coloured one: only one of the two is
    // a choice somebody made.
    const red = "#d50000";
    const slot = nearestRampSlot(red, FALLBACK_FILLS)!;
    const collides = [...Array(40).keys()]
      .map((n) => `cal-${n}`)
      .find((id) => assignHues([id]).get(id) === slot);
    expect(collides).toBeDefined();

    const colors = assignCalendarColors(
      [{ id: "chosen", backgroundColor: red }, { id: collides! }],
      FALLBACK_FILLS,
      assignHues,
    );
    expect(colors.get("chosen")).toBe(red);
    expect(colors.get(collides!)).not.toBe(FALLBACK_FILLS[slot]);
  });

  it("does not depend on the order sync happened to return calendars in", () => {
    const calendars = [
      { id: "b", backgroundColor: "#0b8043" },
      { id: "a" },
      { id: "c", backgroundColor: "#3f51b5" },
    ];
    const forwards = assignCalendarColors(calendars, FALLBACK_FILLS, assignHues);
    const backwards = assignCalendarColors([...calendars].reverse(), FALLBACK_FILLS, assignHues);
    expect([...forwards.entries()].sort()).toEqual([...backwards.entries()].sort());
  });

  it("measures hue distance around the wheel rather than along a number line", () => {
    // This deep magenta lands at 0.9°, which is nearer the ramp's 340° slot than
    // its 25° one — a metric subtracting raw angles would make it look 339° away.
    expect(oklchHue("#880044")).toBeLessThan(2);
    expect(nearestRampSlot("#880044", FALLBACK_FILLS)).toBe(7);
    expect(nearestRampSlot("#c2185b", FALLBACK_FILLS)).toBe(0);
    expect(nearestRampSlot("#9e9e9e", FALLBACK_FILLS)).toBeNull();
  });
});

describe("the page tokens this file duplicates", () => {
  /*
   * `PAGE` is a copy of `--background` from `globals.css`, because contrast is
   * arithmetic and a CSS variable is not a number until something paints. That
   * duplication is only safe if it is pinned, so this reads the stylesheet.
   */
  it("still matches globals.css", () => {
    const css = readFileSync(new URL("../styles/globals.css", import.meta.url), "utf8");
    const values = [...css.matchAll(/--background:\s*oklch\(([\d.]+)\s+([\d.]+)\s+([\d.]+)\)/g)];
    expect(values.length).toBeGreaterThanOrEqual(2);
    const asHex = values.map(([, l, c, h]) =>
      fromOklch({ l: Number(l), c: Number(c), h: Number(h) }),
    );
    expect(asHex).toContain(PAGE.light);
    expect(asHex).toContain(PAGE.dark);
  });
});
