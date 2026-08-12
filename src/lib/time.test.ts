import { describe, expect, it } from "vitest";
import { clockTime, dayLabel, listTime, messageTime } from "./time";

/*
 * Every case pins `now` explicitly. A time formatter tested against the wall
 * clock passes all morning and fails at midnight on the machine that runs CI,
 * which is the one hour of the day these functions are most likely to be wrong.
 */

const now = at(2026, 7, 12, 14, 30); // Wed 12 Aug 2026, 2:30 PM

/** Local time, the way every one of these formatters reads a timestamp. */
function at(year: number, month: number, day: number, hour = 0, minute = 0): number {
  return new Date(year, month, day, hour, minute).getTime();
}

describe("dayLabel", () => {
  it("returns null for today, so the caller decides what to draw instead", () => {
    expect(dayLabel(at(2026, 7, 12, 7, 8), now)).toBeNull();
    expect(dayLabel(at(2026, 7, 12, 0, 0), now)).toBeNull();
    expect(dayLabel(at(2026, 7, 12, 23, 59), now)).toBeNull();
  });

  it("names yesterday, then the weekday, then the date, then the year", () => {
    expect(dayLabel(at(2026, 7, 11, 9, 0), now)).toBe("Yesterday");
    expect(dayLabel(at(2026, 7, 10, 9, 0), now)).toBe("Mon");
    expect(dayLabel(at(2026, 7, 6, 9, 0), now)).toBe("Thu"); // six days back, the last weekday
    expect(dayLabel(at(2026, 7, 5, 9, 0), now)).toBe("Aug 5");
    expect(dayLabel(at(2025, 7, 10, 9, 0), now)).toBe("8/10/25");
  });
});

describe("listTime", () => {
  it("is the clock today and the day name after that", () => {
    expect(listTime(at(2026, 7, 12, 7, 8), now)).toBe("7:08 AM");
    expect(listTime(at(2026, 7, 11, 23, 59), now)).toBe("Yesterday");
    expect(listTime(at(2026, 7, 10, 9, 0), now)).toBe("Mon");
    expect(listTime(at(2026, 7, 5, 9, 0), now)).toBe("Aug 5");
    expect(listTime(at(2025, 11, 24, 9, 0), now)).toBe("12/24/25");
  });
});

describe("messageTime", () => {
  it("is a bare clock for today", () => {
    expect(messageTime(at(2026, 7, 12, 7, 8), now)).toBe("7:08 AM");
    expect(messageTime(at(2026, 7, 12, 12, 5), now)).toBe("12:05 PM");
    expect(messageTime(at(2026, 7, 12, 0, 1), now)).toBe("12:01 AM");
  });

  it("puts the day in front of the clock for yesterday", () => {
    expect(messageTime(at(2026, 7, 11, 19, 4), now)).toBe("Yesterday 7:04 PM");
  });

  it("uses the weekday for the rest of the week behind us", () => {
    expect(messageTime(at(2026, 7, 10, 10, 34), now)).toBe("Mon 10:34 AM");
    expect(messageTime(at(2026, 7, 6, 10, 34), now)).toBe("Thu 10:34 AM");
  });

  it("uses a date within the year and a short date before it", () => {
    expect(messageTime(at(2026, 7, 5, 10, 34), now)).toBe("Aug 5 10:34 AM");
    expect(messageTime(at(2026, 0, 3, 8, 15), now)).toBe("Jan 3 8:15 AM");
    expect(messageTime(at(2024, 10, 30, 16, 45), now)).toBe("11/30/24 4:45 PM");
  });

  it("keeps the clock last, so the column's clocks line up under each other", () => {
    const stamps = [
      messageTime(at(2026, 7, 12, 7, 8), now),
      messageTime(at(2026, 7, 11, 12, 14), now),
      messageTime(at(2026, 7, 10, 10, 34), now),
    ];
    for (const stamp of stamps) {
      expect(stamp).toMatch(/\d{1,2}:\d{2} [AP]M$/);
    }
  });

  /*
   * The boundary that actually bites. Reading at 12:01 AM, a message sent two
   * minutes earlier is from a different day, and a formatter that subtracts 24
   * hours instead of comparing calendar days would call it today's — which is
   * exactly the row a reader would misdate, because "11:59 PM" beside a fresh
   * thread reads as last night either way.
   */
  it("does not call 11:59 PM yesterday today's, read at 12:01 AM", () => {
    const justAfterMidnight = at(2026, 7, 12, 0, 1);
    const justBefore = at(2026, 7, 11, 23, 59);

    expect(messageTime(justBefore, justAfterMidnight)).toBe("Yesterday 11:59 PM");
    expect(messageTime(justAfterMidnight, justAfterMidnight)).toBe("12:01 AM");
    expect(dayLabel(justBefore, justAfterMidnight)).toBe("Yesterday");
  });

  it("agrees with listTime about which day a timestamp belongs to", () => {
    for (let back = 0; back < 400; back++) {
      const ts = now - back * 86_400_000;
      const day = dayLabel(ts, now);
      expect(listTime(ts, now)).toBe(day ?? clockTime(ts));
      expect(messageTime(ts, now)).toBe(day ? `${day} ${clockTime(ts)}` : clockTime(ts));
    }
  });
});
