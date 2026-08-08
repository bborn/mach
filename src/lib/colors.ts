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
 * Adopting the colour the user actually chose, without giving up contrast.
 *
 * Google hands back `backgroundColor` as an sRGB hex — the colour the user
 * picked in Google Calendar, and the only defensible answer to "what colour is
 * this calendar". Mach had never fetched it, so it assigned hues by hashing the
 * calendar id: stable, evenly spaced, and wrong. Somebody's red family calendar
 * came out cyan.
 *
 * But the hex cannot simply be painted. `calendar-palette.ts` holds every fill
 * at one lightness and one chroma on purpose — that uniformity is what stops a
 * week with nine calendars in it reading as confetti, and it is what guarantees
 * 4.5:1 against the white text drawn on top of every solid block, in both
 * themes. Google's palette has no such property: "Banana" (`#fbd75b`) is far too
 * light to carry white text, and "Grape" (`#8e24aa`) is dark enough to swallow
 * it in dark mode.
 *
 * So the split is: **hue is the user's, luminance is ours.** The hex is
 * converted to OKLCH, its hue read off, and the nearest slot on the app's
 * existing ramp taken — which keeps every calendar recognisably the colour its
 * owner chose while leaving lightness and chroma on the values that were tuned
 * for contrast. That is the "adjust luminance rather than discard the choice"
 * trade, made once, in one place, for both themes at once.
 *
 * The cost is honest and worth naming: two calendars whose colours differ only
 * in lightness — Google's "Blueberry" and "Lavender", say — land on the same
 * slot and look alike. That is already possible with eight hues and more than
 * eight calendars, and it is a far smaller problem than unreadable text.
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

/**
 * The OKLCH hue angle of an sRGB hex, in degrees, or `null`.
 *
 * OKLab rather than HSL because hue in HSL is not perceptual: HSL blue and HSL
 * green are nowhere near equally spaced from HSL cyan, so "nearest hue" computed
 * there picks visibly wrong neighbours. The matrices are Björn Ottosson's.
 *
 * A colour with no meaningful hue — a grey, a white, a black — returns `null`
 * rather than an arbitrary angle, because rounding grey to "orange" would be a
 * worse answer than falling back to the palette.
 */
export function oklchHue(hex: string): number | null {
  const rgb = linearRgb(hex);
  if (!rgb) return null;
  const [r, g, b] = rgb;

  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);

  const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const bb = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;

  // Below this the colour is a neutral and its hue is numerical noise.
  const chroma = Math.hypot(a, bb);
  if (chroma < 0.01) return null;

  const degrees = (Math.atan2(bb, a) * 180) / Math.PI;
  return (degrees + 360) % 360;
}

/** Shortest distance between two angles on the wheel, in degrees. */
function hueDistance(a: number, b: number): number {
  const raw = Math.abs(a - b) % 360;
  return raw > 180 ? 360 - raw : raw;
}

/**
 * The slot on `ramp` whose hue is closest to `hex`, or `null`.
 *
 * `ramp` is `CALENDAR_HUES`, passed in rather than imported so this file stays
 * a colour utility with no opinion about the calendar's palette — and so the
 * test can drive it with a ramp it controls.
 */
export function nearestRampSlot(hex: string, ramp: readonly number[]): number | null {
  const hue = oklchHue(hex);
  if (hue === null || ramp.length === 0) return null;
  let best = 0;
  for (let i = 1; i < ramp.length; i++) {
    if (hueDistance(hue, ramp[i]) < hueDistance(hue, ramp[best])) best = i;
  }
  return best;
}

/**
 * Hue slot per calendar: Google's colour where there is one, the hashed
 * fallback where there is not.
 *
 * Two rules, and the order matters. A calendar with a colour gets the slot that
 * colour maps to, *even if another calendar already has it* — the user chose
 * those two colours and if they chose the same one twice, showing them alike is
 * correct, and second-guessing it would move a calendar off the colour they
 * picked. A calendar with no colour then takes whatever is left, probing forward
 * from its hashed slot exactly as before, so adding a coloured calendar never
 * silently recolours an uncoloured one that was already on screen.
 *
 * `fallback` is `assignHues` from `calendar-palette.ts`, injected for the same
 * reason `ramp` is.
 */
export function assignCalendarHues(
  calendars: readonly { id: string; backgroundColor?: string }[],
  ramp: readonly number[],
  fallback: (ids: readonly string[]) => Map<string, number>,
): Map<string, number> {
  const out = new Map<string, number>();
  const taken = new Set<number>();
  const uncoloured: string[] = [];

  for (const calendar of [...calendars].sort((a, b) => a.id.localeCompare(b.id))) {
    const slot = calendar.backgroundColor
      ? nearestRampSlot(calendar.backgroundColor, ramp)
      : null;
    if (slot === null) {
      uncoloured.push(calendar.id);
      continue;
    }
    out.set(calendar.id, slot);
    taken.add(slot);
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
    out.set(id, chosen);
  }
  return out;
}
