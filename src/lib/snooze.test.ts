/**
 * The instants, pinned.
 *
 * Every case here is built from a real local `Date` rather than an epoch
 * constant, because the whole module is about *local* wall-clock time — 8am is
 * 8am wherever the machine is, and an offset computed in UTC would be right in
 * London and an hour out in New York twice a year.
 *
 * The dates are chosen, not arbitrary. August 2026 is used throughout because
 * the 3rd is a Monday, which makes every weekday in the month easy to name:
 * add the day-of-month to the weekday and the arithmetic is checkable by eye.
 */

import { describe, expect, it } from "vitest";
import {
  LATER_TODAY_HOURS,
  SNOOZE_MORNING_HOUR,
  laterToday,
  moveSnoozeCursor,
  nextWeek,
  nextWeekdayAt,
  parseSnoozeAt,
  snoozeLabel,
  snoozeOptions,
  snoozeWhen,
  thisWeekend,
} from "./snooze";

/** A local instant. Month is 1-based here, unlike `Date`. */
function at(year: number, month: number, day: number, hour = 0, minute = 0): number {
  return new Date(year, month - 1, day, hour, minute, 0, 0).getTime();
}

/* August 2026: the 3rd is a Monday, the 8th a Saturday, the 9th a Sunday. */
const MON = at(2026, 8, 3, 9, 15);
const WED = at(2026, 8, 5, 14, 20);
const FRI = at(2026, 8, 7, 16, 0);
const SAT = at(2026, 8, 8, 10, 0);
const SUN = at(2026, 8, 9, 10, 0);

describe("weekdays", () => {
  it("names the day of the week the fixtures claim", () => {
    expect(new Date(MON).getDay()).toBe(1);
    expect(new Date(WED).getDay()).toBe(3);
    expect(new Date(FRI).getDay()).toBe(5);
    expect(new Date(SAT).getDay()).toBe(6);
    expect(new Date(SUN).getDay()).toBe(0);
  });
});

describe("later today", () => {
  it("is three hours out, rounded up to the hour", () => {
    // 9:15am + 3h = 12:15pm, rounded up to 1pm.
    expect(laterToday(MON)).toBe(at(2026, 8, 3, 13));
  });

  it("rounds up even when the extra is a minute", () => {
    expect(laterToday(at(2026, 8, 3, 9, 1))).toBe(at(2026, 8, 3, 13));
  });

  it("stays on the hour when now is on the hour", () => {
    expect(laterToday(at(2026, 8, 3, 9))).toBe(at(2026, 8, 3, 12));
  });

  it("is always at least LATER_TODAY_HOURS away", () => {
    const now = at(2026, 8, 3, 9, 59);
    expect(laterToday(now)! - now).toBeGreaterThanOrEqual(LATER_TODAY_HOURS * 3_600_000);
  });

  it("disappears once three hours out is tomorrow", () => {
    // 10:30pm + 3h is 1:30am, which is neither later nor today.
    expect(laterToday(at(2026, 8, 3, 22, 30))).toBeNull();
    // 9:30pm + 3h is 12:30am. Rounding up cannot rescue it.
    expect(laterToday(at(2026, 8, 3, 21, 30))).toBeNull();
    // 8:30pm + 3h is 11:30pm, which rounds up to midnight — the next day.
    expect(laterToday(at(2026, 8, 3, 20, 30))).toBeNull();
    // 8pm exactly is the last minute it survives: 11pm, still today.
    expect(laterToday(at(2026, 8, 3, 20))).toBe(at(2026, 8, 3, 23));
  });

  it("survives the last day of a month", () => {
    expect(laterToday(at(2026, 8, 31, 9))).toBe(at(2026, 8, 31, 12));
    expect(laterToday(at(2026, 8, 31, 23))).toBeNull();
  });
});

describe("this weekend", () => {
  it("is the coming Saturday at the morning hour", () => {
    expect(thisWeekend(MON)).toBe(at(2026, 8, 8, SNOOZE_MORNING_HOUR));
    expect(thisWeekend(WED)).toBe(at(2026, 8, 8, SNOOZE_MORNING_HOUR));
    expect(thisWeekend(FRI)).toBe(at(2026, 8, 8, SNOOZE_MORNING_HOUR));
  });

  it("is not offered during the weekend it names", () => {
    expect(thisWeekend(SAT)).toBeNull();
    expect(thisWeekend(SUN)).toBeNull();
  });
});

describe("next week", () => {
  it("is the coming Monday at the morning hour", () => {
    expect(nextWeek(WED)).toBe(at(2026, 8, 10, SNOOZE_MORNING_HOUR));
    expect(nextWeek(FRI)).toBe(at(2026, 8, 10, SNOOZE_MORNING_HOUR));
    expect(nextWeek(SAT)).toBe(at(2026, 8, 10, SNOOZE_MORNING_HOUR));
  });

  it("from a Sunday is the very next day", () => {
    expect(nextWeek(SUN)).toBe(at(2026, 8, 10, SNOOZE_MORNING_HOUR));
  });

  it("from a Monday is a whole week out, not this morning", () => {
    expect(nextWeek(MON)).toBe(at(2026, 8, 10, SNOOZE_MORNING_HOUR));
  });

  it("from early on a Monday is still a whole week out", () => {
    // The comparison is on the day, not the clock: 6am Monday must not resolve
    // to 8am the same morning.
    expect(nextWeek(at(2026, 8, 3, 6))).toBe(at(2026, 8, 10, SNOOZE_MORNING_HOUR));
  });

  it("crosses a month boundary", () => {
    // Monday 31 August 2026 → Monday 7 September.
    expect(nextWeek(at(2026, 8, 31, 9))).toBe(at(2026, 9, 7, SNOOZE_MORNING_HOUR));
  });
});

describe("nextWeekdayAt", () => {
  it("never returns today", () => {
    for (let day = 3; day <= 9; day += 1) {
      const now = at(2026, 8, day, 12);
      for (let weekday = 0; weekday < 7; weekday += 1) {
        const result = nextWeekdayAt(now, weekday, SNOOZE_MORNING_HOUR);
        expect(new Date(result).getDay()).toBe(weekday);
        expect(result).toBeGreaterThan(now);
      }
    }
  });
});

describe("the option list", () => {
  it("offers all five on a weekday morning, in order of distance", () => {
    const options = snoozeOptions(MON);
    expect(options.map((o) => o.id)).toEqual([
      "later-today",
      "tomorrow",
      "this-weekend",
      "next-week",
      "custom",
    ]);
  });

  it("drops 'later today' late at night", () => {
    const options = snoozeOptions(at(2026, 8, 3, 23));
    expect(options.map((o) => o.id)).toEqual([
      "tomorrow",
      "this-weekend",
      "next-week",
      "custom",
    ]);
  });

  it("drops 'this weekend' at the weekend", () => {
    expect(snoozeOptions(SAT).map((o) => o.id)).toEqual([
      "later-today",
      "tomorrow",
      "next-week",
      "custom",
    ]);
  });

  it("drops 'this weekend' on a Friday, where it is just tomorrow", () => {
    // Both resolve to Saturday 8am. The nearer, more concrete name wins.
    expect(snoozeOptions(FRI).map((o) => o.id)).toEqual([
      "later-today",
      "tomorrow",
      "next-week",
      "custom",
    ]);
  });

  it("drops 'next week' on a Sunday, where it is just tomorrow", () => {
    // The week starts Monday, so on a Sunday both are Monday 8am.
    expect(snoozeOptions(SUN).map((o) => o.id)).toEqual([
      "later-today",
      "tomorrow",
      "custom",
    ]);
  });

  it("drops both the late option and the duplicate on a Sunday night", () => {
    expect(snoozeOptions(at(2026, 8, 9, 23)).map((o) => o.id)).toEqual([
      "tomorrow",
      "custom",
    ]);
  });

  it("never offers the same instant twice, on any day or hour", () => {
    for (let day = 3; day <= 9; day += 1) {
      for (let hour = 0; hour < 24; hour += 1) {
        const times = snoozeOptions(at(2026, 8, day, hour))
          .map((o) => o.at)
          .filter((t): t is number => t !== null);
        expect(new Set(times).size).toBe(times.length);
      }
    }
  });

  it("resolves every offered instant except the one that is typed", () => {
    for (const option of snoozeOptions(WED)) {
      if (option.id === "custom") expect(option.at).toBeNull();
      else expect(option.at).toBeGreaterThan(WED);
    }
  });

  it("puts tomorrow at the morning hour", () => {
    const tomorrow = snoozeOptions(WED).find((o) => o.id === "tomorrow");
    expect(tomorrow?.at).toBe(at(2026, 8, 6, SNOOZE_MORNING_HOUR));
  });

  it("keeps the instants strictly increasing down the list", () => {
    for (let day = 3; day <= 9; day += 1) {
      for (let hour = 0; hour < 24; hour += 1) {
        const times = snoozeOptions(at(2026, 8, day, hour))
          .map((o) => o.at)
          .filter((t): t is number => t !== null);
        for (let i = 1; i < times.length; i += 1) {
          expect(times[i]).toBeGreaterThan(times[i - 1]!);
        }
      }
    }
  });

  it("only ever ends with the typed option", () => {
    for (let day = 3; day <= 9; day += 1) {
      const options = snoozeOptions(at(2026, 8, day, 12));
      expect(options[options.length - 1]?.id).toBe("custom");
      expect(options.filter((o) => o.at === null)).toHaveLength(1);
    }
  });
});

describe("the typed option", () => {
  it("reads a weekday and a time", () => {
    // From Wednesday 5 August, "next tuesday" is the 11th.
    expect(parseSnoozeAt("next tuesday 9am", WED)).toBe(at(2026, 8, 11, 9));
  });

  it("reads a bare weekday and wakes it at the morning hour", () => {
    expect(parseSnoozeAt("friday", WED)).toBe(at(2026, 8, 7, SNOOZE_MORNING_HOUR));
  });

  it("reads tomorrow", () => {
    expect(parseSnoozeAt("tomorrow", WED)).toBe(at(2026, 8, 6, SNOOZE_MORNING_HOUR));
    expect(parseSnoozeAt("tomorrow at 6pm", WED)).toBe(at(2026, 8, 6, 18));
  });

  it("reads an explicit date", () => {
    expect(parseSnoozeAt("august 20 at 10am", WED)).toBe(at(2026, 8, 20, 10));
  });

  it("takes a bare 1-6 to mean the afternoon, as the event field does", () => {
    // The convention is shared with calendar-nlp and datetime-field: nobody
    // asks a mail client to wake a thread at three in the morning.
    expect(parseSnoozeAt("tuesday at 3", WED)).toBe(at(2026, 8, 11, 15));
    expect(parseSnoozeAt("tuesday at 9", WED)).toBe(at(2026, 8, 11, 9));
  });

  it("respects an explicit meridiem over the afternoon rule", () => {
    expect(parseSnoozeAt("tuesday at 3am", WED)).toBe(at(2026, 8, 11, 3));
  });

  it("has no answer for an empty or unparseable field", () => {
    expect(parseSnoozeAt("", WED)).toBeNull();
    expect(parseSnoozeAt("   ", WED)).toBeNull();
    expect(parseSnoozeAt("banana", WED)).toBeNull();
  });

  it("refuses an instant that has already gone", () => {
    expect(parseSnoozeAt("yesterday", WED)).toBeNull();
    expect(parseSnoozeAt("august 1 2020 at 9am", WED)).toBeNull();
    expect(parseSnoozeAt("2 hours ago", WED)).toBeNull();
  });

  it("rolls a bare date forward rather than calling it past", () => {
    // `forwardDate` is the same setting the event field uses: a date with no
    // year is a date ahead of you. August 1 in an August-5 week is next year's.
    expect(parseSnoozeAt("august 1 at 9am", WED)).toBe(at(2027, 8, 1, 9));
  });

  it("refuses this very moment", () => {
    expect(parseSnoozeAt("august 5 2026 at 2:20pm", WED)).toBeNull();
  });
});

describe("phrasing an instant", () => {
  it("says Today for today", () => {
    expect(snoozeWhen(at(2026, 8, 5, 17), WED)).toBe("Today, 5:00 PM");
  });

  it("says Tomorrow for tomorrow", () => {
    expect(snoozeWhen(at(2026, 8, 6, 8), WED)).toBe("Tomorrow, 8:00 AM");
  });

  it("names the weekday inside the week", () => {
    expect(snoozeWhen(at(2026, 8, 8, 8), WED)).toBe("Sat, 8:00 AM");
    expect(snoozeWhen(at(2026, 8, 10, 8), WED)).toBe("Mon, 8:00 AM");
  });

  it("adds the date once a week is up", () => {
    expect(snoozeWhen(at(2026, 8, 12, 8), WED)).toBe("Wed, Aug 12, 8:00 AM");
  });

  it("adds the year once it changes", () => {
    expect(snoozeWhen(at(2027, 1, 4, 8), WED)).toBe("Mon, Jan 4 2027, 8:00 AM");
  });

  it("labels the undo entry with the count and the instant", () => {
    expect(snoozeLabel(1, at(2026, 8, 6, 8), WED)).toBe(
      "Snoozed 1 conversation until Tomorrow, 8:00 AM",
    );
    expect(snoozeLabel(3, at(2026, 8, 6, 8), WED)).toBe(
      "Snoozed 3 conversations until Tomorrow, 8:00 AM",
    );
  });
});

describe("the cursor", () => {
  it("wraps in both directions", () => {
    expect(moveSnoozeCursor(0, 1, 5)).toBe(1);
    expect(moveSnoozeCursor(4, 1, 5)).toBe(0);
    expect(moveSnoozeCursor(0, -1, 5)).toBe(4);
  });

  it("does not divide by an empty list", () => {
    expect(moveSnoozeCursor(0, 1, 0)).toBe(0);
  });
});
