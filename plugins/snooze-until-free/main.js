/**
 * Snooze until the next gap in the calendar.
 *
 * "Snooze until tomorrow 9am" is a lie on a day that is already full. This
 * finds the next stretch of time the user could actually read the thing in, and
 * wakes the conversation then.
 *
 * Two decisions worth knowing about:
 *
 *   - Free means free in *working hours*, not free at 3am. A gap that starts
 *     before the working day is clamped to its start.
 *   - All-day events do not make the day busy. Half of them are "Anna OOO" and
 *     the rest are birthdays; treating them as blocking would push every snooze
 *     into next week.
 */

const MIN = 60_000;
const DAY = 24 * 60 * MIN;

const DEFAULT_SETTINGS = {
  /** Local hours, [start, end). */
  workday: [9, 18],
  /** 1 = Monday … 7 = Sunday. */
  workdays: [1, 2, 3, 4, 5],
  minimumMinutes: 45,
  /** Never wake something in the next N minutes; you are busy now. */
  leadMinutes: 60,
};

/** How far ahead to look before giving up and saying so. */
const HORIZON_DAYS = 21;

export const actions = {
  async snooze({ mach, threadIds, params }) {
    if (threadIds.length === 0) {
      mach.notify("Nothing selected");
      return;
    }

    const settings = await loadSettings(mach);
    const minimum = (params?.minimumMinutes ?? settings.minimumMinutes) * MIN;
    const slot = await nextFreeSlot(mach, { ...settings, minimum });

    if (!slot) {
      mach.notify(`No free ${Math.round(minimum / MIN)} minutes in the next ${HORIZON_DAYS} days`, "error");
      return;
    }

    await mach.run({ kind: "snooze", threadIds, until: slot.start });
    mach.notify(`Back at ${formatWhen(slot.start, mach.now())}`);
  },

  async configure({ mach }) {
    const settings = await loadSettings(mach);
    const answer = await mach.ask.text({
      title: "Working hours",
      placeholder: "9-18",
      initial: `${settings.workday[0]}-${settings.workday[1]}`,
    });
    if (answer === null) return;

    const [start, end] = answer.split("-").map((n) => Number(n.trim()));
    if (!Number.isInteger(start) || !Number.isInteger(end) || start >= end || end > 24) {
      mach.notify("Working hours look like 9-18", "error");
      return;
    }
    await mach.store.set("settings", { ...settings, workday: [start, end] });
    mach.notify(`Working hours set to ${start}:00–${end}:00`);
  },
};

export const views = {
  /**
   * A line above the composer telling the user what ⇧H would actually do.
   *
   * It is the whole value of the plugin made visible: "snooze" is a guess until
   * you can see the time it resolves to.
   */
  async "next-free"({ mach, threadId }) {
    if (threadId == null) return null;

    const settings = await loadSettings(mach);
    const slot = await nextFreeSlot(mach, { ...settings, minimum: settings.minimumMinutes * MIN });
    if (!slot) return null;

    return {
      type: "section",
      children: [
        {
          type: "row",
          label: "Next free",
          value: formatWhen(slot.start, mach.now()),
          tone: slot.start - mach.now() > 3 * DAY ? "warning" : "default",
        },
        { type: "button", label: "Snooze until then", action: "snooze" },
      ],
    };
  },
};

/* -------------------------------------------------------------------------- */
/* The actual work                                                             */
/* -------------------------------------------------------------------------- */

/**
 * The first working-hours window of at least `minimum` with nothing in it.
 *
 * One calendar read for the whole horizon rather than a read per day: the
 * events come back sorted and merged, and walking them once is simpler than
 * asking the same question twenty-one times.
 */
async function nextFreeSlot(mach, settings) {
  const now = mach.now();
  const from = now + settings.leadMinutes * MIN;
  const to = from + HORIZON_DAYS * DAY;

  const events = await mach.read.events({ start: from, end: to });
  const busy = merge(
    events
      // An all-day block is a label, not an appointment. See the module note.
      .filter((e) => !e.isAllDay && e.rsvp !== "declined")
      .map((e) => [e.start, e.end])
      .sort((a, b) => a[0] - b[0]),
  );

  for (const window of workingWindows(from, to, settings)) {
    for (const gap of subtract(window, busy)) {
      if (gap[1] - gap[0] >= settings.minimum) {
        return { start: gap[0], end: gap[1] };
      }
    }
  }
  return null;
}

/** Overlapping or touching intervals, collapsed. */
function merge(intervals) {
  const out = [];
  for (const [start, end] of intervals) {
    const last = out[out.length - 1];
    if (last && start <= last[1]) last[1] = Math.max(last[1], end);
    else out.push([start, end]);
  }
  return out;
}

/** `window` minus everything in `busy`, in order. */
function subtract([start, end], busy) {
  const out = [];
  let cursor = start;
  for (const [bStart, bEnd] of busy) {
    if (bEnd <= cursor) continue;
    if (bStart >= end) break;
    if (bStart > cursor) out.push([cursor, Math.min(bStart, end)]);
    cursor = Math.max(cursor, bEnd);
    if (cursor >= end) return out;
  }
  if (cursor < end) out.push([cursor, end]);
  return out;
}

/** Each working day's [start, end) inside the horizon, clamped to `from`. */
function* workingWindows(from, to, settings) {
  const [openHour, closeHour] = settings.workday;
  for (let day = startOfDay(from); day < to; day += DAY) {
    const date = new Date(day);
    const weekday = date.getDay() === 0 ? 7 : date.getDay();
    if (!settings.workdays.includes(weekday)) continue;

    const open = Math.max(from, atHour(day, openHour));
    const close = Math.min(to, atHour(day, closeHour));
    if (close > open) yield [open, close];
  }
}

function startOfDay(ms) {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

function atHour(dayStart, hour) {
  const d = new Date(dayStart);
  d.setHours(hour, 0, 0, 0);
  return d.getTime();
}

async function loadSettings(mach) {
  return { ...DEFAULT_SETTINGS, ...((await mach.store.get("settings")) ?? {}) };
}

/** "Thu 2:30 PM", or "Tomorrow 9:00 AM" when that reads better. */
function formatWhen(ts, now) {
  const days = Math.round((startOfDay(ts) - startOfDay(now)) / DAY);
  const time = new Date(ts).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  if (days === 0) return `today ${time}`;
  if (days === 1) return `tomorrow ${time}`;
  if (days < 7) return `${new Date(ts).toLocaleDateString(undefined, { weekday: "long" })} ${time}`;
  return `${new Date(ts).toLocaleDateString(undefined, { month: "short", day: "numeric" })} ${time}`;
}
