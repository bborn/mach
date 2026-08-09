/**
 * The host half of the plugin sandbox.
 *
 * Everything a plugin can do arrives here as a message and is checked against
 * its manifest *on this side of the boundary*. The guest is never trusted to
 * enforce anything about itself — it is only trusted to be unable to do
 * anything else, which is what `conformance.ts` exists to verify.
 *
 * Four jobs:
 *   1. stand the guest up on its own origin and boot the worker inside it;
 *   2. answer `mach.*` calls, refusing anything outside the manifest;
 *   3. keep a per-call timeout, so a spinning plugin is a disabled plugin
 *      rather than a frozen window;
 *   4. terminate a worker that will not yield, and stay usable afterwards.
 *
 * # Why the transport is injectable
 *
 * The security properties worth testing — that an undeclared command is
 * refused, that a hang is survived, that a throw does not escape — are
 * properties of *this* file, not of the WebView. A fake transport lets every
 * one of them be a unit test that runs in CI on any machine. The WebView's own
 * half is verified separately, and only in the WebView, by the conformance
 * probe. Neither test can substitute for the other.
 */

import type { GuestMessage, HostMessage, PluginManifest } from "./types";
import { capabilityDenial } from "./capability";

/** How long any single plugin call may take before it is abandoned. */
export const CALL_TIMEOUT_MS = 5_000;

/** How many timeouts a plugin gets before it is switched off. */
export const TIMEOUT_STRIKES = 3;

/** Everything the host sends that expects an answer. `boot` is the exception. */
type Request = Exclude<HostMessage, { t: "boot" }>;

/** `Omit` over a union has to distribute, or every member loses its own keys. */
type WithoutId<T> = T extends unknown ? Omit<T, "id"> : never;

export interface SandboxTransport {
  post(message: HostMessage): void;
  receive(handler: (message: GuestMessage) => void): void;
  destroy(): void;
}

/** The host implementations, keyed by the flat `mach.*` method name. */
export type HostApi = Record<string, (...args: never[]) => unknown>;

export interface SandboxOptions {
  manifest: PluginManifest;
  transport: SandboxTransport;
  workerSource: string;
  api: HostApi;
  timeoutMs?: number;
  /** Called when the plugin is switched off for misbehaving. */
  onDisabled?: (reason: string) => void;
  /** Called for anything worth putting in the log, prefixed with the id. */
  onLog?: (...args: unknown[]) => void;
  /** Milliseconds to add to the plugin's clock. Tests freeze time with this. */
  clockOffset?: number;
}

export interface PluginExports {
  actions: string[];
  views: string[];
  events: string[];
  decorations: string[];
}

export class PluginSandbox {
  readonly id: string;
  private readonly options: SandboxOptions;
  private readonly pending = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }
  >();
  private next = 1;
  private booted: Promise<string> | null = null;
  private resolveBooted: ((origin: string) => void) | null = null;
  private rejectBooted: ((error: Error) => void) | null = null;
  private fatal: string | null = null;
  private timeouts = 0;
  private disabled = false;

  constructor(options: SandboxOptions) {
    this.options = options;
    this.id = options.manifest.id;
    options.transport.receive((message) => this.receive(message));
  }

  /** Stand the guest up. Idempotent; the second caller waits on the first. */
  start(): Promise<string> {
    if (this.booted) return this.booted;
    this.booted = new Promise<string>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`${this.id}: the sandbox never became ready`)),
        this.timeout(),
      );
      // Settling either way clears the watchdog.
      this.resolveBooted = (origin) => {
        clearTimeout(timer);
        resolve(origin);
      };
      this.rejectBooted = (error) => {
        clearTimeout(timer);
        reject(error);
      };
    });
    return this.booted;
  }

  async load(source: string): Promise<PluginExports> {
    await this.start();
    const value = (await this.send({
      t: "load",
      source,
      clockOffset: this.options.clockOffset ?? 0,
    })) as PluginExports;
    return value;
  }

  /** Invoke an exported action, view or event handler. */
  invoke(kind: string, name: string, ctx: Record<string, unknown>): Promise<unknown> {
    return this.send({ t: "invoke", kind, name, ctx });
  }

  /** The document-scope escapes. Only the conformance probe calls this. */
  probeGuest(ctx: Record<string, unknown>): Promise<unknown> {
    return this.send({ t: "guest-probe", ctx });
  }

  /**
   * Stop a worker that will not yield.
   *
   * A per-call timeout abandons the *call*; only `terminate()` stops the loop.
   * The guest survives, so the next activation costs a worker rather than a
   * whole document.
   */
  terminate(): void {
    this.options.transport.post({ t: "terminate", id: this.next++ });
    this.failAll(new Error(`${this.id} was terminated`));
  }

  destroy(): void {
    this.failAll(new Error(`${this.id}: the sandbox was destroyed`));
    this.options.transport.destroy();
    this.booted = null;
  }

  isDisabled(): boolean {
    return this.disabled;
  }

  private timeout(): number {
    return this.options.timeoutMs ?? CALL_TIMEOUT_MS;
  }

  private send(message: WithoutId<Request>): Promise<unknown> {
    if (this.disabled) {
      return Promise.reject(new Error(`${this.id} is disabled`));
    }
    if (this.fatal) return Promise.reject(new Error(this.fatal));

    const id = this.next++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        this.strike();
        reject(new Error(`${this.id} did not answer within ${this.timeout()}ms`));
      }, this.timeout());
      this.pending.set(id, { resolve, reject, timer });
      this.options.transport.post({ ...message, id } as HostMessage);
    });
  }

  /**
   * A plugin that hangs three times is a plugin that is off.
   *
   * Terminating on the first timeout would punish a slow network the plugin
   * does not even have; never terminating means one bad plugin owns a core for
   * the life of the window.
   */
  private strike(): void {
    this.timeouts += 1;
    this.terminate();
    if (this.timeouts >= TIMEOUT_STRIKES && !this.disabled) {
      this.disabled = true;
      this.options.onDisabled?.(
        `${this.id} stopped responding ${this.timeouts} times — switched off`,
      );
    }
  }

  private failAll(error: Error): void {
    for (const waiting of this.pending.values()) {
      clearTimeout(waiting.timer);
      waiting.reject(error);
    }
    this.pending.clear();
  }

  private receive(message: GuestMessage): void {
    switch (message?.t) {
      case "ready":
        this.options.transport.post({ t: "boot", workerSource: this.options.workerSource });
        return;

      case "booted":
        this.resolveBooted?.(message.origin);
        return;

      /*
       * The guest cannot continue. Failing every in-flight call now is the
       * whole point: without it the caller waits out the timeout and is told
       * "timed out" instead of "the worker could not start", which is a much
       * worse bug report.
       */
      case "fatal": {
        this.fatal = message.error;
        this.rejectBooted?.(new Error(message.error));
        this.failAll(new Error(message.error));
        return;
      }

      case "terminated":
        return;

      case "result": {
        const waiting = this.pending.get(message.id);
        if (!waiting) return;
        this.pending.delete(message.id);
        clearTimeout(waiting.timer);
        if (message.ok) waiting.resolve(message.value);
        else waiting.reject(new Error(message.error ?? "the plugin threw"));
        return;
      }

      case "call":
        void this.answer(message);
        return;
    }
  }

  private async answer(message: Extract<GuestMessage, { t: "call" }>): Promise<void> {
    let reply: HostMessage;
    try {
      reply = {
        t: "reply",
        id: message.id,
        ok: true,
        value: await this.dispatch(message.method, message.args ?? []),
      };
    } catch (error) {
      reply = {
        t: "reply",
        id: message.id,
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      };
    }
    this.options.transport.post(reply);
  }

  /** One `mach.*` call: checked, then performed. Never the other way round. */
  private async dispatch(method: string, args: unknown[]): Promise<unknown> {
    const denial = capabilityDenial(this.options.manifest, method, args);
    if (denial) throw new Error(denial);

    if (method === "log") {
      this.options.onLog?.(...args);
      return null;
    }

    const implementation = this.options.api[method];
    if (!implementation) throw new Error(`no such method: ${method}`);
    return await (implementation as (...a: unknown[]) => unknown)(...args);
  }
}

/* -------------------------------------------------------------------------- */
/* The real transport                                                          */
/* -------------------------------------------------------------------------- */

/**
 * Sandbox flags on the guest frame.
 *
 * `allow-same-origin` is correct here and is *not* the combination
 * `docs/message-rendering-invariants.md` forbids. That rule is about content on
 * the app's own origin, where the pair lets a frame reach app storage and strip
 * its own sandbox attribute. The guest's origin is already foreign, so the flag
 * means "keep being foreign" — and it has to be granted, because an opaque
 * origin cannot run a worker at all (`blob:null/…` is not fetchable).
 *
 * Everything else stays off: no popups, no top-level navigation, no forms, no
 * pointer lock, no downloads.
 */
export const GUEST_SANDBOX = "allow-scripts allow-same-origin";

/** `plugin://<id>/guest.html` — one origin, and one storage partition, each. */
export function guestUrl(id: string): string {
  return `plugin://${id}/guest.html`;
}

/** A hidden iframe on the plugin's own origin. */
export function iframeTransport(id: string): SandboxTransport {
  const frame = document.createElement("iframe");
  frame.setAttribute("aria-hidden", "true");
  frame.setAttribute("sandbox", GUEST_SANDBOX);
  frame.dataset.pluginId = id;
  frame.style.cssText = "position:absolute;width:0;height:0;border:0;visibility:hidden";
  frame.src = guestUrl(id);

  let handler: ((message: GuestMessage) => void) | null = null;
  const onMessage = (event: MessageEvent) => {
    // The guest may or may not report a useful `event.origin` depending on the
    // platform's custom-protocol spelling, so identity comes from the window
    // reference — which cannot be forged from inside the frame.
    if (event.source !== frame.contentWindow) return;
    handler?.(event.data as GuestMessage);
  };
  window.addEventListener("message", onMessage);
  document.body.appendChild(frame);

  return {
    post(message) {
      frame.contentWindow?.postMessage(message, "*");
    },
    receive(next) {
      handler = next;
    },
    destroy() {
      window.removeEventListener("message", onMessage);
      frame.remove();
      handler = null;
    },
  };
}
