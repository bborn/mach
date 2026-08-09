import { describe, expect, it } from "vitest";
import type { CalendarEvent } from "@/types";
import { conferenceLink, dialUrl, entryLabel, googleCalendarUrl, joinUrl } from "./calendar-links";

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

describe("conferenceData, now that it is stored", () => {
  const withMeet: CalendarEvent = {
    ...base,
    conference: {
      id: "abc-defg-hij",
      name: "Google Meet",
      entryPoints: [
        {
          kind: "video",
          uri: "https://meet.google.com/abc-defg-hij",
          label: "meet.google.com/abc-defg-hij",
        },
      ],
    },
  };

  it("prefers Google's own structured answer to a link scraped out of prose", () => {
    // The description can quote a *past* meeting's link; conferenceData cannot.
    const link = conferenceLink({
      ...withMeet,
      description: "Last week: https://meet.google.com/old-one-xyz",
    });
    expect(link).toEqual({ provider: "Google Meet", url: "https://meet.google.com/abc-defg-hij" });
  });

  it("falls back to the scan for the providers Google never mints a conference for", () => {
    expect(conferenceLink({ ...base, location: "https://acme.zoom.us/j/98765432" })?.provider).toBe(
      "Zoom",
    );
  });

  it("skips a stored entry point it would refuse to open", () => {
    // The URI is attacker-controlled, so an entry point that fails validation is
    // not offered as a button — and the scan behind it still gets its chance.
    const link = conferenceLink({
      ...base,
      conference: {
        name: "Google Meet",
        entryPoints: [{ kind: "video", uri: "javascript:alert(1)" }],
      },
      location: "https://meet.google.com/real-link-abc",
    });
    expect(link?.url).toBe("https://meet.google.com/real-link-abc");
  });
});

describe("joinUrl", () => {
  it("accepts an ordinary https conference link, unchanged", () => {
    expect(joinUrl("https://meet.google.com/abc-defg-hij")).toBe(
      "https://meet.google.com/abc-defg-hij",
    );
    expect(joinUrl("  https://acme.zoom.us/j/98765432?pwd=x  ")).toBe(
      "https://acme.zoom.us/j/98765432?pwd=x",
    );
  });

  it("refuses a scheme that is not https", () => {
    // The three that matter: script execution, an inline document, and cleartext.
    expect(joinUrl("javascript:alert(document.cookie)")).toBeNull();
    expect(joinUrl("data:text/html,<script>alert(1)</script>")).toBeNull();
    expect(joinUrl("http://meet.google.com/abc-defg-hij")).toBeNull();
    expect(joinUrl("file:///etc/passwd")).toBeNull();
  });

  it("refuses the credentials trick, which is what a hostile invitation would use", () => {
    // Reads as Meet, resolves to evil.example — and the label beside the button
    // is attacker-controlled too, so nothing else on screen would contradict it.
    expect(joinUrl("https://meet.google.com@evil.example/join")).toBeNull();
    expect(joinUrl("https://user:pass@evil.example/join")).toBeNull();
  });

  it("refuses a host that is not a dotted name", () => {
    expect(joinUrl("https://intranet/join")).toBeNull();
    expect(joinUrl("https://10.0.0.1/join")).toBeNull();
    expect(joinUrl("https://[::1]/join")).toBeNull();
  });

  it("refuses nonsense rather than guessing at it", () => {
    expect(joinUrl("")).toBeNull();
    expect(joinUrl(undefined)).toBeNull();
    expect(joinUrl("meet.google.com/abc-defg-hij")).toBeNull();
  });
});

describe("dialUrl", () => {
  it("accepts the dial strings Google actually sends", () => {
    expect(dialUrl("tel:+1-513-555-0199")).toBe("tel:+1-513-555-0199");
    expect(dialUrl("tel:+1-513-555-0199,,396011834#")).toBe("tel:+1-513-555-0199,,396011834#");
  });

  it("refuses anything that is a tel: prefix with a payload behind it", () => {
    expect(dialUrl("tel:+1-513-555-0199 <script>")).toBeNull();
    expect(dialUrl("telnet:host")).toBeNull();
    expect(dialUrl("tel:")).toBeNull();
  });
});

describe("entryLabel", () => {
  it("uses Google's label, and makes the URI readable when there is none", () => {
    expect(entryLabel({ kind: "video", uri: "x", label: "meet.google.com/abc" })).toBe(
      "meet.google.com/abc",
    );
    expect(entryLabel({ kind: "video", uri: "https://meet.google.com/abc" })).toBe(
      "meet.google.com/abc",
    );
    expect(entryLabel({ kind: "phone", uri: "tel:+1-513-555-0199" })).toBe("+1-513-555-0199");
  });
});
