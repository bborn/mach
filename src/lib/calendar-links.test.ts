import { describe, expect, it } from "vitest";
import type { CalendarEvent } from "@/types";
import { conferenceLink, googleCalendarUrl } from "./calendar-links";

const base: CalendarEvent = {
  id: 1,
  calendarId: "primary",
  accountId: 1,
  title: "Standup",
  start: new Date(2026, 7, 7, 9, 0).getTime(),
  end: new Date(2026, 7, 7, 9, 30).getTime(),
  allDay: false,
  attendees: [],
};

describe("conferenceLink", () => {
  it("finds a Meet link in the description, where Google copies it", () => {
    const link = conferenceLink({
      ...base,
      description: "Join at https://meet.google.com/abc-defg-hij or dial in.",
    });
    expect(link).toEqual({ provider: "Google Meet", url: "https://meet.google.com/abc-defg-hij" });
  });

  it("prefers the location, which is where the organiser put the real one", () => {
    const link = conferenceLink({
      ...base,
      location: "https://meet.google.com/new-one-here",
      description: "Last week we used https://meet.google.com/old-one-xyz",
    });
    expect(link?.url).toBe("https://meet.google.com/new-one-here");
  });

  it("recognises Zoom, Teams and Webex too", () => {
    expect(conferenceLink({ ...base, location: "https://acme.zoom.us/j/98765432" })?.provider).toBe(
      "Zoom",
    );
    expect(
      conferenceLink({
        ...base,
        location: "https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc",
      })?.provider,
    ).toBe("Microsoft Teams");
    expect(
      conferenceLink({ ...base, location: "https://acme.webex.com/meet/alex" })?.provider,
    ).toBe("Webex");
  });

  it("trims the punctuation a link picks up from prose", () => {
    expect(
      conferenceLink({ ...base, description: "(https://meet.google.com/abc-defg-hij)." })?.url,
    ).toBe("https://meet.google.com/abc-defg-hij");
  });

  it("is null when there is no call to join", () => {
    expect(conferenceLink({ ...base, location: "Room 2" })).toBeNull();
    expect(conferenceLink(base)).toBeNull();
  });
});

describe("googleCalendarUrl", () => {
  const linked: CalendarEvent = {
    ...base,
    htmlLink: "https://www.google.com/calendar/event?eid=abc123",
  };

  it("uses Google's own link for the event when the row has one", () => {
    expect(googleCalendarUrl(linked)).toBe(
      "https://www.google.com/calendar/event?eid=abc123",
    );
  });

  it("adds the account to a link that already has a query string", () => {
    expect(googleCalendarUrl(linked, "alex@example.com")).toBe(
      "https://www.google.com/calendar/event?eid=abc123&authuser=alex%40example.com",
    );
  });

  it("falls back to the day when there is no link — fixtures, and unsynced rows", () => {
    expect(googleCalendarUrl(base)).toBe(
      "https://calendar.google.com/calendar/u/0/r/day/2026/8/7",
    );
  });

  it("names the account, so it lands in the right session", () => {
    expect(googleCalendarUrl(base, "alex@example.com")).toBe(
      "https://calendar.google.com/calendar/u/0/r/day/2026/8/7?authuser=alex%40example.com",
    );
  });
});
