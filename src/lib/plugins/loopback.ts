/**
 * The sandbox's guest and worker, in process, for tests.
 *
 * There is no iframe and no `plugin://` origin in a unit test, so this speaks
 * the same `postMessage` protocol `sandbox.js` and `worker.js` speak and calls
 * into an already-imported module. That makes the *host's* behaviour testable
 * without a WebView: the capability refusals, the per-call timeout, the
 * termination path, the undo grouping, and both worked examples running against
 * a real `mach`.
 *
 * **What this deliberately does not test**, and the reason the conformance probe
 * exists as well: the isolation. A loopback transport has no origin, no CSP and
 * no worker, so it can prove that the host refuses an undeclared command and can
 * prove nothing whatsoever about whether a real plugin could reach the network.
 * Those are different claims and they need different evidence — one runs in CI
 * on any machine, the other only in WKWebView.
 */

import type { GuestMessage, HostMessage } from "./types";
import type { SandboxTransport } from "./sandbox";

/** The shape a plugin module exports. All four are optional. */
export interface PluginModule {
  actions?: Record<string, (ctx: Record<string, unknown>) => unknown>;
  views?: Record<string, (ctx: Record<string, unknown>) => unknown>;
  events?: Record<string, (ctx: Record<string, unknown>) => unknown>;
  decorations?: Record<string, (ctx: Record<string, unknown>) => unknown>;
}

export interface LoopbackOptions {
  module: PluginModule;
  /** Never answer, so the host's timeout can be exercised. */
  deaf?: boolean;
  now?: () => number;
}

/** The flat `mach.*` method names, exactly as the real worker shim posts them. */
const METHODS = [
  "run",
  "read.threads",
  "read.thread",
  "read.events",
  "read.labels",
  "read.accounts",
  "ask.pick",
  "ask.text",
  "ask.confirm",
  "notify",
  "store.get",
  "store.set",
  "log",
];

export function loopbackTransport(options: LoopbackOptions): SandboxTransport {
  let toHost: ((message: GuestMessage) => void) | null = null;
  let terminated = false;
  let nextCall = 1;
  const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();

  const post = (message: GuestMessage) => queueMicrotask(() => toHost?.(message));

  function call(method: string, args: unknown[]): Promise<unknown> {
    const id = nextCall++;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      post({ t: "call", id, method, args });
    });
  }

  const mach: Record<string, unknown> = { now: options.now ?? Date.now };
  for (const method of METHODS) {
    const [head, tail] = method.split(".");
    if (tail === undefined) {
      mach[head] = (...args: unknown[]) => call(method, args);
    } else {
      const group = (mach[head] ??= {}) as Record<string, unknown>;
      group[tail] = (...args: unknown[]) => call(method, args);
    }
  }

  return {
    post(message: HostMessage) {
      if (options.deaf && message.t !== "boot") return;
      switch (message.t) {
        case "boot":
          return post({ t: "booted", origin: "loopback://plugin" });

        case "load":
          return post({
            t: "result",
            id: message.id,
            ok: true,
            value: {
              actions: Object.keys(options.module.actions ?? {}),
              views: Object.keys(options.module.views ?? {}),
              events: Object.keys(options.module.events ?? {}),
              decorations: Object.keys(options.module.decorations ?? {}),
            },
          });

        case "invoke": {
          if (terminated) return;
          const table = (options.module as Record<string, unknown>)[message.kind] as
            | Record<string, (ctx: Record<string, unknown>) => unknown>
            | undefined;
          const handler = table?.[message.name];
          if (!handler) {
            return post({
              t: "result",
              id: message.id,
              ok: false,
              error: `no ${message.kind} named "${message.name}"`,
            });
          }
          void (async () => {
            try {
              const value = await handler({ ...message.ctx, mach });
              if (!terminated) {
                post({ t: "result", id: message.id, ok: true, value: value ?? null });
              }
            } catch (error) {
              if (!terminated) {
                post({
                  t: "result",
                  id: message.id,
                  ok: false,
                  error: error instanceof Error ? error.message : String(error),
                });
              }
            }
          })();
          return;
        }

        case "reply": {
          const waiting = pending.get(message.id);
          pending.delete(message.id);
          if (!waiting) return;
          return message.ok
            ? waiting.resolve(message.value)
            : waiting.reject(new Error(message.error ?? "refused"));
        }

        case "terminate":
          terminated = true;
          for (const waiting of pending.values()) {
            waiting.reject(new Error("the plugin was terminated"));
          }
          pending.clear();
          return post({ t: "terminated", id: message.id });

        case "guest-probe":
          return post({ t: "result", id: message.id, ok: true, value: [] });
      }
    },

    receive(handler) {
      toHost = handler;
      // The real guest announces itself as soon as its script runs.
      post({ t: "ready", origin: "loopback://plugin" });
    },

    destroy() {
      toHost = null;
      pending.clear();
    },
  };
}
