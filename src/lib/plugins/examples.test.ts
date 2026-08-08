/**
 * The two worked examples from `docs/plugins.md`, running.
 *
 * They are the real specification. If the implementation cannot run them **as
 * written**, the implementation is wrong — so these import the plugins' actual
 * `main.js` from `plugins/`, unmodified, and drive them through the real
 * `PluginSandbox`, the real capability check and the real `mach.*` surface built
 * by `createHostApi`. Only the transport is a stand-in, and only because a unit
 * test has no iframe; the isolation it stands in for is proved separately, in a
 * real WKWebView, by the conformance probe.
 *
 * `plugins/snooze-until-free/main.js` is extracted from the design document
 * verbatim, which is why the calendar arithmetic here is asserted against times
 * rather than against a mock: the point is that the *published* code works.
 */

import { describe, expect, it, vi } from "vitest";
import { PluginSandbox } from "./sandbox";
import { loopbackTransport } from "./loopback";
import { createHostApi, type PluginStore } from "./api";
import type { PluginManifest } from "./types";
import type { Command, CommandResult, MachDataSource } from "@/lib/data";
import { fixtureSource } from "@/lib/data";

import * as quickFile from "../../../plugins/quick-file/main.js";
import * as snoozeUntilFree from "../../../plugins/snooze-until-free/main.js";
import quickFileManifest from "../../../plugins/quick-file/mach-plugin.json";
import snoozeManifest from "../../../plugins/snooze-until-free/mach-plugin.json";

/* -------------------------------------------------------------------------- */
/* Harness                                                                     */
/* -------------------------------------------------------------------------- */

/** The manifests are Rust's shape; the files omit the fields serde defaults. */
function asManifest(raw: unknown): PluginManifest {
  const m = raw as Partial<PluginManifest> & { capabilities?: Record<string, unknown> };
  return {
    machApiProposed: [],
    runtime: "sandbox",
    description: "",
    author: "",
    ...m,
    capabilities: {
      read: [],
      commands: [],
      ui: [],
      events: [],
      store: false,
      agent: true,
      ...(m.capabilities ?? {}),
    },
    contributes: {
      actions: [],
      views: [],
      ...(m.contributes ?? {}),
    },
  } as PluginManifest;
}

function memoryStore(): PluginStore {
  const map = new Map<string, unknown>();
  return {
    async get(key) {
      return map.has(key) ? map.get(key) : null;
    },
    async set(key, value) {
      map.set(key, value);
    },
  };
}

interface Harness {
  sandbox: PluginSandbox;
  runs: Command[];
  notices: string[];
  inverses: Command[];
}

function harness(options: {
  manifest: PluginManifest;
  module: unknown;
  source?: Partial<MachDataSource>;
  ask?: Partial<Parameters<typeof createHostApi>[0]["ask"]>;
  now?: () => number;
}): Harness {
  const runs: Command[] = [];
  const notices: string[] = [];
  const inverses: Command[] = [];

  const source: MachDataSource = {
    ...fixtureSource,
    ...options.source,
    async execute(command: Command): Promise<CommandResult> {
      runs.push(command);
      return {
        ok: true,
        message: `ran ${command.kind}`,
        undo: { kind: "unarchive", threadIds: [1] },
        applied: [1],
        failed: [],
      };
    },
  };

  const api = createHostApi({
    id: options.manifest.id,
    name: options.manifest.name,
    source,
    ask: {
      async pick() {
        return null;
      },
      async text() {
        return null;
      },
      async confirm() {
        return false;
      },
      ...options.ask,
    } as Parameters<typeof createHostApi>[0]["ask"],
    notify: (message) => notices.push(message),
    log: () => {},
    onRun: (_command, result) => {
      if (result.undo) inverses.push(result.undo);
    },
    store: memoryStore(),
  });

  const sandbox = new PluginSandbox({
    manifest: options.manifest,
    transport: loopbackTransport({
      module: options.module as never,
      now: options.now,
    }),
    workerSource: "",
    api,
    timeoutMs: 1_000,
  });

  return { sandbox, runs, notices, inverses };
}

/* -------------------------------------------------------------------------- */
/* Worked example 1 — Quick File                                               */
/* -------------------------------------------------------------------------- */

describe("worked example 1 — Quick File", () => {
  const manifest = asManifest(quickFileManifest);

  it("reads labels, picks one, applies it and archives — in that order", async () => {
    const pick = vi.fn(async (o: { items: { value: unknown }[] }) => o.items[0]?.value);
    const h = harness({
      manifest,
      module: quickFile,
      source: {
        async listLabels() {
          return [
            { id: "CHAT", accountId: 1, name: "Chat", kind: "system" },
            { id: "Label_2", accountId: 1, name: "Receipts", kind: "user" },
            { id: "Label_1", accountId: 1, name: "Admin", kind: "user" },
          ];
        },
      },
      ask: { pick: pick as never },
    });

    await h.sandbox.invoke("actions", "file", { threadIds: [1, 2, 3] });

    // Only user labels are offered: filing to CHAT is not a thing anyone means.
    const offered = pick.mock.calls[0]?.[0].items as unknown as { title: string }[];
    expect(offered.map((i) => i.title)).toEqual([
      "Admin",
      "Receipts",
    ]);
    expect(h.runs).toEqual([
      { kind: "label", threadIds: [1, 2, 3], labelId: "Label_1", add: true },
      { kind: "archive", threadIds: [1, 2, 3] },
    ]);
    expect(h.notices).toEqual(["Filed 3 to Admin"]);
  });

  /** ⌘Z takes back the label *and* the archive, because both went through `run`. */
  it("produces two inverses for one gesture, so undo is one step", async () => {
    const h = harness({
      manifest,
      module: quickFile,
      source: {
        async listLabels() {
          return [{ id: "Label_1", accountId: 1, name: "Admin", kind: "user" }];
        },
      },
      ask: { pick: (async (o: { items: { value: unknown }[] }) => o.items[0]?.value) as never },
    });

    await h.sandbox.invoke("actions", "file", { threadIds: [7] });
    expect(h.inverses).toHaveLength(2);
  });

  it("does nothing when the picker is dismissed", async () => {
    const h = harness({
      manifest,
      module: quickFile,
      source: {
        async listLabels() {
          return [{ id: "Label_1", accountId: 1, name: "Admin", kind: "user" }];
        },
      },
    });
    await h.sandbox.invoke("actions", "file", { threadIds: [1] });
    expect(h.runs).toEqual([]);
  });

  it("says so when nothing is selected", async () => {
    const h = harness({ manifest, module: quickFile });
    await h.sandbox.invoke("actions", "file", { threadIds: [] });
    expect(h.notices).toEqual(["Nothing selected"]);
    expect(h.runs).toEqual([]);
  });

  it("remembers what was filed and ranks it first next time", async () => {
    const labels = [
      { id: "Label_1", accountId: 1, name: "Admin", kind: "user" as const },
      { id: "Label_2", accountId: 1, name: "Receipts", kind: "user" as const },
    ];
    const store = memoryStore();
    const seen: string[][] = [];

    for (const choose of ["Label_2", "Label_2"]) {
      const api = createHostApi({
        id: manifest.id,
        name: manifest.name,
        source: { ...fixtureSource, async listLabels() { return labels; } } as MachDataSource,
        ask: {
          async pick(o) {
            seen.push(o.items.map((i) => i.title));
            return choose;
          },
          async text() {
            return null;
          },
          async confirm() {
            return false;
          },
        },
        notify: () => {},
        log: () => {},
        store,
      });
      const sandbox = new PluginSandbox({
        manifest,
        transport: loopbackTransport({ module: quickFile as never }),
        workerSource: "",
        api,
        timeoutMs: 1_000,
      });
      await sandbox.invoke("actions", "file", { threadIds: [1] });
    }

    // First time alphabetical; second time the one just used is on top.
    expect(seen[0]).toEqual(["Admin", "Receipts"]);
    expect(seen[1]).toEqual(["Receipts", "Admin"]);
  });

  /** The manifest opts out, so this action is deliberately not an agent tool. */
  it("is not exposed to the agent", () => {
    expect(manifest.capabilities.agent).toBe(false);
  });
});

/* -------------------------------------------------------------------------- */
/* Worked example 2 — Snooze Until Free                                        */
/* -------------------------------------------------------------------------- */

describe("worked example 2 — Snooze Until Free", () => {
  const manifest = asManifest(snoozeManifest);

  /** Monday 2026-08-10, 08:00 local. */
  const monday = new Date(2026, 7, 10, 8, 0, 0).getTime();
  const at = (day: number, hour: number, minute = 0) =>
    new Date(2026, 7, day, hour, minute, 0).getTime();

  function calendar(events: { start: number; end: number; allDay?: boolean; rsvp?: string }[]) {
    return {
      async listEvents() {
        return events.map((event, index) => ({
          id: index + 1,
          calendarId: "primary",
          accountId: 1,
          title: `event ${index}`,
          start: event.start,
          end: event.end,
          allDay: event.allDay ?? false,
          attendees: [],
          rsvp: event.rsvp as never,
        }));
      },
    };
  }

  it("snoozes to the first working-hours gap big enough to use", async () => {
    const h = harness({
      manifest,
      module: snoozeUntilFree,
      now: () => monday,
      // Booked solid until 14:00, so the first 45-minute gap starts there.
      source: calendar([{ start: at(10, 9), end: at(10, 14) }]) as never,
    });

    await h.sandbox.invoke("actions", "snooze", { threadIds: [42], params: {} });

    expect(h.runs).toHaveLength(1);
    const command = h.runs[0] as { kind: string; threadIds: number[]; until: number };
    expect(command.kind).toBe("snooze");
    expect(command.threadIds).toEqual([42]);
    expect(command.until).toBe(at(10, 14));
    expect(h.notices[0]).toMatch(/Back at today 2:00/);
  });

  /**
   * The module note, asserted: "half of them are 'Anna OOO' and the rest are
   * birthdays; treating them as blocking would push every snooze into next
   * week."
   */
  it("does not let an all-day event make the day busy", async () => {
    const h = harness({
      manifest,
      module: snoozeUntilFree,
      now: () => monday,
      source: calendar([{ start: at(10, 0), end: at(11, 0), allDay: true }]) as never,
    });

    await h.sandbox.invoke("actions", "snooze", { threadIds: [1], params: {} });
    // 08:00 + a 60-minute lead = 09:00, which is when the working day opens.
    expect((h.runs[0] as { until: number }).until).toBe(at(10, 9));
  });

  it("ignores an event the user declined", async () => {
    const h = harness({
      manifest,
      module: snoozeUntilFree,
      now: () => monday,
      source: calendar([{ start: at(10, 9), end: at(10, 17), rsvp: "declined" }]) as never,
    });
    await h.sandbox.invoke("actions", "snooze", { threadIds: [1], params: {} });
    expect((h.runs[0] as { until: number }).until).toBe(at(10, 9));
  });

  it("honours the minimumMinutes parameter the agent may pass", async () => {
    // A 30-minute hole at 11:00, then nothing until 16:00.
    const busy = [
      { start: at(10, 9), end: at(10, 11) },
      { start: at(10, 11, 30), end: at(10, 16) },
    ];

    const short = harness({
      manifest,
      module: snoozeUntilFree,
      now: () => monday,
      source: calendar(busy) as never,
    });
    await short.sandbox.invoke("actions", "snooze", {
      threadIds: [1],
      params: { minimumMinutes: 20 },
    });
    expect((short.runs[0] as { until: number }).until).toBe(at(10, 11));

    const long = harness({
      manifest,
      module: snoozeUntilFree,
      now: () => monday,
      source: calendar(busy) as never,
    });
    await long.sandbox.invoke("actions", "snooze", { threadIds: [1], params: {} });
    expect((long.runs[0] as { until: number }).until).toBe(at(10, 16));
  });

  it("says so, and snoozes nothing, when there is no gap at all", async () => {
    const h = harness({
      manifest,
      module: snoozeUntilFree,
      now: () => monday,
      // Three weeks of solid calendar — the whole horizon.
      source: calendar([{ start: at(10, 0), end: at(40, 0) }]) as never,
    });
    await h.sandbox.invoke("actions", "snooze", { threadIds: [1], params: {} });
    expect(h.runs).toEqual([]);
    expect(h.notices[0]).toMatch(/No free 45 minutes in the next 21 days/);
  });

  /** The reading-pane view: the plugin's value made visible before ⇧H is pressed. */
  it("renders a reading-pane view naming the time it would land on", async () => {
    const h = harness({
      manifest,
      module: snoozeUntilFree,
      now: () => monday,
      source: calendar([{ start: at(10, 9), end: at(10, 14) }]) as never,
    });

    const node = (await h.sandbox.invoke("views", "next-free", { threadId: 42 })) as {
      type: string;
      children: { type: string; label?: string; value?: string; action?: string }[];
    };

    expect(node.type).toBe("section");
    expect(node.children[0]).toMatchObject({ type: "row", label: "Next free" });
    expect(node.children[0].value).toMatch(/today 2:00/);
    // The only way a view can cause anything: it names one of the plugin's own
    // actions.
    expect(node.children[1]).toMatchObject({ type: "button", action: "snooze" });
  });

  it("renders nothing when no conversation is open", async () => {
    const h = harness({ manifest, module: snoozeUntilFree, now: () => monday });
    await expect(h.sandbox.invoke("views", "next-free", { threadId: null })).resolves.toBeNull();
  });

  it("stores working hours from the configure action", async () => {
    const h = harness({
      manifest,
      module: snoozeUntilFree,
      now: () => monday,
      ask: { text: (async () => "10-16") as never },
    });
    await h.sandbox.invoke("actions", "configure", {});
    expect(h.notices[0]).toBe("Working hours set to 10:00–16:00");
  });

  it("refuses working hours that are not working hours", async () => {
    const h = harness({
      manifest,
      module: snoozeUntilFree,
      now: () => monday,
      ask: { text: (async () => "18-9") as never },
    });
    await h.sandbox.invoke("actions", "configure", {});
    expect(h.notices[0]).toBe("Working hours look like 9-18");
  });

  /** Its whole grant is the calendar; it must never be able to read mail. */
  it("cannot read a message body with the capabilities it declared", async () => {
    const { capabilityDenial } = await import("./capability");
    expect(capabilityDenial(manifest, "read.thread", [1])).toBe(
      'snooze-until-free did not declare read: ["threads"]',
    );
    expect(capabilityDenial(manifest, "read.events", [{}])).toBeNull();
  });
});
