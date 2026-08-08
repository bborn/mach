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
 *   2. **Uniform lightness.** A palette looks like confetti when the colours
 *      vary in lightness, not when they vary in hue. Eight hues, evenly spaced,
 *      one lightness and one chroma for all of them.
 *
 * The lightness was chosen the way the brief says to choose it: nudged down
 * globally until every hue clears 4.5:1 against white text. At L=0.54 C=0.13
 * the worst hue (205°, cyan) is 4.53:1. Dark mode fills sit at L=0.50 against
 * near-white text, worst hue 4.87:1.
 *
 * These are the only literal colours in the calendar. Everything else —
 * surfaces, borders, ink, the selection ring — comes from the token layer in
 * `globals.css`, which has no eight-hue calendar ramp to borrow.
 */

/** Evenly spaced around the wheel. Eight is enough for the calendars one person actually keeps. */
export const CALENDAR_HUES = [25, 70, 115, 160, 205, 250, 295, 340] as const;

const LIGHT = { l: 0.54, c: 0.13 };
const DARK = { l: 0.5, c: 0.13 };

/** Text on a solid fill. Not `--foreground`: it must be light in both themes. */
const ON_FILL_LIGHT = "oklch(1 0 0)";
const ON_FILL_DARK = "oklch(0.97 0 0)";

export type HueIndex = number;

export function hueAt(index: HueIndex): number {
  return CALENDAR_HUES[((index % CALENDAR_HUES.length) + CALENDAR_HUES.length) % CALENDAR_HUES.length];
}

/** Solid fill for a hue index, in the given theme. */
export function calendarFill(index: HueIndex, dark: boolean): string {
  const { l, c } = dark ? DARK : LIGHT;
  return `oklch(${l} ${c} ${hueAt(index)})`;
}

/** The same hue as ink — used for the time and border of an unanswered invite. */
export function calendarInk(index: HueIndex, dark: boolean): string {
  const { c } = dark ? DARK : LIGHT;
  const l = dark ? 0.72 : 0.48;
  return `oklch(${l} ${c} ${hueAt(index)})`;
}

export function onFill(dark: boolean): string {
  return dark ? ON_FILL_DARK : ON_FILL_LIGHT;
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
export function assignHues(calendarIds: readonly string[]): Map<string, HueIndex> {
  const taken = new Set<number>();
  const out = new Map<string, HueIndex>();
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
 *   accepted / own   solid fill, white text
 *   unanswered       white fill, dark title, time and 1px border in the hue
 *   tentative        the unanswered treatment, plus a dashed border
 *   past             the same fill at 60% opacity, hue intact
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
}

export function paintFor(
  index: HueIndex,
  tone: EventTone,
  options: { dark: boolean; past?: boolean },
): BlockPaint {
  const { dark } = options;
  const fill = calendarFill(index, dark);
  const ink = calendarInk(index, dark);
  const opacity = options.past ? 0.6 : 1;

  if (tone === "solid") {
    return {
      background: fill,
      color: onFill(dark),
      timeColor: onFill(dark),
      opacity,
      strikethrough: false,
    };
  }

  return {
    background: "var(--background)",
    color: "var(--foreground)",
    border: ink,
    borderStyle: tone === "tentative" ? "dashed" : "solid",
    timeColor: ink,
    opacity: tone === "declined" ? Math.min(opacity, 0.7) : opacity,
    strikethrough: tone === "declined",
  };
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
