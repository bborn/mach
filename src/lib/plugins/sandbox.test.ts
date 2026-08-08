/**
 * The host's half of the boundary, tested as security properties.
 *
 * Each test here is one sentence from `docs/plugins.md` §3 turned into an
 * assertion. The isolation itself — no network, no app DOM, no app storage, no
 * Tauri IPC — is not testable here and is not tested here: it is a claim about
 * WKWebView, and `conformance.ts` running inside a real window is its evidence.
 * What *is* testable here is everything the host promises on top of that, and
 * every one of those promises is a place a bug would be silent.
 */

import { describe, expect, it, vi } from "vitest";
import { PluginSandbox } from "./sandbox";
import { loopbackTransport, type PluginModule } from "./loopback";
import { capabilityDenial, agentMayCall } from "./capability";
import type { PluginManifest } from "./types";

function manifest(overrides: Partial<PluginManifest["capabilities"]> = {}): PluginManifest {
  return {
    id: "quick-file",
    name: "Quick File",
    version: "1.0.0",
    machApi: "1",
    description: "",
    author: "",
    main: "main.js",
    machApiProposed: [],
    runtime: "sandbox",
    capabilities: {
      read: ["labels"],
      commands: ["label", "archive"],
      ui: ["palette"],
      events: [],
      store: true,
      agent: true,
      ...overrides,
    },
    contributes: { actions: [], views: [] },
  };
}

function sandboxWith(module: PluginModule, api: Record<string, (...a: never[]) => unknown> = {}) {
  return new PluginSandbox({
    manifest: manifest(),
    transport: loopbackTransport({ module }),
    workerSource: "",
    api,
    timeoutMs: 200,
  });
}

describe("capability enforcement", () => {
  it("refuses a command the manifest did not declare, by name", () => {
    const denial = capabilityDenial(manifest(), "run", [{ kind: "trash", threadIds: [1] }]);
    expect(denial).toBe('quick-file did not declare commands: ["trash"]');
  });

  it("allows a command the manifest did declare", () => {
    expect(capabilityDenial(manifest(), "run", [{ kind: "archive", threadIds: [1] }])).toBeNull();
  });

  /**
   * The line the install prompt's honesty depends on: metadata is a smaller ask
   * than bodies, and it has to actually be smaller.
   */
  it("does not let a metadata grant read message bodies", () => {
    const metadataOnly = manifest({ read: ["threads.metadata"] });
    expect(capabilityDenial(metadataOnly, "read.threads", [{}])).toBeNull();
    expect(capabilityDenial(metadataOnly, "read.thread", [1])).toBe(
      'quick-file did not declare read: ["threads"]',
    );
  });

  it("lets a bodies grant imply metadata, and nothing else imply anything", () => {
    const bodies = manifest({ read: ["threads"] });
    expect(capabilityDenial(bodies, "read.threads", [{}])).toBeNull();
    expect(capabilityDenial(bodies, "read.events", [{}])).toBe(
      'quick-file did not declare read: ["calendar"]',
    );
  });

  it("refuses store and ask when they were not asked for", () => {
    expect(capabilityDenial(manifest({ store: false }), "store.set", ["k", 1])).toMatch(/store/);
    expect(capabilityDenial(manifest({ ui: [] }), "ask.pick", [{}])).toMatch(/cannot prompt/);
  });

  /**
   * A method that is not licensed is indistinguishable from a method that does
   * not exist, so a refusal leaks nothing about the rest of the host.
   */
  it("treats an unknown method as an unknown method", () => {
    expect(capabilityDenial(manifest(), "invoke", [])).toBe("no such method: invoke");
    expect(capabilityDenial(manifest(), "fs.readFile", [])).toBe("no such method: fs.readFile");
  });

  it("is opt-out for the agent, per the owner's decision", () => {
    expect(agentMayCall(manifest(), "file")).toBe(true);
    expect(agentMayCall(manifest({ agent: false }), "file")).toBe(false);
    expect(agentMayCall(manifest({ agent: ["other"] }), "file")).toBe(false);
    expect(agentMayCall(manifest({ agent: ["file"] }), "file")).toBe(true);
  });
});

describe("the sandbox host", () => {
  it("checks the capability before it looks the implementation up", async () => {
    const trash = vi.fn();
    // Registered under a name a plugin can reach, and still never called.
    const sandbox = sandboxWith(
      {
        actions: {
          async evil({ mach }) {
            const m = mach as { run: (c: unknown) => Promise<unknown> };
            try {
              await m.run({ kind: "trash", threadIds: [1] });
              return "the host failed to refuse this";
            } catch (error) {
              return (error as Error).message;
            }
          },
        },
      },
      { run: trash },
    );

    await expect(sandbox.invoke("actions", "evil", {})).resolves.toBe(
      'quick-file did not declare commands: ["trash"]',
    );
    expect(trash).not.toHaveBeenCalled();
  });

  it("does not let a plugin that throws take anything else with it", async () => {
    const sandbox = sandboxWith({
      actions: {
        boom() {
          throw new Error("plugin exploded");
        },
        fine() {
          return "still here";
        },
      },
    });

    await expect(sandbox.invoke("actions", "boom", {})).rejects.toThrow("plugin exploded");
    await expect(sandbox.invoke("actions", "fine", {})).resolves.toBe("still here");
  });

  it("abandons a call that never answers rather than waiting forever", async () => {
    const sandbox = new PluginSandbox({
      manifest: manifest(),
      transport: loopbackTransport({ module: {}, deaf: true }),
      workerSource: "",
      api: {},
      timeoutMs: 30,
    });
    await expect(sandbox.invoke("actions", "hang", {})).rejects.toThrow(/did not answer/);
  });

  it("switches a plugin off after three hangs, and says so", async () => {
    const onDisabled = vi.fn();
    const sandbox = new PluginSandbox({
      manifest: manifest(),
      transport: loopbackTransport({ module: {}, deaf: true }),
      workerSource: "",
      api: {},
      timeoutMs: 20,
      onDisabled,
    });

    for (let i = 0; i < 3; i++) {
      await sandbox.invoke("actions", "hang", {}).catch(() => {});
    }
    expect(sandbox.isDisabled()).toBe(true);
    expect(onDisabled).toHaveBeenCalledWith(expect.stringMatching(/stopped responding 3 times/));
    // And it stays off until someone re-enables it.
    await expect(sandbox.invoke("actions", "anything", {})).rejects.toThrow(/is disabled/);
  });

  /**
   * The PoC's first finding, kept as a test: an opaque origin cannot host a
   * worker, `new Worker` constructs and *then* fails, and the failure mode was
   * a silent five-second timeout. "The worker could not start" is a bug report;
   * "timed out" is not.
   */
  it("reports a guest that cannot start, instead of timing out", async () => {
    let toHost: ((message: never) => void) | null = null;
    const sandbox = new PluginSandbox({
      manifest: manifest(),
      transport: {
        post(message) {
          if (message.t !== "boot") return;
          (toHost as unknown as (m: unknown) => void)?.({
            t: "fatal",
            error: "worker failed to start (blob:null/…) — origin is null",
          });
        },
        receive(handler) {
          toHost = handler as never;
          queueMicrotask(() =>
            (toHost as unknown as (m: unknown) => void)?.({ t: "ready", origin: "null" }),
          );
        },
        destroy() {},
      },
      workerSource: "",
      api: {},
      timeoutMs: 500,
    });

    await expect(sandbox.start()).rejects.toThrow(/worker failed to start/);
  });
});
