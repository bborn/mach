/**
 * Natural-language event entry (brief §3, path B).
 *
 * The grammar is Fantastical's, which is the most mature in the category and is
 * documented with worked examples. The date/time half is `chrono-node` (MIT) —
 * the brief is explicit that we do not write a date parser. Everything else is
 * token extraction around it:
 *
 *   at <place>       location, when the phrase is not a time
 *   with <names>     invitees
 *   alert <n> min    reminder offset
 *   every <interval> recurrence
 *   /<calname>       target calendar — `/w` → Work
 *
 * The order matters. Tokens chrono would misread (`alert 20 min`) come out
 * first. `every` is handled with two strings: chrono still sees "Tuesday at 6"
 * so it can resolve the weekday, while the title copy loses the whole phrase.
 * `with` and `at` are extracted *after* chrono's span has been cut out, which
 * is what makes "at Wegmans Thursday at 5pm" put Wegmans in the location and
 * Thursday in the date.
 *
 * Whatever is left over is the title. The caller renders the interpretation
 * under the field as you type, so Enter confirms something already visible.
 */

import * as chrono from "chrono-node";
import { DAY, MINUTE, clockTime, fullDate } from "./time";
import { DEFAULT_EVENT_MINUTES } from "./calendar-geometry";

export interface CalendarChoice {
  id: string;
  name: string;
}

export interface ParsedEvent {
  title: string;
  /** Epoch ms. `null` when the sentence carried no date at all. */
  start: number | null;
  end: number | null;
  allDay: boolean;
  location?: string;
  invitees: string[];
  alertMinutes?: number;
  /** The recurrence phrase, verbatim: "every Tuesday". Not yet expanded. */
  recurrence?: string;
  calendarId?: string;
  calendarName?: string;
  /** The `/token` that matched nothing, so the field can say so. */
  unknownCalendar?: string;
}

const STOP = "at|on|from|to|with|every|alert|in|for";

export function parseEventText(
  input: string,
  options: { now?: number; calendars?: readonly CalendarChoice[] } = {},
): ParsedEvent {
  const now = options.now ?? Date.now();
  let text = input.trim();

  const parsed: ParsedEvent = {
    title: "",
    start: null,
    end: null,
    allDay: false,
    invitees: [],
  };

  // 1. /calendar — one keystroke instead of a dropdown, which matters a lot
  //    when five accounts means choosing a target on every single event.
  text = text.replace(/(?:^|\s)\/([\w.-]+)/g, (_match, token: string) => {
    const hit = matchCalendar(token, options.calendars ?? []);
    if (hit) {
      parsed.calendarId = hit.id;
      parsed.calendarName = hit.name;
    } else {
      parsed.unknownCalendar = token;
    }
    return " ";
  });

  // 2. alert 20 min / alert 1 hour / alert 2 days
  text = text.replace(
    /\balert\s+(\d+)\s*(min(?:ute)?s?|hours?|hrs?|days?)\b/i,
    (_match, amount: string, unit: string) => {
      const n = Number(amount);
      const lower = unit.toLowerCase();
      parsed.alertMinutes = lower.startsWith("h")
        ? n * 60
        : lower.startsWith("d")
          ? n * 60 * 24
          : n;
      return " ";
    },
  );

  // 3. every … — recorded, then split in two: chrono keeps the interval text
  //    ("Tuesday at 6") so it can resolve the weekday; the title copy loses the
  //    whole phrase, so "every year on 5/16" does not leave "year" in the name.
  const recurrence = text.match(
    new RegExp(String.raw`\bevery\s+([\w' ]+?)(?=\s+(?:${STOP})\b|[,.]|$)`, "i"),
  );
  let forChrono = text;
  if (recurrence) {
    parsed.recurrence = `every ${recurrence[1].trim()}`.toLowerCase();
    forChrono = text.replace(/\bevery\s+/i, " ");
    text = text.replace(recurrence[0], " ");
  }

  // 4. chrono. `forwardDate` so a bare "Thursday" means the next one, which is
  //    what someone typing into a calendar means every single time.
  const result = chrono.parse(forChrono, new Date(now), { forwardDate: true })[0];
  if (result) {
    const timed = result.start.isCertain("hour");
    const shift = pmShift(result.start);
    const start = result.start.date().getTime() + shift;
    let end: number;
    if (result.end) {
      end = result.end.date().getTime() + (result.end.isCertain("hour") ? pmShift(result.end) : 0);
      // "August 9-18" means through the 18th, not up to its midnight.
      if (!result.end.isCertain("hour")) end += DAY;
    } else {
      end = timed ? start + DEFAULT_EVENT_MINUTES * MINUTE : start + DAY;
    }
    parsed.start = start;
    parsed.end = Math.max(end, start);
    parsed.allDay = !timed;
    text = removeFirst(text, result.text);
  }

  // 5. with <names>, up to the next grammar keyword. Runs after chrono so a
  //    date sitting behind the names cannot be mistaken for one of them.
  text = text.replace(
    new RegExp(String.raw`\bwith\s+([\w'.@-]+(?:\s+[\w'.@-]+)*?)(?=\s+(?:${STOP})\b|[,.]|\s*$)`, "i"),
    (_match, names: string) => {
      parsed.invitees = names
        .split(/\s*(?:,|\band\b|&)\s*/i)
        .map((name) => name.trim())
        .filter(Boolean);
      return " ";
    },
  );

  // 6. Whatever "at …" survived chrono is a place, not a time.
  text = text.replace(
    new RegExp(String.raw`\bat\s+([\w'#.,&-]+(?:\s+[\w'#.,&-]+)*?)(?=\s+(?:${STOP})\b|[,.]|\s*$)`, "i"),
    (_match, place: string) => {
      const trimmed = place.trim();
      if (trimmed) parsed.location = trimmed;
      return " ";
    },
  );

  parsed.title = tidy(text);
  return parsed;
}

/**
 * "Lunch at 1:30" means half past one in the afternoon. Nobody schedules a
 * standup at 01:30, and Fantastical assumes daylight too: an hour of 1–6 with
 * no am/pm said out loud gets twelve hours added.
 */
function pmShift(component: chrono.ParsedComponents): number {
  if (!component.isCertain("hour") || component.isCertain("meridiem")) return 0;
  const hour = component.get("hour") ?? 0;
  return hour >= 1 && hour <= 6 ? 12 * 60 * 60_000 : 0;
}

function removeFirst(haystack: string, needle: string): string {
  const at = haystack.toLowerCase().indexOf(needle.toLowerCase());
  if (at === -1) return haystack;
  return `${haystack.slice(0, at)} ${haystack.slice(at + needle.length)}`;
}

/** `/w` → Work. Prefix on the whole name first, then on any word in it. */
export function matchCalendar(
  token: string,
  calendars: readonly CalendarChoice[],
): CalendarChoice | undefined {
  const needle = token.toLowerCase();
  return (
    calendars.find((c) => c.name.toLowerCase().startsWith(needle)) ??
    calendars.find((c) =>
      c.name
        .toLowerCase()
        .split(/[\s@._-]+/)
        .some((word) => word.startsWith(needle)),
    ) ??
    calendars.find((c) => c.id.toLowerCase().startsWith(needle))
  );
}

/**
 * Cutting a date out of the middle of a sentence leaves prepositions behind —
 * "Family vacation from ⟨August 9-18⟩" becomes "Family vacation from". A title
 * should not end in a dangling "from", "on" or "at".
 */
function tidy(text: string): string {
  let out = text.replace(/\s+/g, " ").trim();
  const dangling = /\s+(?:at|on|from|to|in|for|by|of|the)$/i;
  while (dangling.test(out)) out = out.replace(dangling, "");
  return out.replace(/^[\s,;:-]+|[\s,;:-]+$/g, "").trim();
}

/**
 * The line under the field. It has to resolve as you type — the Enter is meant
 * to be a confirmation of something already on screen, not a leap of faith.
 */
export function describeParsed(parsed: ParsedEvent): string {
  const bits: string[] = [];
  if (parsed.start === null) {
    bits.push("no date yet");
  } else if (parsed.allDay) {
    const days = Math.round(((parsed.end ?? parsed.start) - parsed.start) / DAY);
    bits.push(days > 1 ? `${fullDate(parsed.start)} · ${days} days` : `${fullDate(parsed.start)} · all day`);
  } else {
    bits.push(`${fullDate(parsed.start)} · ${clockTime(parsed.start)} – ${clockTime(parsed.end ?? parsed.start)}`);
  }
  if (parsed.recurrence) bits.push(parsed.recurrence);
  if (parsed.location) bits.push(`at ${parsed.location}`);
  if (parsed.invitees.length > 0) bits.push(`with ${parsed.invitees.join(", ")}`);
  if (parsed.alertMinutes !== undefined) bits.push(`alert ${parsed.alertMinutes} min before`);
  if (parsed.calendarName) bits.push(parsed.calendarName);
  else if (parsed.unknownCalendar) bits.push(`no calendar matches /${parsed.unknownCalendar}`);
  return bits.join(" · ");
}
