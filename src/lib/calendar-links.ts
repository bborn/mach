/**
 * The links an event detail needs, and the rules for following one.
 *
 * `conferenceData` now *does* reach the UI — migration 7 gave it a column and
 * `mapEvent` carries it — so the text scanning below is no longer the only
 * source of a call. It is still the fallback, and it earns its place: Zoom,
 * Teams and Webex are pasted into a description or a location and never appear
 * in `conferenceData` at all, because Google only mints a conference for Meet.
 *
 * Everything a call brings with it — the URL, the phone number, the label — is
 * a string an attacker chose. An invitation is an unauthenticated write into
 * this store from anyone who knows the user's address, so a link is only ever
 * *followed* through `joinUrl` or `dialUrl`, which are the two functions in the
 * app allowed to say a conference URI is safe to open.
 */

import type { CalendarEvent, ConferenceEntry } from "@/types";

/**
 * The video call on an event, if it has one.
 *
 * `conferenceData` is checked first now that it is stored: it is Google's own
 * structured answer, it survives a description being edited, and it is the only
 * one of the three sources that also knows the meeting code and the dial-in.
 *
 * The text scan stays behind it, unchanged, for every call Google did not mint.
 * There, ordering matters: `location` is checked before `description` because
 * when a link is in the location it is the one the organiser meant, while a
 * description can also quote a *past* meeting's link in a reply chain.
 *
 * A URI that does not survive `joinUrl` is skipped rather than shown broken —
 * an entry point Mach will not open must not be offered as a button.
 */
export function conferenceLink(event: CalendarEvent): ConferenceLink | null {
  const video = event.conference?.entryPoints.find((entry) => entry.kind === "video");
  const stored = video && joinUrl(video.uri);
  if (video && stored) {
    return { provider: event.conference?.name ?? "the call", url: stored };
  }

  for (const field of [event.location, event.description]) {
    if (!field) continue;
    for (const pattern of PROVIDERS) {
      const match = field.match(pattern.re);
      if (match) {
        const url = joinUrl(normalise(match[0]));
        if (url) return { provider: pattern.name, url };
      }
    }
  }
  return null;
}

/**
 * A conference URI, if it is one this app will open.
 *
 * The check is a whitelist of shape, not of vendor. Mach cannot know every
 * conferencing host — the owner's calendar has five providers on it — so the
 * rule is about what a link may *be*, and the four things it says no to are the
 * four ways this could go wrong:
 *
 *  * **`https` only.** `javascript:` and `data:` are the obvious attacks;
 *    plain `http` is refused because a meeting link that is not confidential in
 *    transit is not a meeting link worth defending, and no real provider uses
 *    one.
 *  * **No credentials.** `https://meet.google.com@evil.example/` reads as Meet
 *    to a human and resolves to `evil.example` in a browser. This is the exact
 *    shape a hostile invitation would use, since the label beside the button is
 *    also attacker-controlled.
 *  * **A dotted host name.** Not a bare label (`https://intranet`), not an IP
 *    literal — the last label must contain a letter, which is what rules out
 *    `https://1.2.3.4/join` and its IPv6 equivalent.
 *  * **Nothing else re-encoded.** The URL is returned as `URL` parsed it, so
 *    what gets opened is what was checked, not a second parse of the raw text.
 *
 * `open_external` on the Rust side checks the scheme again. That is deliberate
 * duplication: this check is for the interface, and anything running in the
 * webview can call the command directly.
 */
export function joinUrl(raw: string | null | undefined): string | null {
  if (!raw) return null;
  let url: URL;
  try {
    url = new URL(raw.trim());
  } catch {
    return null;
  }
  if (url.protocol !== "https:") return null;
  if (url.username || url.password) return null;

  const host = url.hostname.toLowerCase();
  if (!HOST_SHAPE.test(host)) return null;
  const tld = host.slice(host.lastIndexOf(".") + 1);
  if (!/[a-z]/.test(tld)) return null;
  return url.toString();
}

const HOST_SHAPE = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/;

/**
 * A dial-in URI, if it is one this app will open.
 *
 * `tel:` is on `open_external`'s allowed list, so the same rule applies as
 * above: a number is only offered as a link when it is unambiguously a number.
 * Digits, spacing punctuation, and the `,` `#` `*` that a dial string uses to
 * pause and to enter a PIN — nothing else, which is what stops a URI that
 * merely starts with `tel:` from carrying anything after it.
 */
export function dialUrl(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const trimmed = raw.trim();
  return /^tel:\+?[0-9 ,.()#*-]{3,}$/i.test(trimmed) ? trimmed : null;
}

/**
 * What to show for an entry point: its own label, or the URI made readable.
 *
 * A Meet video URI reads `https://meet.google.com/abc-defg-hij` and Google's
 * label reads `meet.google.com/abc-defg-hij`; when there is no label, stripping
 * the scheme is what closes that gap. `tel:` becomes the number.
 */
export function entryLabel(entry: ConferenceEntry): string {
  if (entry.label) return entry.label;
  return entry.uri.replace(/^https:\/\//i, "").replace(/^tel:/i, "");
}

export interface ConferenceLink {
  provider: string;
  url: string;
}

/* -------------------------------------------------------------------------- */
/* Where it is, as somewhere you can be shown                                  */
/* -------------------------------------------------------------------------- */

/**
 * A cap on how much text will be carried into a maps query.
 *
 * Google's `location` is a free-text field of about a kilobyte and people paste
 * whole paragraphs into it — parking instructions, a door code, the agenda.
 * None of that is an address, a query that long returns nothing, and the number
 * is here so the refusal is a stated rule rather than a surprise further down.
 */
const MAX_ADDRESS_CHARS = 300;

/**
 * The shapes that say "this is a link, not a place".
 *
 * Checked first and checked hard, because the two ways this feature could go
 * wrong both live here. A conference URL in `location` is the common case and
 * already has a button — see `conferenceLink` — so it must not also grow a map.
 * And a location is attacker-controlled text, so anything that even *reads* as a
 * URL is refused rather than reasoned about: `javascript:`, `data:`, `file:`, a
 * custom scheme nobody has heard of, a bare host. None of them is ever parsed,
 * followed, or repaired.
 */
const URL_SHAPED: RegExp[] = [
  // Any scheme with an authority, anywhere in the string.
  /[a-z][a-z0-9+.-]*:\/\//i,
  // A scheme the string *opens* with — `javascript:`, `mailto:`, `tel:`,
  // `zoommtg:`. Anchored, so "Room 2: the annex" is still a room.
  /^[a-z][a-z0-9+.-]*:/i,
  // A bare host: `meet.google.com/abc-defg-hij`, `zoom.us`. It has to end the
  // token or carry a path, which is what keeps "St. Paul, MN" a city.
  /(?:^|\s)(?:[a-z0-9-]+\.)+[a-z]{2,}(?:[/?#]\S*)?(?=\s|$)/i,
];

/**
 * A street line: a number, then up to four words, then a word that names a kind
 * of street.
 *
 * This is the rule that carries almost every real hit — "1317 Marshall St NE",
 * "10 Downing Street", "1 Rue de l'Église" — and it is also the rule that
 * refuses "Room 2", which has the number and nothing after it. The suffix list
 * is anglophone plus the handful of French and Spanish words that lead an
 * address rather than end one, since those land in the same position relative
 * to the number.
 */
const STREET_SUFFIX = [
  "street", "st", "avenue", "ave", "av", "road", "rd", "boulevard", "blvd",
  "lane", "ln", "drive", "dr", "court", "ct", "place", "pl", "way", "terrace",
  "ter", "highway", "hwy", "parkway", "pkwy", "circle", "cir", "square", "sq",
  "trail", "trl", "loop", "row", "walk", "alley", "close", "crescent", "cres",
  "quay", "wharf", "mews", "esplanade", "promenade", "rue", "avenida", "calle",
  "plaza", "camino", "via", "viale", "corso",
].join("|");

const STREET_LINE = new RegExp(
  String.raw`\b\d{1,6}[a-z]?\b(?: +[\w'’.-]+){0,4} +(?:${STREET_SUFFIX})\b`,
  "i",
);

/**
 * The German-shaped compound street, where the number comes second.
 *
 * "Bahnhofstrasse 10" is one word plus a number, so `STREET_LINE` cannot see
 * it — the number is on the wrong side and there is no separate suffix token.
 * A word *ending* in one of these is unambiguous enough to stand on its own.
 */
const COMPOUND_STREET =
  /\p{L}{3,}(?:stra(?:ss|ß)e|gasse|platz|weg|allee|straat|laan|gatan|gata|gade|vej)\b\.? *\d{1,5}\b/iu;

/**
 * A postal code, in the three forms distinctive enough to mean one.
 *
 * A bare run of digits is not on this list on purpose. "8001 Zürich" and "2026
 * Roadmap" are the same string to a regular expression, and a map offered for
 * the second is the failure this predicate exists to avoid — so a plain
 * four-digit European postcode is a miss unless the street carries the hit.
 */
const POSTAL_CODE: RegExp[] = [
  // US, in its canonical form: the two-letter state is what makes it one.
  /\b[A-Z]{2},? \d{5}(?:-\d{4})?\b/,
  // UK: "SW1A 2AA", "M1 1AE".
  /\b[A-Z]{1,2}\d[A-Z\d]? ?\d[A-Z]{2}\b/i,
  // Canada: "K1A 0B1".
  /\b[A-Z]\d[A-Z] ?\d[A-Z]\d\b/i,
];

/**
 * Is this location somewhere a map could show?
 *
 * A `location` carries whatever the organiser typed: a street address, "Room
 * 2", a Zoom link, a person's name, nothing at all. Only the first of those is
 * worth a map, and the brief this was written to is explicit that offering
 * directions to "Room 2" is worse than offering nothing — so the rule leans
 * towards precision, and the misses it takes are listed below rather than
 * papered over.
 *
 * **What it gets right.** A street address in the anglophone order, with or
 * without a venue name in front of it and with or without a city, region and
 * postcode after it. A German-shaped compound street. A US, UK or Canadian
 * postcode anywhere in the string.
 *
 * **What it misses.** A bare place name with no street in it — "Cafe Lurcat",
 * "The Walker" — which is a real address to a human and to Google Maps, and
 * nothing at all to a regular expression. A European address whose street has
 * no suffix and whose postcode is a plain run of digits. An address with a URL
 * next to it, which is refused whole, because a location that mentions a link
 * might *be* a link.
 *
 * **What it gets wrong.** Any string with a US/UK/Canadian postcode shape in it
 * is an address as far as this is concerned, and so is anything with a number
 * followed by a word on the suffix list — "Suite 4 Way" would qualify. Both are
 * a wasted click into an empty Maps search rather than anything worse.
 *
 * Pure and exported so it can be tuned against real locations without a
 * component in the way.
 */
export function looksLikeAddress(location: string | null | undefined): boolean {
  if (!location) return false;
  const text = location.trim();
  if (text.length === 0 || text.length > MAX_ADDRESS_CHARS) return false;
  if (URL_SHAPED.some((re) => re.test(text))) return false;
  if (STREET_LINE.test(text)) return true;
  if (COMPOUND_STREET.test(text)) return true;
  return POSTAL_CODE.some((re) => re.test(text));
}

/**
 * The maps origin, written out once and never assembled from anything.
 *
 * Google Maps rather than Apple Maps, for two reasons that point the same way.
 * The addresses on these events were geocoded by Google and are formatted the
 * way Google formats them, so Google's own search resolves them and a second
 * geocoder is a second chance to fail. And Apple Maps opens natively from a
 * `maps:` URL, which would mean widening `OPENABLE_SCHEMES` in
 * `ipc/render.rs` — a four-scheme allow-list whose whole value is that it is
 * four schemes long — to save a click. Its `https://maps.apple.com` form has no
 * such cost and also no such benefit: it opens in the browser like this one.
 *
 * A map, not directions. Directions need an origin, and the app does not know
 * where he is and should not be asking for permission to find out. The search
 * page lands on the pin with a Directions button on it, and that page *does*
 * know where he is, so the one thing left to choose is left where it can be
 * answered properly.
 */
const MAPS_SEARCH = "https://www.google.com/maps/search/?api=1&query=";

/**
 * A map of this location, if this location is a place.
 *
 * The location is **never treated as a URL**, even when it looks like one — it
 * is a query parameter on a fixed `https://` origin and nothing else. The
 * origin is a literal, the only variable part goes through
 * `encodeURIComponent`, and that function cannot emit `:`, `/`, `?`, `#` or
 * `&`. So no location, however hostile, can move the host, add a path, escape
 * the query string, or change the scheme: the worst a `javascript:` payload can
 * do is be a Google Maps search that finds nothing. The predicate refuses it
 * first anyway; the encoding is the second lock on the same door.
 */
export function mapsUrl(location: string | null | undefined): string | null {
  if (!looksLikeAddress(location)) return null;
  return MAPS_SEARCH + encodeURIComponent(location!.trim());
}

const PROVIDERS: { name: string; re: RegExp }[] = [
  { name: "Google Meet", re: /https?:\/\/meet\.google\.com\/[a-z0-9-]+/i },
  { name: "Zoom", re: /https?:\/\/[\w.-]*zoom\.us\/j\/\d+(\?[^\s<>"]*)?/i },
  {
    name: "Microsoft Teams",
    re: /https?:\/\/teams\.microsoft\.com\/l\/meetup-join\/[^\s<>"]+/i,
  },
  { name: "Webex", re: /https?:\/\/[\w.-]*webex\.com\/[^\s<>"]+/i },
  { name: "Whereby", re: /https?:\/\/whereby\.com\/[^\s<>"]+/i },
];

/** Trim the punctuation a link picks up from prose around it. */
function normalise(url: string): string {
  return url.replace(/[.,;:)\]]+$/, "");
}

/**
 * Google Calendar, open on this event.
 *
 * `htmlLink` is Google's own URL for the event and is what the button should
 * use — it lands on the event, not near it. It was being dropped by `mapEvent`
 * in `src/lib/ipc.ts`; now that it is carried, this prefers it.
 *
 * The day view is the fallback, for the two rows that have no link: fixture
 * data, and an event created in this session before sync has answered. That is
 * a worse link but an honest one — it lands the user two clicks from the event
 * rather than nowhere.
 *
 * `authuser` is what makes either open in the right account when several Google
 * sessions are signed in, which with several accounts is always. Google's own
 * `htmlLink` never carries it, so it is added here.
 */
export function googleCalendarUrl(event: CalendarEvent, accountEmail?: string): string {
  const base = event.htmlLink || dayUrl(event.start);
  if (!accountEmail) return base;
  const separator = base.includes("?") ? "&" : "?";
  return `${base}${separator}authuser=${encodeURIComponent(accountEmail)}`;
}

function dayUrl(start: number): string {
  const d = new Date(start);
  return `https://calendar.google.com/calendar/u/0/r/day/${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
}
