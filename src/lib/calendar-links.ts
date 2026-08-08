/**
 * The two links an event detail needs, derived rather than stored.
 *
 * Neither `htmlLink` nor `conferenceData` reaches the UI: `src/lib/ipc.ts` maps
 * the Rust row onto `CalendarEvent` and carries neither field. Rather than
 * widen a seam another unit owns, both are reconstructed from what *is* here —
 * and the reconstruction is honest about what it can and cannot do.
 */

import type { CalendarEvent } from "@/types";

/**
 * The video call on an event, if it has one.
 *
 * Google puts a Meet link in `conferenceData`, but it also copies it into the
 * description of almost every invitation, and Zoom/Teams/Webex only ever appear
 * in the description or the location. Scanning both fields therefore catches
 * more calls than `conferenceData` alone would, with no extra sync surface.
 *
 * Ordering matters: `location` is checked first because when a link is there it
 * is the one the organiser meant, while a description can also quote a *past*
 * meeting's link in a reply chain.
 */
export function conferenceLink(event: CalendarEvent): ConferenceLink | null {
  for (const field of [event.location, event.description]) {
    if (!field) continue;
    for (const pattern of PROVIDERS) {
      const match = field.match(pattern.re);
      if (match) return { provider: pattern.name, url: normalise(match[0]) };
    }
  }
  return null;
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
 * Google Calendar, open on this event's day, in this event's account.
 *
 * **Not a deep link to the event itself**, and it cannot be one: the deep-link
 * form is `.../r/eventedit/<base64(googleEventId + " " + calendarId)>`, and the
 * UI never receives `googleEventId` — `mapEvent` in `src/lib/ipc.ts` drops it.
 * The day view for the right account is the closest honest thing, and it lands
 * the user two clicks from the event rather than none.
 *
 * `authuser` is what makes it open in the right account when several Google
 * sessions are signed in, which with several accounts is always.
 */
export function googleCalendarUrl(event: CalendarEvent, accountEmail?: string): string {
  const d = new Date(event.start);
  const path = `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
  const base = `https://calendar.google.com/calendar/u/0/r/day/${path}`;
  return accountEmail ? `${base}?authuser=${encodeURIComponent(accountEmail)}` : base;
}
