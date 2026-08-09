/**
 * The calendar rail, tested as markup.
 *
 * Two claims worth pinning, and only one of them is about names.
 *
 * The first is that the rail shows what Google calls a calendar. That used to
 * be impossible — nothing had ever fetched the metadata — so the component made
 * labels up, and `Shared · d814cb` is the string this file exists to keep from
 * coming back.
 *
 * The second is the keyboard, which is a standing requirement for this app and
 * the easiest thing in a sidebar to lose: a `<div onClick>` renders identically
 * and the only thing that would tell you is a tab key. So the assertions are on
 * the *elements* — real buttons with real accessible names and a real pressed
 * state — via `react-dom/server`, with no jsdom and nothing to click.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Account, Calendar } from "@/types";
import { CalendarSidebar, calendarLabel } from "./CalendarSidebar";

function calendar(over: Partial<Calendar> = {}): Calendar {
  return { id: "c1", accountId: 1, name: "Family", colorIndex: 1, ...over };
}

const ACCOUNT: Account = {
  id: 1,
  email: "bruno@example.com",
  name: "Bruno",
  colorIndex: 1,
  kind: "personal",
};

function render(calendars: Calendar[]): string {
  return renderToStaticMarkup(
    <CalendarSidebar
      accounts={[ACCOUNT]}
      calendars={calendars}
      hidden={[]}
      colorFor={() => "#16a765"}
      dark={false}
      soloAccount={null}
      onToggle={() => {}}
      onSolo={() => {}}
      settings={{ mergeDuplicates: false, showDeclined: false, showWeekends: true }}
      onSettings={() => {}}
    />,
  );
}

describe("calendarLabel", () => {
  it("shows the name Google gave the calendar", () => {
    expect(calendarLabel(calendar({ name: "Holidays in United States" }))).toBe(
      "Holidays in United States",
    );
    expect(calendarLabel(calendar({ name: "Dad/Ben Schedule" }))).toBe("Dad/Ben Schedule");
  });

  it("never invents a label again", () => {
    // The whole bug: a calendar with no metadata was shown as `Shared · d814cb`,
    // a string that appears nowhere in Google and is identical for every other
    // shared calendar whose id happens to start the same way. Shortening the id
    // is allowed; making one up is not.
    const raw = "c_d814cb1f2e@group.calendar.google.com";
    const label = calendarLabel(calendar({ id: raw, name: raw }));
    expect(label).not.toContain("Shared");
    expect(label).toBe("c_d814cb1f2e");
  });

  it("shortens Google's generated addresses and leaves real ones whole", () => {
    expect(
      calendarLabel(
        calendar({ name: "en.usa#holiday@group.v.calendar.google.com" }),
      ),
    ).toBe("en.usa#holiday");
    // Somebody's actual address is already a name; truncating it would lose the
    // only part that says whose calendar it is.
    expect(calendarLabel(calendar({ name: "alicia@example.com" }))).toBe("alicia@example.com");
  });

  it("falls back to the id when there is no name at all", () => {
    expect(calendarLabel(calendar({ id: "c9", name: "   " }))).toBe("c9");
  });
});

describe("the calendar rail", () => {
  it("renders each calendar as a real toggle button", () => {
    const html = render([calendar({ name: "Alicia & Bruno" })]);
    // Buttons, so tab reaches them and space and enter work without any
    // keydown handler of our own.
    expect(html).toContain("<button");
    expect(html).not.toMatch(/<div[^>]*onclick/i);
    // A swatch cannot be read aloud, so the pressed state is stated.
    expect(html).toContain('aria-pressed="true"');
    expect(html).toContain("Alicia &amp; Bruno");
  });

  it("says a calendar is off rather than just drawing it differently", () => {
    const html = renderToStaticMarkup(
      <CalendarSidebar
        accounts={[ACCOUNT]}
        calendars={[calendar()]}
        hidden={["c1"]}
        colorFor={() => "#16a765"}
        dark={false}
        soloAccount={null}
        onToggle={() => {}}
        onSolo={() => {}}
        settings={{ mergeDuplicates: false, showDeclined: false, showWeekends: true }}
        onSettings={() => {}}
      />,
    );
    expect(html).toContain('aria-pressed="false"');
  });

  it("puts the full name in the tooltip, because the row truncates", () => {
    const html = render([
      calendar({ name: "Holidays in United States and its territories" }),
    ]);
    // `truncate` is what makes a long name fit a 13rem rail; the title is the
    // only place the rest of it survives, so it leads with the name.
    expect(html).toContain("truncate");
    expect(html).toMatch(/title="Holidays in United States and its territories/);
  });

  it("mentions a description and a read-only calendar in the tooltip", () => {
    const html = render([
      calendar({ name: "Meridian", description: "Shared with me", accessRole: "reader" }),
    ]);
    expect(html).toContain("Shared with me");
    expect(html).toContain("Read-only");
  });

  it("keeps a calendar whose account has gone reachable", () => {
    const html = render([calendar({ accountId: 99, name: "Orphan" })]);
    expect(html).toContain("Orphan");
  });
});
