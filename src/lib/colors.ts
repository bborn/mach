/**
 * The per-account colour ramp, resolved to real class names.
 *
 * Written out longhand rather than interpolated, because Tailwind v4 scans
 * source text for class names and `bg-account-${n}` would never be emitted.
 */

import type { ColorIndex } from "@/types";

export const ACCOUNT_BG: Record<ColorIndex, string> = {
  1: "bg-account-1",
  2: "bg-account-2",
  3: "bg-account-3",
  4: "bg-account-4",
  5: "bg-account-5",
};

export const ACCOUNT_TEXT: Record<ColorIndex, string> = {
  1: "text-account-1",
  2: "text-account-2",
  3: "text-account-3",
  4: "text-account-4",
  5: "text-account-5",
};

/** For inline styles — borders, `color-mix` tints, the calendar event fill. */
export function accountVar(index: ColorIndex | number): string {
  const clamped = Math.min(5, Math.max(1, Math.round(index)));
  return `var(--account-${clamped})`;
}

/**
 * A calendar block's fill: the account hue mixed down into the page surface,
 * so it tints correctly in both themes off a single token.
 */
export function tintedSurface(index: ColorIndex | number, percent = 16): string {
  return `color-mix(in oklab, ${accountVar(index)} ${percent}%, var(--background))`;
}

/* -------------------------------------------------------------------------- */
/* Google's calendar colours                                                   */
/* -------------------------------------------------------------------------- */

/**
 * Painting the colour the user actually chose.
 *
 * Google hands back `backgroundColor` as an sRGB hex — the colour the user
 * picked in Google Calendar, and the only defensible answer to "what colour is
 * this calendar". It is used here verbatim.
 *
 * It was not always. The previous version of this file converted the hex to
 * OKLCH, read off *only the hue angle*, snapped it to the nearest of eight
 * palette slots, and kept Mach's own lightness and chroma. The argument was
 * contrast, and it was a real argument: white text on a solid block needs
 * 4.5:1, "Banana" (`#fbd75b`) cannot carry white text at any size, and holding
 * every fill at one lightness guaranteed the ratio in one stroke for every hue
 * in both themes.
 *
 * The argument is sound and the conclusion was wrong, for a reason worth
 * writing down because it is easy to re-derive the old answer:
 *
 * **The text colour was treated as a constant and the fill as the free
 * variable. It is the other way round.** The fill is the user's — it is the one
 * thing on this surface that is not ours to choose. The text is ours, nobody
 * has an opinion about it, and there are two of it. So pick the text to suit
 * the fill, which is exactly what Google Calendar itself does, and the contrast
 * problem disappears without anybody's calendar changing colour.
 *
 * And it does disappear, completely, not approximately. For any sRGB colour,
 * the better of black and white clears **4.5:1** — the worst case in the whole
 * cube is a violet-blue around `#5d60ff`, where black and white are equally bad
 * and both still reach 4.58:1. That is a property of the WCAG contrast formula,
 * not a fact about Google's palette: the two ratios cross at relative luminance
 * 0.179, and the crossing point *is* the minimum. So "choose the better of
 * black and white" is a guarantee, where "hold every fill at L 0.54" was a
 * tuning exercise that had to be redone whenever the ramp moved.
 *
 * What was lost with the old scheme was the thing the user was looking at:
 * seven vivid, distinct calendar colours arrived, were reduced to eight hue
 * buckets at one lightness, and came out as a narrow band of muted pinks and
 * purples in which several visually different calendars were hard to tell
 * apart.
 *
 * The hue-snapping machinery below survives, demoted to the one job it is
 * actually right for: keeping two *uncoloured* calendars, which fall back to a
 * hashed slot on Mach's own ramp, from landing on a colour a real calendar is
 * already using.
 */

/** sRGB hex → linear-light RGB, or `null` when it is not a colour. */
function linearRgb(hex: string): [number, number, number] | null {
  const match = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(hex.trim());
  if (!match) return null;
  const digits = match[1];
  const full =
    digits.length === 3
      ? digits
          .split("")
          .map((d) => d + d)
          .join("")
      : digits;
  const channels = [0, 2, 4].map((i) => parseInt(full.slice(i, i + 2), 16) / 255);
  // The sRGB transfer function, not a 2.2 gamma approximation: the difference
  // shows up in the near-blacks, which is where several of Google's darker
  // calendar colours live.
  const toLinear = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  return [toLinear(channels[0]), toLinear(channels[1]), toLinear(channels[2])];
}

/** Linear-light channel → 8-bit sRGB, clamped. */
function encodeChannel(value: number): string {
  const clamped = Math.min(1, Math.max(0, value));
  const gamma = clamped <= 0.0031308 ? 12.92 * clamped : 1.055 * clamped ** (1 / 2.4) - 0.055;
  return Math.round(gamma * 255)
    .toString(16)
    .padStart(2, "0");
}

/** Whether a hex string is something this module can do arithmetic on. */
export function isColor(hex: string | undefined): hex is string {
  return hex !== undefined && linearRgb(hex) !== null;
}

/**
 * WCAG relative luminance, or `null` when the string is not a colour.
 *
 * This is the *photometric* quantity — the Y of CIE XYZ — and it is deliberately
 * not OKLCH's L. Perceptual lightness is the right axis for "make this a bit
 * darker"; the contrast ratio is defined on luminance, and using anything else
 * would produce numbers that look plausible and do not mean 4.5:1.
 */
export function relativeLuminance(hex: string): number | null {
  const rgb = linearRgb(hex);
  if (!rgb) return null;
  return 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
}

/** WCAG 2.x contrast ratio between two colours, 1…21, or `null`. */
export function contrastRatio(a: string, b: string): number | null {
  const la = relativeLuminance(a);
  const lb = relativeLuminance(b);
  if (la === null || lb === null) return null;
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

export const INK_DARK = "#000000";
export const INK_LIGHT = "#ffffff";

/**
 * The text colour for a fill: black or white, whichever contrasts more.
 *
 * Pure black and pure white, not near-black and near-white. The 4.58:1
 * worst-case guarantee described above is a property of the *extremes*; softening
 * the light ink to `oklch(0.97 0 0)`, which is what this palette used to draw on
 * solid blocks, drops the guaranteed floor to 4.39:1 and quietly loses the whole
 * argument for two percent less glare.
 *
 * Google's stored `foregroundColor` is not consulted, and must not be: every row
 * in a real store reads `#000000`, including on fills where black is the wrong
 * answer. It is a legacy field from the old 24-colour API that Google itself
 * stopped populating meaningfully.
 */
export function readableInk(fill: string): string {
  const onWhite = contrastRatio(fill, INK_LIGHT);
  const onBlack = contrastRatio(fill, INK_DARK);
  if (onWhite === null || onBlack === null) return INK_DARK;
  return onWhite >= onBlack ? INK_LIGHT : INK_DARK;
}

/* -------------------------------------------------------------------------- */
/* OKLCH, for the adjustments that have to preserve identity                    */
/* -------------------------------------------------------------------------- */

export interface Oklch {
  l: number;
  c: number;
  h: number;
}

/**
 * sRGB hex → OKLCH.
 *
 * OKLab rather than HSL because neither hue nor lightness is perceptual in HSL:
 * HSL blue and HSL green are nowhere near equally spaced from HSL cyan, and HSL
 * "lightness" 50% is a different apparent brightness for every hue. Every
 * adjustment in this file moves lightness while holding hue and chroma, which
 * is only a meaning-preserving operation in a perceptual space. The matrices
 * are Björn Ottosson's.
 */
export function toOklch(hex: string): Oklch | null {
  const rgb = linearRgb(hex);
  if (!rgb) return null;
  const [r, g, b] = rgb;

  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);

  const lightness = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
  const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const bb = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;

  const chroma = Math.hypot(a, bb);
  const degrees = (Math.atan2(bb, a) * 180) / Math.PI;
  return { l: lightness, c: chroma, h: (degrees + 360) % 360 };
}

function oklabToLinear(l: number, a: number, b: number): [number, number, number] {
  const l_ = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m_ = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s_ = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
    -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
    -0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_,
  ];
}

function linearFor(color: Oklch, chroma: number): [number, number, number] {
  const radians = (color.h * Math.PI) / 180;
  return oklabToLinear(color.l, chroma * Math.cos(radians), chroma * Math.sin(radians));
}

const GAMUT_SLACK = 1 / 512;

function inGamut(rgb: readonly number[]): boolean {
  return rgb.every((channel) => channel >= -GAMUT_SLACK && channel <= 1 + GAMUT_SLACK);
}

/**
 * OKLCH → sRGB hex, reducing chroma until the colour fits.
 *
 * Clipping the channels instead would be simpler and would shift the hue: an
 * out-of-gamut orange clipped to `#ff...` comes back yellower. Holding hue and
 * lightness and giving up saturation is what CSS Color 4 gamut mapping does,
 * and it is the right trade here because hue is the part carrying identity.
 */
export function fromOklch(color: Oklch): string {
  let chroma = Math.max(0, color.c);
  if (!inGamut(linearFor(color, chroma))) {
    let low = 0;
    let high = chroma;
    for (let i = 0; i < 20; i++) {
      const mid = (low + high) / 2;
      if (inGamut(linearFor(color, mid))) low = mid;
      else high = mid;
    }
    chroma = low;
  }
  return `#${linearFor(color, chroma).map(encodeChannel).join("")}`;
}

/** The same colour at a different perceptual lightness. */
export function withLightness(hex: string, lightness: number): string {
  const base = toOklch(hex);
  if (!base) return hex;
  return fromOklch({ ...base, l: Math.min(1, Math.max(0, lightness)) });
}

/** The same colour, with its lightness pulled into `[min, max]`. */
export function clampLightness(hex: string, min: number, max: number): string {
  const base = toOklch(hex);
  if (!base) return hex;
  const clamped = Math.min(max, Math.max(min, base.l));
  return clamped === base.l ? hex : fromOklch({ ...base, l: clamped });
}

/**
 * The nearest lightness at which `hex` clears `target` against `page`.
 *
 * Used for colour drawn *as ink* — the 1px border and the time on an outlined
 * block, where the calendar's colour is the text rather than the ground behind
 * it and black-or-white is not available. Hue and chroma are held; the search
 * moves away from the page, so a colour already dark enough on a white page is
 * returned untouched rather than darkened for tidiness.
 */
export function fitLightness(hex: string, page: string, target: number): string {
  const base = toOklch(hex);
  const here = contrastRatio(hex, page);
  const pageLuminance = relativeLuminance(page);
  if (!base || here === null || pageLuminance === null) return hex;
  if (here >= target) return hex;

  // The passing end of the interval, and the end we are bisecting towards.
  let passing = pageLuminance > 0.18 ? 0 : 1;
  let failing = base.l;
  for (let i = 0; i < 24; i++) {
    const mid = (passing + failing) / 2;
    const candidate = fromOklch({ ...base, l: mid });
    const ratio = contrastRatio(candidate, page);
    if (ratio !== null && ratio >= target) passing = mid;
    else failing = mid;
  }
  return fromOklch({ ...base, l: passing });
}

/* -------------------------------------------------------------------------- */
/* Falling back when a calendar has no colour at all                            */
/* -------------------------------------------------------------------------- */

/**
 * The OKLCH hue angle of an sRGB hex, in degrees, or `null`.
 *
 * A colour with no meaningful hue — a grey, a white, a black — returns `null`
 * rather than an arbitrary angle. Note what this is *not* used for any more: a
 * grey calendar is drawn grey, exactly as its owner set it. Returning `null`
 * here only means "this colour cannot claim a slot on the hashed ramp", which
 * is true, because the ramp is eight hues and grey is none of them.
 */
export function oklchHue(hex: string): number | null {
  const color = toOklch(hex);
  // Below this the colour is a neutral and its hue is numerical noise.
  if (!color || color.c < 0.01) return null;
  return color.h;
}

/** Shortest distance between two angles on the wheel, in degrees. */
function hueDistance(a: number, b: number): number {
  const raw = Math.abs(a - b) % 360;
  return raw > 180 ? 360 - raw : raw;
}

/**
 * The slot on `ramp` whose hue is closest to `hex`, or `null`.
 *
 * `ramp` is `FALLBACK_FILLS`, passed in rather than imported so this file stays
 * a colour utility with no opinion about the calendar's palette — and so the
 * test can drive it with a ramp it controls.
 */
export function nearestRampSlot(hex: string, ramp: readonly string[]): number | null {
  const hue = oklchHue(hex);
  if (hue === null || ramp.length === 0) return null;
  const hues = ramp.map(oklchHue);
  let best: number | null = null;
  for (let i = 0; i < hues.length; i++) {
    const candidate = hues[i];
    if (candidate === null) continue;
    if (best === null || hueDistance(hue, candidate) < hueDistance(hue, hues[best]!)) best = i;
  }
  return best;
}

/**
 * Fill per calendar: Google's colour where there is one, the hashed ramp where
 * there is not.
 *
 * Two rules, and the order matters. A calendar with a colour gets that colour,
 * verbatim, *even if another calendar has the same one* — the user chose both,
 * and if they chose the same one twice, showing them alike is correct; Google
 * draws them alike too. A calendar with no colour then takes whatever hue slot
 * is left, probing forward from its hashed slot, so adding a coloured calendar
 * never silently recolours an uncoloured one that was already on screen.
 *
 * "Has a colour" means the string parses, not that it has a hue: `#c2c2c2` is a
 * calendar somebody deliberately made grey, and the old code sent it to the
 * fallback and gave it a random hue.
 *
 * `fallback` is `assignHues` from `calendar-palette.ts`, injected for the same
 * reason `ramp` is.
 *
 * Room for a local override, if it is ever wanted: this takes `backgroundColor`
 * per calendar rather than reading the store, so "let me set colours myself"
 * is a stored `Record<CalendarId, string>` merged over `calendars` at the one
 * call site, and nothing below this function changes. It is deliberately not
 * built yet — Google already answers the question, and a second source of truth
 * for a calendar's colour would have to be reconciled every time the user
 * changes it in Google.
 */
export function assignCalendarColors(
  calendars: readonly { id: string; backgroundColor?: string }[],
  ramp: readonly string[],
  fallback: (ids: readonly string[]) => Map<string, number>,
): Map<string, string> {
  const out = new Map<string, string>();
  const taken = new Set<number>();
  const uncoloured: string[] = [];

  for (const calendar of [...calendars].sort((a, b) => a.id.localeCompare(b.id))) {
    if (!isColor(calendar.backgroundColor)) {
      uncoloured.push(calendar.id);
      continue;
    }
    out.set(calendar.id, calendar.backgroundColor);
    // Only so the hashed fallback can steer clear of it. A grey or a colour
    // whose hue is nowhere near the ramp simply claims nothing.
    const slot = nearestRampSlot(calendar.backgroundColor, ramp);
    if (slot !== null) taken.add(slot);
  }

  for (const [id, slot] of fallback(uncoloured)) {
    let chosen = slot;
    for (let step = 0; step < ramp.length; step++) {
      const candidate = (slot + step) % ramp.length;
      if (!taken.has(candidate)) {
        chosen = candidate;
        break;
      }
    }
    taken.add(chosen);
    out.set(id, ramp[chosen % ramp.length]);
  }
  return out;
}
