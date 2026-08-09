/**
 * The calendar colour system (brief §6).
 *
 * Two rules, and they are the whole design:
 *
 *   1. **Hue encodes calendar identity, and nothing else.** Status — accepted,
 *      tentative, unanswered, past, declined — is carried by *fill treatment*,
 *      never by a second hue. Five accounts × several calendars is already
 *      15+ hues; spending one on "you haven't replied" turns the week into a
 *      bag of sweets.
 *   2. **The colour belongs to the calendar's owner.** Google returns
 *      `backgroundColor` for every calendar, and it is painted verbatim.
 *
 * Rule 2 replaced an earlier one — "uniform lightness", every fill held at
 * L 0.54 C 0.13 so that white text cleared 4.5:1 on all of them. That bought
 * its guarantee by throwing away the user's colours, and `colors.ts` has the
 * long version of why it is the wrong trade. The short version: the text is
 * ours to choose and the fill is not, so the *text* moves. Black or white,
 * whichever contrasts more, clears 4.5:1 on any colour in sRGB — a guarantee,
 * not a tuning.
 *
 * Everything else — surfaces, borders, ink, the selection ring — comes from the
 * token layer in `globals.css`, which has no calendar ramp to borrow.
 */

import {
  clampLightness,
  contrastRatio,
  fitLightness,
  fromOklch,
  isColor,
  readableInk,
} from "./colors";

/**
 * A calendar's colour: an sRGB hex, as Google gives it.
 *
 * A bare `string` rather than a branded type, because it crosses the IPC
 * boundary as a string and every consumer here re-checks it with `isColor`
 * anyway — a brand would only move the unchecked cast to the edge.
 */
export type CalendarColor = string;

/**
 * `--background` from `globals.css`, as hexes.
 *
 * Duplicated from the token layer, which is a cost worth naming: if those two
 * tokens ever change, these have to change with them. The alternative is a
 * `getComputedStyle` read per block per frame to discover a value that is a
 * constant of the theme, which trades a maintenance hazard for a layout
 * thrash. The test pins them against the tokens.
 */
export const PAGE = { light: "#ffffff", dark: "#0a0a0a" } as const;

/**
 * How far a fill may be lightened or darkened for the dark theme.
 *
 * The light theme does nothing at all: Google's colours were chosen against a
 * white page and they are painted on a white page, so fidelity is free.
 *
 * A near-black page is a different ground, and two things go wrong on it at the
 * ends of the range:
 *
 *   * **Below L 0.52 a fill stops being an object.** "Blueberry" (`#3f51b5`),
 *     "Grape" (`#8e24aa`) and "Graphite" (`#616161`) sit at 2.98–3.39:1 against
 *     `#0a0a0a` — under the 3:1 that WCAG 1.4.11 asks of a non-text element you
 *     are supposed to be able to see the boundary of. L 0.52 is the lowest
 *     lightness at which *every* hue and chroma clears 3:1 on this page; the
 *     worst of them (a saturated violet) lands at 3.11:1.
 *   * **Above L 0.78 a fill is a lamp.** "Banana" (`#fbd75b`) is 14:1 brighter
 *     than the page — where the loudest thing light mode ever draws is about
 *     5.5:1 — and in a dark room it blooms. The cap holds the brightest fill to
 *     ~10.5:1, which is still twice light mode's maximum; going further would
 *     start costing recognisability, which is the thing being protected.
 *
 * It is a clamp, not a remap. Hue and chroma are untouched; 25 of the 46
 * colours Google offers pass through unchanged, and across the whole palette
 * the average move is 0.026 of lightness — under three percent, which is below
 * the threshold at which anyone would call it a different colour. Only
 * "Banana" (0.148) and the two near-whites move enough to see. A calendar is
 * the same colour in both themes; the handful of extreme ones are the same
 * colour, turned down.
 *
 * The honest cost: two calendars whose colours differ *only* above the cap —
 * "Banana" and a paler yellow — converge in lightness in dark mode. They keep
 * their own hue and chroma, so they remain a yellow and a paler yellow, and it
 * is a far smaller problem than a block you cannot find.
 */
const DARK_BAND = { min: 0.52, max: 0.78 } as const;

/** Evenly spaced around the wheel. Eight is enough for the calendars one person actually keeps. */
export const CALENDAR_HUES = [25, 70, 115, 160, 205, 250, 295, 340] as const;

/**
 * The fallback ramp, for calendars Google gave no colour.
 *
 * L 0.54 C 0.13 are the values the old uniform-lightness palette used, kept
 * deliberately: they were tuned to sit well on a white page, they are inside
 * `DARK_BAND` so the dark theme leaves them alone, and keeping them means a
 * calendar with no colour looks today exactly as it looked yesterday. These are
 * now ordinary colours going through the ordinary pipeline, not a special case.
 */
export const FALLBACK_FILLS: readonly CalendarColor[] = CALENDAR_HUES.map((h) =>
  fromOklch({ l: 0.54, c: 0.13, h }),
);

/** Text on a fill must clear this. It always can — see `readableInk`. */
export const CONTRAST_TARGET = 4.5;

export function fallbackFill(index: number): CalendarColor {
  const length = FALLBACK_FILLS.length;
  return FALLBACK_FILLS[((index % length) + length) % length];
}

/**
 * The one place a calendar's colour becomes a pixel.
 *
 * The sidebar swatch, the month chip, the all-day bar, the event block and the
 * modal's dot all come through here, so a calendar cannot be one colour in the
 * rail and another in the grid.
 */
export function calendarFill(color: CalendarColor, dark: boolean): string {
  if (!isColor(color)) return fallbackFill(0);
  return dark ? clampLightness(color, DARK_BAND.min, DARK_BAND.max) : color;
}

/**
 * The same colour as *ink* — the time and the 1px border of an unanswered
 * invitation, which are drawn on the page rather than on a fill.
 *
 * Black-or-white is not available here: the whole point of the outlined
 * treatment is that the calendar's colour is what identifies it. So the colour
 * itself is moved along its own lightness axis, away from the page, until it
 * clears 4.5:1 — the least it can move and still be readable.
 */
export function calendarInk(color: CalendarColor, dark: boolean): string {
  if (!isColor(color)) return calendarInk(fallbackFill(0), dark);
  return fitLightness(color, dark ? PAGE.dark : PAGE.light, CONTRAST_TARGET);
}

/** The text colour for a fill, and the ratio it achieves. */
export function inkOn(fill: string): string {
  return readableInk(fill);
}

/** What `inkOn` actually measured — exported for the tests and for reporting. */
export function inkContrast(fill: string): number {
  return contrastRatio(fill, readableInk(fill)) ?? 1;
}

/* -------------------------------------------------------------------------- */
/* Stable assignment                                                           */
/* -------------------------------------------------------------------------- */

/** FNV-1a. Any stable 32-bit hash will do; this one is four lines. */
export function hashString(value: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < value.length; i++) {
    hash ^= value.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return hash >>> 0;
}

/**
 * Hue index per calendar, stable across sessions.
 *
 * Hashed rather than assigned by load order, so a calendar keeps its colour
 * when another account is added or sync returns things in a different order.
 * Collisions are resolved by probing forward from the hashed slot, walking the
 * ids in sorted order so the result does not depend on the input's order
 * either. With more calendars than hues, hues repeat — which is honest, and
 * better than 15 near-identical colours.
 */
export function assignHues(calendarIds: readonly string[]): Map<string, number> {
  const taken = new Set<number>();
  const out = new Map<string, number>();
  for (const id of [...calendarIds].sort()) {
    const start = hashString(id) % CALENDAR_HUES.length;
    let slot = start;
    for (let step = 0; step < CALENDAR_HUES.length; step++) {
      const candidate = (start + step) % CALENDAR_HUES.length;
      if (!taken.has(candidate)) {
        slot = candidate;
        break;
      }
    }
    taken.add(slot);
    out.set(id, slot);
  }
  return out;
}

/* -------------------------------------------------------------------------- */
/* Fill treatments (§6)                                                        */
/* -------------------------------------------------------------------------- */

/**
 * What a block looks like, by state. Copied from Google, which gets this right:
 *
 *   accepted / own   solid fill, text in whichever of black and white reads
 *   unanswered       page-coloured fill, dark title, time and 1px border in the
 *                    calendar's colour
 *   tentative        the unanswered treatment, plus a dashed border
 *   past             the same fill at 60% opacity, colour intact
 *   declined         outlined, title struck through (hidden by default)
 */
export type EventTone = "solid" | "outline" | "tentative" | "declined";

export interface BlockPaint {
  background: string;
  color: string;
  /** `undefined` means no border at all — solid blocks have none. */
  border?: string;
  borderStyle?: "solid" | "dashed";
  /** Colour for the time span, which stays hue-coloured on outlined blocks. */
  timeColor: string;
  opacity: number;
  strikethrough: boolean;
  /**
   * The 2px band the selection cursor draws immediately outside the block.
   *
   * On a solid block this is the block's own ink, which is guaranteed 4.5:1
   * against the fill by construction — see the cursor's comment in
   * `EventBlock.tsx` for why that guarantee is the load-bearing part.
   */
  selectionGap: string;
}

/**
 * Paint is pure and the inputs are a handful of colours, so it is memoised.
 *
 * Every block in the week calls this on every render, and each call can run two
 * gamut-mapping bisections. The key space is bounded by (calendars × tones × 2
 * themes × 2), so the map cannot grow with the number of events.
 */
const paintCache = new Map<string, BlockPaint>();

export function paintFor(
  color: CalendarColor,
  tone: EventTone,
  options: { dark: boolean; past?: boolean },
): BlockPaint {
  const { dark } = options;
  const past = options.past ?? false;
  const key = `${color}|${tone}|${dark ? "d" : "l"}|${past ? "p" : "n"}`;
  const hit = paintCache.get(key);
  if (hit) return hit;

  const opacity = past ? 0.6 : 1;
  let paint: BlockPaint;

  if (tone === "solid") {
    const fill = calendarFill(color, dark);
    const ink = inkOn(fill);
    paint = {
      background: fill,
      color: ink,
      timeColor: ink,
      opacity,
      strikethrough: false,
      selectionGap: ink,
    };
  } else {
    const ink = calendarInk(color, dark);
    paint = {
      background: "var(--background)",
      color: "var(--foreground)",
      border: ink,
      borderStyle: tone === "tentative" ? "dashed" : "solid",
      timeColor: ink,
      opacity: tone === "declined" ? Math.min(opacity, 0.7) : opacity,
      strikethrough: tone === "declined",
      // An outlined block *is* the page, so there is nothing for a gap to
      // separate it from; the accent band does the whole job.
      selectionGap: "var(--background)",
    };
  }

  paintCache.set(key, paint);
  return paint;
}

/** Map an RSVP (and whether the event has already happened) onto a tone. */
export function toneFor(rsvp: string | undefined): EventTone {
  switch (rsvp) {
    case "needsAction":
      return "outline";
    case "tentative":
      return "tentative";
    case "declined":
      return "declined";
    default:
      // No RSVP at all means it is not an invitation: you own it.
      return "solid";
  }
}
