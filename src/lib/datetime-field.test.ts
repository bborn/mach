import { describe, expect, it } from "vitest";
import {
  addDaysToValue,
  addMonthsToValue,
  dateFromValue,
  dateValue,
  formatDateField,
  formatTimeField,
  parseDateField,
  parseTimeField,
  timeChoices,
} from "./datetime-field";

/** Wed 5 Aug 2026, 10:00 local — the week the seed store sits in. */
const NOW = new Date(2026, 7, 5, 10, 0, 0).getTime();

describe("dateValue / dateFromValue", () => {
  it("round-trips a local date", () => {
    expect(dateValue(new Date(2026, 7, 5))).toBe("2026-08-05");
    expect(dateFromValue("2026-08-05")?.getDate()).toBe(5);
  });

  it("rejects a date that does not exist", () => {
    expect(dateFromValue("2026-02-30")).toBeNull();
    expect(dateFromValue("2026-13-01")).toBeNull();
    expect(dateFromValue("nonsense")).toBeNull();
  });
});

describe("formatDateField", () => {
  it("spells the date the way the app spells every other date", () => {
    expect(formatDateField("2026-08-05")).toBe("Wed, Aug 5, 2026");
  });

  it("leaves anything that is not a date alone", () => {
    expect(formatDateField("half past never")).toBe("half past never");
  });

  it("round-trips: what it shows, it can read back", () => {
    for (const value of ["2026-01-01", "2026-08-05", "2027-12-31", "2028-02-29"]) {
      expect(parseDateField(formatDateField(value), { now: NOW })).toBe(value);
    }
  });
});

describe("parseDateField", () => {
  it("takes an ISO date", () => {
    expect(parseDateField("2026-08-05", { now: NOW })).toBe("2026-08-05");
  });

  it("takes the numeric forms people actually type", () => {
    expect(parseDateField("8/12", { now: NOW })).toBe("2026-08-12");
    expect(parseDateField("8/12/27", { now: NOW })).toBe("2027-08-12");
    expect(parseDateField("08.12.2027", { now: NOW })).toBe("2027-08-12");
    expect(parseDateField("8-12", { now: NOW })).toBe("2026-08-12");
  });

  it("takes a bare day of the month", () => {
    expect(parseDateField("12", { now: NOW })).toBe("2026-08-12");
  });

  it("reads a numeric date against the year already in the field", () => {
    expect(parseDateField("3/2", { now: NOW, current: "2029-11-04" })).toBe("2029-03-02");
    expect(parseDateField("9", { now: NOW, current: "2029-11-04" })).toBe("2029-11-09");
  });

  it("takes words, and a bare weekday means the next one", () => {
    expect(parseDateField("aug 12", { now: NOW })).toBe("2026-08-12");
    expect(parseDateField("tomorrow", { now: NOW })).toBe("2026-08-06");
    expect(parseDateField("friday", { now: NOW })).toBe("2026-08-07");
    expect(parseDateField("next monday", { now: NOW })).toBe("2026-08-10");
  });

  it("refuses a day that does not exist rather than rolling it over", () => {
    expect(parseDateField("2/30", { now: NOW })).toBeNull();
    expect(parseDateField("13/1", { now: NOW })).toBeNull();
  });

  it("returns null for nothing, and for nonsense", () => {
    expect(parseDateField("", { now: NOW })).toBeNull();
    expect(parseDateField("   ", { now: NOW })).toBeNull();
    expect(parseDateField("qqq", { now: NOW })).toBeNull();
  });
});

describe("parseTimeField", () => {
  it("takes 24-hour and explicit meridiem", () => {
    expect(parseTimeField("21:30")).toBe("21:30");
    expect(parseTimeField("9:30 pm")).toBe("21:30");
    expect(parseTimeField("9:30pm")).toBe("21:30");
    expect(parseTimeField("9:30 p.m.")).toBe("21:30");
    expect(parseTimeField("9a")).toBe("09:00");
    expect(parseTimeField("9 AM")).toBe("09:00");
  });

  it("gets midnight and noon the right way round", () => {
    expect(parseTimeField("12:00 am")).toBe("00:00");
    expect(parseTimeField("12:00 pm")).toBe("12:00");
    expect(parseTimeField("noon")).toBe("12:00");
    expect(parseTimeField("midnight")).toBe("00:00");
  });

  it("assumes the afternoon for a bare 1 to 6", () => {
    expect(parseTimeField("1")).toBe("13:00");
    expect(parseTimeField("6")).toBe("18:00");
    expect(parseTimeField("1:30")).toBe("13:30");
    expect(parseTimeField("7")).toBe("07:00");
    expect(parseTimeField("9")).toBe("09:00");
    expect(parseTimeField("12")).toBe("12:00");
  });

  it("takes the four-digit shorthand", () => {
    expect(parseTimeField("930")).toBe("09:30");
    expect(parseTimeField("1430")).toBe("14:30");
    expect(parseTimeField("0930")).toBe("09:30");
  });

  it("returns null for an impossible time", () => {
    expect(parseTimeField("25:00")).toBeNull();
    expect(parseTimeField("9:99")).toBeNull();
    expect(parseTimeField("13 pm")).toBeNull();
    expect(parseTimeField("")).toBeNull();
  });

  it("round-trips what the field displays", () => {
    for (const value of ["00:00", "09:05", "12:00", "12:30", "13:45", "23:59"]) {
      expect(parseTimeField(formatTimeField(value))).toBe(value);
    }
  });
});

describe("formatTimeField", () => {
  it("shows a twelve-hour clock", () => {
    expect(formatTimeField("00:00")).toBe("12:00 AM");
    expect(formatTimeField("12:30")).toBe("12:30 PM");
    expect(formatTimeField("13:05")).toBe("1:05 PM");
  });

  it("leaves a non-time alone", () => {
    expect(formatTimeField("soon")).toBe("soon");
    expect(formatTimeField("31:00")).toBe("31:00");
  });
});

describe("timeChoices", () => {
  it("covers the day at the given step", () => {
    const choices = timeChoices(30);
    expect(choices).toHaveLength(48);
    expect(choices[0]).toBe("00:00");
    expect(choices[1]).toBe("00:30");
    expect(choices[choices.length - 1]).toBe("23:30");
  });
});

describe("month and day arithmetic", () => {
  it("clamps the end of the month", () => {
    expect(addMonthsToValue("2026-01-31", 1)).toBe("2026-02-28");
    expect(addMonthsToValue("2026-03-15", -1)).toBe("2026-02-15");
  });

  it("crosses a year", () => {
    expect(addMonthsToValue("2026-12-15", 1)).toBe("2027-01-15");
    expect(addDaysToValue("2026-12-31", 1)).toBe("2027-01-01");
    expect(addDaysToValue("2026-08-05", -7)).toBe("2026-07-29");
  });
});
