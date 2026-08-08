import { describe, expect, it } from "vitest";
import { assignCalendarHues, nearestRampSlot, oklchHue } from "./colors";
import { assignHues, CALENDAR_HUES } from "./calendar-palette";

describe("reading a hue out of Google's hex", () => {
  it("puts the primaries where a person would put them", () => {
    // OKLCH hue angles, not HSL ones: red sits near 29°, green near 142°, blue
    // near 264°. These are the anchors everything else is judged against.
    expect(oklchHue("#ff0000")).toBeCloseTo(29.2, 0);
    expect(oklchHue("#00ff00")).toBeCloseTo(142.5, 0);
    expect(oklchHue("#0000ff")).toBeCloseTo(264.1, 0);
  });

  it("accepts the shorthand and ignores the leading hash", () => {
    expect(oklchHue("#f00")).toBeCloseTo(oklchHue("#ff0000")!, 5);
    expect(oklchHue("ff0000")).toBeCloseTo(oklchHue("#ff0000")!, 5);
  });

  it("refuses to invent a hue for a neutral", () => {
    // A grey has no hue; picking one would land a monochrome calendar on an
    // arbitrary colour that looks deliberate and is not.
    expect(oklchHue("#808080")).toBeNull();
    expect(oklchHue("#ffffff")).toBeNull();
    expect(oklchHue("#000000")).toBeNull();
  });

  it("refuses anything that is not a colour", () => {
    expect(oklchHue("")).toBeNull();
    expect(oklchHue("rebeccapurple")).toBeNull();
    expect(oklchHue("#12345")).toBeNull();
  });
});

describe("mapping a colour onto the app's ramp", () => {
  it("picks the neighbouring hue, not a hue on the other side of the wheel", () => {
    // Google's "Tomato" is a red; the ramp's 25° slot is the red end of it.
    expect(nearestRampSlot("#d50000", CALENDAR_HUES)).toBe(0);
    // "Basil" is a green — the 160° slot, three along.
    expect(nearestRampSlot("#0b8043", CALENDAR_HUES)).toBe(3);
    // "Blueberry" is a blue-violet, which is the 250° slot.
    expect(nearestRampSlot("#3f51b5", CALENDAR_HUES)).toBe(5);
  });

  it("measures distance around the wheel rather than along a number line", () => {
    // This deep magenta lands at 0.9°, which is 21° from the 340° slot and 24°
    // from the 25° one — so the right answer is 340, and a metric that
    // subtracted the raw angles would make it look 339° away and pick 25.
    expect(oklchHue("#880044")).toBeLessThan(2);
    expect(nearestRampSlot("#880044", CALENDAR_HUES)).toBe(7);
    // A hue further into the reds crosses back over and takes 25.
    expect(nearestRampSlot("#c2185b", CALENDAR_HUES)).toBe(0);
  });

  it("has no answer for a grey", () => {
    expect(nearestRampSlot("#9e9e9e", CALENDAR_HUES)).toBeNull();
  });
});

describe("assigning hues across a set of calendars", () => {
  it("gives a calendar the hue its owner chose", () => {
    const hues = assignCalendarHues(
      [
        { id: "family", backgroundColor: "#d50000" },
        { id: "work", backgroundColor: "#0b8043" },
      ],
      CALENDAR_HUES,
      assignHues,
    );
    expect(hues.get("family")).toBe(0);
    expect(hues.get("work")).toBe(3);
  });

  it("lets two calendars share a colour when the user gave them one", () => {
    // Second-guessing this would move a calendar off the colour it was set to,
    // which is a worse answer than two calendars looking alike — Google itself
    // draws them alike.
    const hues = assignCalendarHues(
      [
        { id: "a", backgroundColor: "#d50000" },
        { id: "b", backgroundColor: "#d50000" },
      ],
      CALENDAR_HUES,
      assignHues,
    );
    expect(hues.get("a")).toBe(hues.get("b"));
  });

  it("falls back to the hash for a calendar with no colour", () => {
    const hues = assignCalendarHues([{ id: "team" }], CALENDAR_HUES, assignHues);
    expect(hues.get("team")).toBe(assignHues(["team"]).get("team"));
  });

  it("keeps an uncoloured calendar off a slot a coloured one has taken", () => {
    // The uncoloured one moves, never the coloured one: only one of the two is
    // a choice somebody made.
    const red = "#d50000";
    const collides = [...Array(40).keys()]
      .map((n) => `cal-${n}`)
      .find((id) => assignHues([id]).get(id) === nearestRampSlot(red, CALENDAR_HUES));
    expect(collides).toBeDefined();

    const hues = assignCalendarHues(
      [{ id: "chosen", backgroundColor: red }, { id: collides! }],
      CALENDAR_HUES,
      assignHues,
    );
    expect(hues.get("chosen")).toBe(nearestRampSlot(red, CALENDAR_HUES));
    expect(hues.get(collides!)).not.toBe(hues.get("chosen"));
  });

  it("does not depend on the order sync happened to return calendars in", () => {
    const calendars = [
      { id: "b", backgroundColor: "#0b8043" },
      { id: "a" },
      { id: "c", backgroundColor: "#3f51b5" },
    ];
    const forwards = assignCalendarHues(calendars, CALENDAR_HUES, assignHues);
    const backwards = assignCalendarHues([...calendars].reverse(), CALENDAR_HUES, assignHues);
    expect([...forwards.entries()].sort()).toEqual([...backwards.entries()].sort());
  });

  it("treats a colour it cannot read as no colour at all", () => {
    const hues = assignCalendarHues(
      [{ id: "grey", backgroundColor: "#808080" }],
      CALENDAR_HUES,
      assignHues,
    );
    expect(hues.get("grey")).toBe(assignHues(["grey"]).get("grey"));
  });
});
