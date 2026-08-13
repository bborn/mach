import { describe, expect, it } from "vitest";
import type { CalendarEvent } from "@/types";
import {
  conferenceLink,
  dialUrl,
  entryLabel,
  googleCalendarUrl,
  joinUrl,
  looksLikeAddress,
  mapsUrl,
} from "./calendar-links";

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

/**
 * The locations here are the shapes that actually turn up on his calendar:
 * a venue with a street address after it, a room, a Meet link, and nothing.
 */
describe("looksLikeAddress", () => {
  it("says yes to a street address, with or without the venue in front of it", () => {
    expect(
      looksLikeAddress(
        "Twin Ignition Startup Garage, 1317 Marshall St NE, Minneapolis, MN 55413, USA",
      ),
    ).toBe(true);
    expect(looksLikeAddress("1317 Marshall St NE, Minneapolis, MN 55413, USA")).toBe(true);
    expect(looksLikeAddress("10 Downing Street, London SW1A 2AA")).toBe(true);
    expect(looksLikeAddress("Bahnhofstrasse 10, 8001 Zürich")).toBe(true);
  });

  it("says no to a room, which is the case a map would be wrong about", () => {
    // The number is there and means nothing. Offering directions to this is
    // the specific failure the predicate leans away from.
    expect(looksLikeAddress("Room 2")).toBe(false);
    expect(looksLikeAddress("Conference Room 2, Building A")).toBe(false);
    expect(looksLikeAddress("Bruno's desk")).toBe(false);
  });

  it("says no to the rest of what a week actually holds", () => {
    // Every other location in the fixture week, which is a fair sample of what
    // a `location` carries: a floor, a building, a provider's name, a school.
    for (const location of [
      "Northloop, 3rd floor",
      "Westview HS",
      "All hands",
      "Zoom",
      "Google Meet",
      "Clinic",
    ]) {
      expect(looksLikeAddress(location), location).toBe(false);
    }
  });

  it("says no to a call, which already has a Join button", () => {
    expect(looksLikeAddress("https://meet.google.com/abc-defg-hij")).toBe(false);
    expect(looksLikeAddress("meet.google.com/abc-defg-hij")).toBe(false);
    expect(looksLikeAddress("https://acme.zoom.us/j/98765432")).toBe(false);
  });

  it("says no to nothing at all", () => {
    expect(looksLikeAddress("")).toBe(false);
    expect(looksLikeAddress("   ")).toBe(false);
    expect(looksLikeAddress(null)).toBe(false);
    expect(looksLikeAddress(undefined)).toBe(false);
  });

  it("refuses every scheme a hostile invitation could carry", () => {
    for (const hostile of [
      "javascript:alert(1)",
      "JavaScript:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "file:///etc/passwd",
      "zoommtg://zoom.us/join?confno=1",
      "vbscript:msgbox(1)",
    ]) {
      expect(looksLikeAddress(hostile)).toBe(false);
    }
  });

  it("refuses a paragraph, which is parking instructions rather than a place", () => {
    expect(looksLikeAddress(`1317 Marshall St NE ${"and then a long note ".repeat(30)}`)).toBe(
      false,
    );
  });
});

describe("mapsUrl", () => {
  it("is null wherever the location is not a place", () => {
    expect(mapsUrl("Room 2")).toBeNull();
    expect(mapsUrl("https://meet.google.com/abc-defg-hij")).toBeNull();
    expect(mapsUrl("")).toBeNull();
    expect(mapsUrl(undefined)).toBeNull();
  });

  it("carries the address as an encoded query, not as a path", () => {
    // Spaces, commas, an ampersand and a non-ASCII letter — everything that
    // would break a URL if it were pasted in raw.
    const url = mapsUrl("Café & Bar, 1 Rue de l'Église, 75001 Paris");
    expect(url).toBe(
      "https://www.google.com/maps/search/?api=1&query=" +
        "Caf%C3%A9%20%26%20Bar%2C%201%20Rue%20de%20l'%C3%89glise%2C%2075001%20Paris",
    );
    // The `&` inside the address is `%26`, so it cannot start a parameter of
    // its own, and `query` is still the last one Maps will read.
    expect(new URL(url!).searchParams.get("query")).toBe(
      "Café & Bar, 1 Rue de l'Église, 75001 Paris",
    );
  });

  it("cannot be talked into a scheme, a host, or a path by its input", () => {
    // Address-shaped, so the predicate lets it through — and the URL that comes
    // out is still an https Google Maps search with the payload inert inside a
    // query parameter. This is the second lock: the first is that a bare
    // `javascript:` location never reaches here at all.
    expect(mapsUrl("javascript:alert(1)")).toBeNull();

    for (const payload of [
      "123 Main Street, javascript:alert(1)",
      "123 Main Street#@evil.example",
      "123 Main Street?q=x&z=y",
      "123 Main Street /../../evil",
    ]) {
      const url = mapsUrl(payload);
      expect(url).not.toBeNull();
      const parsed = new URL(url!);
      expect(parsed.protocol).toBe("https:");
      expect(parsed.hostname).toBe("www.google.com");
      expect(parsed.pathname).toBe("/maps/search/");
      expect(parsed.hash).toBe("");
      expect(parsed.searchParams.get("query")).toBe(payload);
      expect(url).not.toContain("javascript:");
    }
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
