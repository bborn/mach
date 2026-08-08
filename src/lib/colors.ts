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
