/**
 * No command may reach Google without saying what it did first.
 *
 * # Why this exists
 *
 * `projection.ts` was written for mail, and for a year it covered mail only.
 * Five calendar commands — `rsvp`, `createEvent`, `updateEvent`, `deleteEvent`,
 * `moveEvent` — were dispatched by a second write path that executed against
 * the data source directly, so none of them was optimistic and nobody noticed,
 * because nothing anywhere said they had to be. Answering "Going" from the
 * right-click menu changed nothing on screen until Google replied.
 *
 * The gap was not a bug in any one of those five. It was that a command could
 * be added, or a whole half of the vocabulary could go unprojected, and the
 * only thing that would ever notice was somebody using the app.
 *
 * # The rule
 *
 * Two claims, and they close the loop between them:
 *
 *  1. **Every `Command` variant is projected, or exempt with a reason.** The
 *     union is read out of `data.ts` rather than imported, so adding a variant
 *     fails here by name before it is dispatched anywhere.
 *  2. **Only the files that project may execute.** A projection nothing goes
 *     through is decoration; a third `getDataSource().execute` call site is how
 *     the calendar came to have its own unprojected write path in the first
 *     place.
 *
 * An honest exemption is possible — `createEvent` is one, and says why. Silence
 * is not: an unlisted, unprojected command fails, and so does an exemption for
 * a command that no longer exists.
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { project, projectEvent } from "./projection";
import type { Command, CommandKind } from "./data";

const ROOT = fileURLToPath(new URL("..", import.meta.url));

/**
 * One of each, for the projection to be asked about.
 *
 * Typed `Record<CommandKind, Command>`, so a new variant that nobody adds here
 * is a type error as well as a failure below — two ways to find out, and the
 * type error is the faster one.
 */
const SAMPLES: Record<CommandKind, Command> = {
  archive: { kind: "archive", threadIds: [1] },
  unarchive: { kind: "unarchive", threadIds: [1] },
  markRead: { kind: "markRead", threadIds: [1], read: true },
  star: { kind: "star", threadIds: [1], starred: true },
  label: { kind: "label", threadIds: [1], labelId: "Label_1", add: true },
  reportSpam: { kind: "reportSpam", threadIds: [1] },
  notSpam: { kind: "notSpam", threadIds: [1] },
  trash: { kind: "trash", threadIds: [1] },
  untrash: { kind: "untrash", threadIds: [1] },
  snooze: { kind: "snooze", threadIds: [1], until: 1 },
  unsnooze: { kind: "unsnooze", threadIds: [1] },
  unsubscribe: { kind: "unsubscribe", messageId: 1 },
  rsvp: { kind: "rsvp", eventId: 1, response: "accepted" },
  createEvent: {
    kind: "createEvent",
    accountId: 1,
    calendarId: "primary",
    draft: {
      title: "Standup",
      startTs: 1,
      endTs: 2,
      isAllDay: false,
      attendees: [],
      recurrence: [],
    },
  },
  updateEvent: { kind: "updateEvent", eventId: 1, patch: { title: "Renamed" } },
  deleteEvent: { kind: "deleteEvent", eventId: 1 },
  moveEvent: { kind: "moveEvent", eventId: 1, accountId: 2, calendarId: "other" },
};

/**
 * Commands with no guess, and why each one has none.
 *
 * A reason, not a checkbox. Every entry here is a claim that there is nothing
 * a guess *could* say — not that saying it was inconvenient.
 */
const NOT_PROJECTED: Partial<Record<CommandKind, string>> = {
  createEvent:
    "There is no id to key a guess by until the command layer has minted one. " +
    "`run` draws the block from the draft the command already carries — see " +
    "`placeholderEvent` and `settledPendingEvents` — which is the same claim " +
    "made the only way a create can make it.",
  unsubscribe:
    "Unsubscribe changes no local row, because it is an outbound request to " +
    "the sender rather than a change to the mailbox, so there is nothing to " +
    "project and nothing to roll back. What the gesture does put on screen is " +
    "the archive it fires alongside — see `unsubscribe` in `useMach` — and " +
    "that command is projected in the ordinary way.",
};

/**
 * The files allowed to call `execute` on the data source.
 *
 * Exactly one today, and that is the point of the list rather than an accident
 * of it: `useMach`'s `run` is where a command is projected before the first
 * `await`, so a caller that goes around it goes around that. Everything else
 * dispatches through `actions.execute`.
 *
 * Adding a file here is a claim that it projects the command itself, in the
 * same tick, and rolls the guess back when the write is refused.
 */
const MAY_EXECUTE = new Set(["hooks/useMach.tsx"]);

/** The `kind` literals of the `Command` union, read out of its declaration. */
function commandKinds(): string[] {
  const source = readFileSync(join(ROOT, "lib/data.ts"), "utf8");
  const start = source.indexOf("export type Command =");
  expect(start, "the Command union should still be declared in lib/data.ts").toBeGreaterThan(-1);
  const end = source.indexOf("export type CommandKind", start);
  const union = source.slice(start, end);
  return [...union.matchAll(/kind:\s*"([a-zA-Z]+)"/g)].map((m) => m[1]!);
}

/** Every `.ts`/`.tsx` under `src`, tests excluded, as repo-relative paths. */
function sources(dir: string, found: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) sources(path, found);
    else if (/\.tsx?$/.test(entry) && !/\.test\.tsx?$/.test(entry)) found.push(path);
  }
  return found;
}

describe("every command", () => {
  it("is one this file knows about", () => {
    // The union is the authority. If it grew, `SAMPLES` has to grow with it —
    // and the sample is what the projection is then asked about below.
    expect([...commandKinds()].sort()).toEqual(Object.keys(SAMPLES).sort());
  });

  it("is projected, or is exempt with a reason", () => {
    const unprojected: string[] = [];

    for (const [kind, command] of Object.entries(SAMPLES) as [CommandKind, Command][]) {
      // `[]` for the rows: the two commands that read them fall back to a
      // delta when the conversation is not loaded, which is the case here.
      const guessed = project(command, []) !== null || projectEvent(command) !== null;
      const excused = NOT_PROJECTED[kind];

      if (!guessed && !excused) {
        unprojected.push(
          `${kind} changes nothing on screen until Google answers. ` +
            "Give it a guess in lib/projection.ts, or list it in NOT_PROJECTED with the reason it cannot have one.",
        );
      }
      if (guessed && excused) {
        unprojected.push(`${kind} is projected and also listed as exempt — drop the exemption.`);
      }
    }

    expect(unprojected).toEqual([]);
  });

  it("has no exemption left behind for a command that is gone", () => {
    for (const kind of Object.keys(NOT_PROJECTED)) {
      expect(SAMPLES, `NOT_PROJECTED names ${kind}, which is not a command`).toHaveProperty(kind);
    }
  });

  it("has a real sentence behind every exemption", () => {
    for (const [kind, reason] of Object.entries(NOT_PROJECTED)) {
      expect(reason!.length, `the exemption for ${kind} needs to say why`).toBeGreaterThan(40);
    }
  });
});

describe("the write path", () => {
  it("is the only place a command is executed", () => {
    const offenders: string[] = [];

    for (const path of sources(ROOT)) {
      const relative = path.slice(ROOT.length).replace(/\\/g, "/");
      if (MAY_EXECUTE.has(relative)) continue;
      const source = readFileSync(path, "utf8");
      // The data source's own declaration and the fixture implementation of it
      // are not call sites.
      if (relative === "lib/data.ts" || relative === "lib/ipc.ts") continue;
      if (/getDataSource\(\)\s*\.?\s*\n?\s*\.execute\(/.test(source)) {
        offenders.push(
          `${relative} executes a command directly, so nothing projects it. ` +
            "Use actions.execute, or add the file to MAY_EXECUTE and say what it projects.",
        );
      }
    }

    expect(offenders).toEqual([]);
  });
});
