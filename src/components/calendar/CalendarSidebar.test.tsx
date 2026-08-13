/**
 * The calendar rail, tested as markup and as two pure functions.
 *
 * Four claims worth pinning, and only one of them is about names.
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
 *
 * The third is that a hide is remembered. It was not: `ui.hiddenCalendars`
 * lived for the length of the window and the persisted session had no field for
 * it, so hiding "Training" lasted until the next launch, five times in one day.
 * `reconcileVisibility` is where the answer is decided and it is pure, because
 * a bug you can only reproduce by quitting the app is one that never gets a
 * second test.
 *
 * The fourth is that one calendar is one row. Four accounts subscribing to
 * Google's US holidays feed used to draw four rows — of one switch, since
 * visibility is keyed by calendar id, so unticking any of them turned all four
 * pale.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Account, Calendar } from "@/types";
import type { Solo } from "@/lib/calendar-solo";
import { parseSession, type CalendarVisibility } from "@/lib/prefs";
import {
  CalendarSidebar,
  calendarLabel,
  calendarRows,
  reconcileVisibility,
} from "./CalendarSidebar";

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

const SECOND: Account = {
  id: 2,
  email: "bruno@work.example",
  name: "Bruno",
  colorIndex: 2,
  kind: "workspace",
};

function render(
  calendars: Calendar[],
  accounts: Account[] = [ACCOUNT],
  hidden: string[] = [],
  solo: Solo | null = null,
) {
  return renderToStaticMarkup(
    <CalendarSidebar
      accounts={accounts}
      calendars={calendars}
      hidden={hidden}
      colorFor={() => "#16a765"}
      dark={false}
      solo={solo}
      onToggle={() => {}}
      onSolo={() => {}}
      settings={{ mergeDuplicates: false, showDeclined: false, showWeekends: true }}
      onSettings={() => {}}
    />,
  );
}

const NOTHING_RECONCILED: ReadonlySet<string> = new Set();

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
    const html = render([calendar()], [ACCOUNT], ["c1"]);
    expect(html).toContain('aria-pressed="false"');
  });

  it("offers taking a calendar out of the list as a second real button", () => {
    // The row's own press is show/hide. Taking it out of the rail is a separate
    // control with its own accessible name, rather than a modifier on the row —
    // a gesture with no element is a mouse-only affordance with extra steps.
    const html = render([calendar({ name: "Training" })]);
    expect(html).toContain('aria-label="Hide Training from list"');
    // Faded until hovered or focused, and focusable throughout: `opacity`, so
    // the button stays in the tab order and in the accessibility tree.
    expect(html).toContain("opacity-0");
    expect(html).toContain("focus-visible:opacity-100");
  });

  it("offers solo as a third real button, naming the key that does it", () => {
    // ⌥-click is the accelerator; this is the half that can be found. Google
    // shows "Display this only" on hover and nothing at all otherwise, which is
    // the same bargain — except that a button is also a tab stop.
    const html = render([calendar({ name: "Training" })]);
    expect(html).toContain('aria-label="Show only Training"');
    expect(html).toContain("Show only this calendar (⌥1)");
  });

  it("lights the soloed row's chip and offers the way back on it", () => {
    const html = render([calendar()], [ACCOUNT], [], { kind: "calendar", id: "c1" });
    expect(html).toContain('aria-label="Show every calendar"');
    // Lit, not faded: while one calendar is soloed the return may not be
    // waiting behind a hover.
    expect(html).toMatch(/aria-label="Show every calendar"[^>]*bg-accent/);
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

  it("renders with no accounts and no calendars", () => {
    // First launch, and the launch after the last account is removed. The rail
    // is still a rail: the three switches at the foot of it are not about any
    // particular calendar and stay usable.
    const html = render([], []);
    expect(html).toContain("Merge duplicates");
    expect(html).not.toContain("aria-pressed");
    expect(html).not.toContain("Hidden from list");
  });

  it("starts a calendar Google says is unsubscribed out of the list, and offers it back", () => {
    // `deleted` is Google's own answer, and the row only exists at all because
    // the events are still in the store. Nothing here reads the calendar's
    // *name*: "Earnest Capital Calendar (Subscription Expired)" says it is dead
    // in its title, and matching on that string would work until the next dead
    // calendar was called something else.
    const html = render([
      calendar({ id: "live", name: "Family" }),
      calendar({ id: "dead", name: "Earnest Capital Calendar (Subscription Expired)", deleted: true }),
    ]);
    expect(html).toContain("Hidden from list (1)");
    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain("Family");
  });
});

describe("one calendar, one row", () => {
  // Read out of his own store: four rows share the id
  // `en.usa#holiday@group.v.calendar.google.com` and two share
  // `bruno.bornsztein@clickfunnels.com`. `calendars` is a list of
  // subscriptions — one per (account, calendar) — and the rail was drawing one
  // row per subscription.
  const HOLIDAYS = "en.usa#holiday@group.v.calendar.google.com";

  function holidays(accountId: number, selected: boolean): Calendar {
    return {
      id: HOLIDAYS,
      accountId,
      name: "Holidays in United States",
      colorIndex: 1,
      accessRole: "reader",
      selected,
    };
  }

  it("collapses one calendar that several accounts subscribe to", () => {
    const { groups } = calendarRows(
      [ACCOUNT, SECOND],
      [holidays(1, false), holidays(2, true)],
      {},
    );
    const rows = groups.flatMap((group) => group.rows);
    expect(rows).toHaveLength(1);
    expect(rows[0].accountIds).toEqual([1, 2]);
  });

  it("puts the row under the account that owns the calendar", () => {
    // His clickfunnels calendar is `owner` and primary on the clickfunnels
    // account and `reader` on the gmail one. Under gmail it would be labelled
    // read-only, which is not true of a calendar he owns.
    const shared = "bruno@work.example";
    const { groups } = calendarRows(
      [ACCOUNT, SECOND],
      [
        { id: shared, accountId: 1, name: shared, colorIndex: 1, accessRole: "reader" },
        {
          id: shared,
          accountId: 2,
          name: shared,
          colorIndex: 2,
          accessRole: "owner",
          primary: true,
        },
      ],
      {},
    );
    expect(groups[0].rows).toHaveLength(0);
    expect(groups[1].rows).toHaveLength(1);
    expect(groups[1].rows[0].calendar.accessRole).toBe("owner");
    expect(groups[1].rows[0].accountIds).toEqual([2, 1]);
  });

  it("keeps two different calendars that share a name apart", () => {
    // Same name, different ids, one per account: two calendars, and nothing may
    // collapse them. The account heading says whose, and so does the tooltip,
    // because a rail that scrolls puts the heading off screen.
    const html = render(
      [
        { id: "fam-personal", accountId: 1, name: "Family", colorIndex: 1 },
        { id: "fam-work", accountId: 2, name: "Family", colorIndex: 2 },
      ],
      [ACCOUNT, SECOND],
    );
    // Two rows, both on. `aria-pressed="true"` rather than `aria-pressed`,
    // because the solo chips beside them carry one too — and theirs is false.
    expect(html.match(/aria-pressed="true"/g)).toHaveLength(2);
    expect(html).toContain("title=\"Family\nbruno@example.com");
    expect(html).toContain("title=\"Family\nbruno@work.example");
  });

  it("shows the shortcut the keymap would actually run", () => {
    // `v <digit>` indexes the `calendars` array. The rail used to number the
    // rows it painted, so a collapsed group — or, now, a collapsed duplicate —
    // shifted every number below it and the tooltip advertised a shortcut that
    // toggled some other calendar.
    const { groups } = calendarRows(
      [ACCOUNT, SECOND],
      [holidays(1, true), calendar({ id: "later", accountId: 2, name: "Work" })],
      {},
    );
    expect(groups[0].rows[0].slot).toBe(0);
    expect(groups[1].rows[0].slot).toBe(1);
  });

  it("starts a calendar on if any account has it selected in Google", () => {
    // Deselected on two of his accounts, selected on the other two. One row, and
    // hiding it would be hiding events he switched on in Google.
    expect(
      calendarRows([ACCOUNT, SECOND], [holidays(1, false), holidays(2, true)], {})
        .groups.flatMap((g) => g.rows)[0].state,
    ).toBe("shown");
    expect(
      calendarRows([ACCOUNT, SECOND], [holidays(1, false), holidays(2, false)], {})
        .groups.flatMap((g) => g.rows)[0].state,
    ).toBe("hidden");
  });
});

describe("a hide, across a relaunch", () => {
  const training = calendar({ id: "training", name: "Training", selected: true });

  it("survives a reload of the persisted session", () => {
    // The whole of "this one keeps coming back". The window stores the hide,
    // the session is written and read back, and the restored map is what puts
    // `ui.hiddenCalendars` back where it was.
    const decisions: Record<string, CalendarVisibility> = { training: "hidden" };
    const reloaded = parseSession(
      JSON.parse(JSON.stringify({ calendarVisibility: decisions })),
    );
    expect(reloaded.calendarVisibility).toEqual({ training: "hidden" });

    const { toggles, patch } = reconcileVisibility(
      [training],
      reloaded.calendarVisibility ?? {},
      [],
      NOTHING_RECONCILED,
    );
    expect(toggles).toEqual(["training"]);
    // Nothing is adopted over a decision that already exists.
    expect(patch).toEqual({});
  });

  it("is not undone by a sync that reports selected: true", () => {
    // Google still thinks Training is shown, and says so on every sync. The
    // stored decision exists, so `selected` is never consulted again.
    const already: ReadonlySet<string> = new Set(["training"]);
    expect(
      reconcileVisibility([training], { training: "hidden" }, ["training"], already),
    ).toEqual({ toggles: [], patch: {}, seen: [] });
  });

  it("adopts Google's selected exactly once, ever", () => {
    // The bug this replaces was a `useRef`: once per *window*, which is the same
    // thing as once per calendar only while nothing is written down. With hides
    // persisted, a ref would re-hide — on every launch, silently — a calendar
    // Google still calls deselected that the user had turned back on here.
    const holiday = calendar({ id: "holiday", name: "Jewish Holidays", selected: false });

    const first = reconcileVisibility([holiday], {}, [], NOTHING_RECONCILED);
    expect(first.toggles).toEqual(["holiday"]);
    expect(first.patch).toEqual({ holiday: "hidden" });

    // The user turns it back on. `hidden` is the truth now, and the map follows.
    const seen: ReadonlySet<string> = new Set(first.seen);
    const after = reconcileVisibility([holiday], first.patch, [], seen);
    expect(after.patch).toEqual({ holiday: "shown" });
    expect(after.toggles).toEqual([]);

    // Next launch. Nothing reconciled, Google still says `selected: false`, and
    // the calendar stays on because the decision outranks it.
    const relaunch = reconcileVisibility([holiday], after.patch, [], NOTHING_RECONCILED);
    expect(relaunch.toggles).toEqual([]);
    expect(relaunch.patch).toEqual({});
  });

  it("records a hide made from the keyboard, not just from the row", () => {
    // `v 1` dispatches straight into the shell's reducer. The rail sees the
    // result rather than the keystroke, which is why recording is a pass over
    // `hidden` and not a handler on the button.
    const seen: ReadonlySet<string> = new Set(["training"]);
    const { patch, toggles } = reconcileVisibility(
      [training],
      { training: "shown" },
      ["training"],
      seen,
    );
    expect(patch).toEqual({ training: "hidden" });
    expect(toggles).toEqual([]);
  });

  it("brings back a calendar taken out of the list when its events are asked for", () => {
    // `v <digit>` still addresses a calendar with no row. Turning its events on
    // is taken as asking for it back, rather than leaving blocks on the grid
    // belonging to a row that cannot be found.
    const seen: ReadonlySet<string> = new Set(["training"]);
    expect(
      reconcileVisibility([training], { training: "unlisted" }, ["training"], seen).patch,
    ).toEqual({});
    expect(
      reconcileVisibility([training], { training: "unlisted" }, [], seen).patch,
    ).toEqual({ training: "shown" });
  });

  it("hides the events of a calendar restored as unlisted", () => {
    const stored: Record<string, CalendarVisibility> = { training: "unlisted" };
    expect(reconcileVisibility([training], stored, [], NOTHING_RECONCILED).toggles).toEqual([
      "training",
    ]);
  });

  it("asks one question per calendar, however many accounts subscribe", () => {
    // Four rows for one id used to mean four passes over the same decision, and
    // the later ones read a `hidden` that the earlier ones had not updated yet —
    // which recorded "shown" over the hide that was being restored.
    const copies = [1, 2, 3, 4].map((accountId) =>
      calendar({ id: "holiday", accountId, name: "Holidays in United States" }),
    );
    const { toggles, seen } = reconcileVisibility(
      copies,
      { holiday: "hidden" },
      [],
      NOTHING_RECONCILED,
    );
    expect(toggles).toEqual(["holiday"]);
    expect(seen).toEqual(["holiday"]);
  });
});
