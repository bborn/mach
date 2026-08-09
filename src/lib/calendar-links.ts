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
