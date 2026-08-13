/**
 * Solo, and the one thing it must never do: lose the configuration.
 *
 * He has five accounts, five calendars taken out of the list already and more
 * unticked inside the ones that are listed. The obvious implementation of
 * "display this only" — untick everything else, then tick it all back — cannot
 * be got right by inspection, and getting it wrong is silent: the rail comes
 * back looking plausible and four calendars are on that were off. So the
 * implementation is a filter that writes nothing, and these are the assertions
 * that say so.
 */

import { describe, expect, it } from "vitest";
import type { Account, Calendar, CalendarEvent } from "@/types";
import {
  accountSoloAt,
  calendarSoloAt,
  nextSolo,
  sameSolo,
  soloEvents,
  type Solo,
} from "./calendar-solo";

function event(over: Partial<CalendarEvent> & Pick<CalendarEvent, "id">): CalendarEvent {
  return {
    accountId: 1,
    calendarId: "family",
    title: "Something",
    start: 0,
    end: 1,
    allDay: false,
    attendees: [],
    ...over,
  };
}

/** One event per calendar, across two accounts. */
const FAMILY = event({ id: 1, accountId: 1, calendarId: "family" });
const WORK = event({ id: 2, accountId: 1, calendarId: "work" });
const TRAINING = event({ id: 3, accountId: 1, calendarId: "training" });
const OTHER_ACCOUNT = event({ id: 4, accountId: 2, calendarId: "clients" });

/** `training` is unticked and `dead` is out of the list, so neither is in here. */
const DEAD = event({ id: 5, accountId: 1, calendarId: "dead" });
const ALL = [FAMILY, WORK, TRAINING, OTHER_ACCOUNT, DEAD];
const SHOWN = [FAMILY, WORK, OTHER_ACCOUNT];

const CALENDARS: Calendar[] = [
  { id: "family", accountId: 1, name: "Family", colorIndex: 1 },
  { id: "work", accountId: 1, name: "Work", colorIndex: 2 },
  { id: "training", accountId: 1, name: "Training", colorIndex: 3 },
  { id: "clients", accountId: 2, name: "Clients", colorIndex: 4 },
];

const ACCOUNTS: Account[] = [
  { id: 1, email: "bruno@example.com", name: "Bruno", colorIndex: 1, kind: "personal" },
  { id: 2, email: "bruno@work.example", name: "Bruno", colorIndex: 2, kind: "workspace" },
];

describe("soloing a calendar", () => {
  it("hides every other calendar, across every account", () => {
    // Google's "Display this only" is global, and so is this. Soloing inside one
    // account would leave the other four accounts' blocks on the grid, which is
    // not what anybody means by "only".
    const only = soloEvents({ kind: "calendar", id: "family" }, SHOWN, ALL);
    expect(only.map((e) => e.id)).toEqual([FAMILY.id]);
    expect(only).not.toContain(WORK);
    expect(only).not.toContain(OTHER_ACCOUNT);
  });

  it("shows a calendar that is unticked, rather than an empty grid", () => {
    // `training` is off in the rail, so it is not in `SHOWN`. A solo naming it
    // is the user saying "this one", and answering with nothing would be the
    // gesture failing at the only thing it does.
    const only = soloEvents({ kind: "calendar", id: "training" }, SHOWN, ALL);
    expect(only.map((e) => e.id)).toEqual([TRAINING.id]);
  });

  it("gives back exactly what was showing before, not everything", () => {
    // The whole point. Un-soloing returns the array it was handed — the same
    // one, by reference — so there is no reconstruction to get wrong: the
    // unticked `training` and the unlisted `dead` are still absent.
    const before = SHOWN;
    const soloed = soloEvents({ kind: "calendar", id: "family" }, before, ALL);
    expect(soloed).toHaveLength(1);

    const after = soloEvents(null, before, ALL);
    expect(after).toBe(before);
    expect(after.map((e) => e.id)).toEqual([FAMILY.id, WORK.id, OTHER_ACCOUNT.id]);
    expect(after.map((e) => e.id)).not.toContain(TRAINING.id);
    expect(after.map((e) => e.id)).not.toContain(DEAD.id);
  });

  it("leaves an account solo filtering only what is already shown", () => {
    // The other half of the pair, unchanged: inside one account the per-calendar
    // ticks still mean something, so `training` stays off.
    const only = soloEvents({ kind: "account", id: 1 }, SHOWN, ALL);
    expect(only.map((e) => e.id)).toEqual([FAMILY.id, WORK.id]);
  });
});

describe("what pressing solo leaves behind", () => {
  const family: Solo = { kind: "calendar", id: "family" };
  const work: Solo = { kind: "calendar", id: "work" };
  const account: Solo = { kind: "account", id: 1 };

  it("starts, moves and clears", () => {
    expect(nextSolo(null, family)).toEqual(family);
    expect(nextSolo(family, work)).toEqual(work);
    expect(nextSolo(family, family)).toBeNull();
  });

  it("holds one solo at a time, so a calendar solo clears an account solo", () => {
    // Both at once has a state that draws nothing — calendar X soloed while the
    // account it does not belong to is soloed — and a gesture that promises
    // "show me this" may not answer with an empty grid.
    expect(nextSolo(account, family)).toEqual(family);
    expect(nextSolo(family, account)).toEqual(account);
  });

  it("tells an account solo and a calendar solo apart when the ids collide", () => {
    // `AccountId` is a number and `CalendarId` a string, but the comparison is
    // written on `kind` first rather than trusting that to stay true.
    expect(sameSolo({ kind: "account", id: 1 }, { kind: "calendar", id: "1" })).toBe(false);
  });
});

describe("the keyboard and the rail address the same calendar", () => {
  it("resolves ⌥<digit> to the target the row's chip sends", () => {
    // The rail's chip builds `{ kind: "calendar", id }` from the row it is on;
    // ⌥3 builds one from the third entry of the same `calendars` array that
    // `v 3` counts. The two have to be the same object or the shortcut in the
    // tooltip is advertising a key that solos some other calendar.
    expect(calendarSoloAt(CALENDARS, 2)).toEqual({ kind: "calendar", id: "training" });
    expect(accountSoloAt(ACCOUNTS, 1)).toEqual({ kind: "account", id: 2 });
  });

  it("does nothing past the end of the list", () => {
    expect(calendarSoloAt(CALENDARS, 8)).toBeNull();
    expect(accountSoloAt(ACCOUNTS, 4)).toBeNull();
  });

  it("runs through the same decision, so a second press clears either way", () => {
    const fromKey = calendarSoloAt(CALENDARS, 0);
    const fromRail: Solo = { kind: "calendar", id: CALENDARS[0].id };
    expect(fromKey).toEqual(fromRail);
    expect(nextSolo(fromRail, fromKey!)).toBeNull();
    expect(nextSolo(fromKey!, fromRail)).toBeNull();
  });
});
