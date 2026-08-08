/**
 * The host half of the plugin sandbox.
 *
 * Everything a plugin can do arrives here as a message and is checked against
 * its manifest *on this side of the boundary*. The guest is never trusted to
 * enforce anything about itself — it is only trusted to be unable to do
 * anything else, which is what `docs/plugin-poc/README.md` exists to verify.
 *
 * Three jobs:
 *   1. build the guest document (iframe srcdoc + CSP) and the worker inside it;
 *   2. answer `mach.*` calls, refusing anything outside the manifest;
 *   3. keep a per-call timeout so a spinning plugin is a disabled plugin
 *      rather than a frozen window.
 */

/** How long any single plugin call may take before it is abandoned. */
const CALL_TIMEOUT_MS = 5_000;

/**
 * The guest's Content-Security-Policy — for the opaque `srcdoc` fallback only.
 *
 * In the preferred mode the guest is a real document on its own origin and
 * carries this policy itself (`guest.html`); in the real app it should arrive
 * as a response header from the custom protocol handler, which is one fewer
 * thing a plugin could ever influence.
 *
 * `connect-src 'none'` is the load-bearing directive: it is what removes fetch,
 * XHR, WebSocket, EventSource and sendBeacon. `script-src` allows exactly the
 * two things the loader needs — the inlined guest script and blob: URLs for the
 * worker and the plugin module — and nothing that could reach the network.
 *
 * Note there is no `https:` anywhere. That is the point.
 */
const GUEST_CSP = [
  "default-src 'none'",
  "script-src 'unsafe-inline' blob:",
  "worker-src blob:",
  "connect-src 'none'",
  "img-src 'none'",
  "style-src 'none'",
  "frame-src 'none'",
  "object-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
].join("; ");

/**
 * Sandbox flags, and the one place this design departs from the message-body
 * rule in `docs/message-rendering-invariants.md`.
 *
 * That rule — never combine `allow-scripts` with `allow-same-origin` — is about
 * content served from **the app's own origin**, where the pair lets the frame
 * reach app storage and remove its own sandbox attribute. The plugin guest is
 * served from a *different* origin (a Tauri custom protocol in the real app;
 * `127.0.0.1` standing in for it here), so `allow-same-origin` means "keep your
 * own, foreign origin" rather than "become us". Nothing about the app is
 * reachable either way.
 *
 * It has to be granted, because an **opaque** origin cannot run a worker at
 * all: `blob:null/…` is not a fetchable script URL, so `new Worker` constructs
 * and then immediately errors. See the negative result in the PoC README.
 *
 * Everything else stays off: no popups, no top-level navigation, no forms, no
 * pointer lock, no downloads.
 */
const GUEST_SANDBOX = "allow-scripts allow-same-origin";

/** The strictly-worse fallback: an opaque origin, which cannot host a worker. */
const OPAQUE_SANDBOX = "allow-scripts";

export class PluginHost {
  #frame = null;
  #ready = null;
  #next = 1;
  #pending = new Map();
  #manifest = null;
  #api = null;

  /**
   * @param {object} o
   * @param {string} [o.guestUrl]     the guest document, on its own origin. Preferred.
   * @param {string} [o.sandboxSource] the guest script as text, for the opaque fallback
   * @param {string} o.workerSource   the worker shim, as text
   */
  constructor({ guestUrl, sandboxSource, workerSource }) {
    this.guestUrl = guestUrl;
    this.sandboxSource = sandboxSource;
    this.workerSource = workerSource;
  }

  /** Stand the guest up. Idempotent. */
  async start() {
    if (this.#ready) return this.#ready;

    const frame = document.createElement("iframe");
    frame.setAttribute("aria-hidden", "true");
    frame.style.cssText = "position:absolute;width:0;height:0;border:0;visibility:hidden";

    if (this.guestUrl) {
      frame.setAttribute("sandbox", GUEST_SANDBOX);
      frame.src = this.guestUrl;
    } else {
      // Kept only so the negative result is reproducible rather than folklore.
      frame.setAttribute("sandbox", OPAQUE_SANDBOX);
      frame.srcdoc = guestDocument(this.sandboxSource, GUEST_CSP);
    }

    this.#frame = frame;
    this.#ready = new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("guest never became ready")), CALL_TIMEOUT_MS);
      const onMessage = (event) => {
        // The guest has an opaque origin, so `event.origin` is the string
        // "null" and proves nothing. Identity comes from the window reference.
        if (event.source !== frame.contentWindow) return;
        if (event.data?.t !== "ready") return this.#receive(event.data);
        clearTimeout(timer);
        frame.contentWindow.postMessage({ t: "boot", workerSource: this.workerSource }, "*");
        resolve();
      };
      window.addEventListener("message", onMessage);
      this.onMessage = onMessage;
    });

    document.body.appendChild(frame);
    return this.#ready;
  }

  /**
   * Load one plugin.
   *
   * @param {object} manifest  the parsed mach-plugin.json
   * @param {string} source    main.js, as text
   * @param {object} api       host implementations, keyed by `mach.*` method name
   */
  async load(manifest, source, api) {
    await this.start();
    this.#manifest = manifest;
    this.#api = api;
    return this.#send({ t: "load", source });
  }

  /** Invoke an exported action, view or event handler. */
  async invoke(kind, name, ctx) {
    return this.#send({ t: "invoke", kind, name, ctx });
  }

  /**
   * Run the escape attempts that only make sense from the guest *document*.
   *
   * Separate from the canary plugin because a worker has no `document` and no
   * `localStorage` to be denied, so proving those from worker scope would prove
   * nothing about the frame the worker lives in.
   */
  async probeGuest(ctx) {
    await this.start();
    return this.#send({ t: "guest-probe", ctx });
  }

  destroy() {
    window.removeEventListener("message", this.onMessage);
    this.#frame?.remove();
    this.#frame = null;
    this.#ready = null;
    for (const { reject } of this.#pending.values()) reject(new Error("host destroyed"));
    this.#pending.clear();
  }

  /* ---------------------------------------------------------------------- */

  #send(message) {
    if (this.fatal) return Promise.reject(new Error(this.fatal));
    const id = this.#next++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`plugin call timed out after ${CALL_TIMEOUT_MS}ms`));
      }, CALL_TIMEOUT_MS);
      this.#pending.set(id, {
        resolve: (v) => (clearTimeout(timer), resolve(v)),
        reject: (e) => (clearTimeout(timer), reject(e)),
      });
      this.#frame.contentWindow.postMessage({ ...message, id }, "*");
    });
  }

  async #receive(message) {
    /*
     * The guest cannot continue. Failing every in-flight call now is the whole
     * point: without it the caller waits out the timeout and gets "timed out"
     * instead of "the worker could not start", which is a much worse bug report.
     */
    if (message?.t === "fatal") {
      const error = new Error(message.error);
      for (const { reject } of this.#pending.values()) reject(error);
      this.#pending.clear();
      this.fatal = message.error;
      return;
    }

    if (message?.t === "result") {
      const waiting = this.#pending.get(message.id);
      this.#pending.delete(message.id);
      if (!waiting) return;
      return message.ok ? waiting.resolve(message.value) : waiting.reject(new Error(message.error));
    }

    if (message?.t === "call") {
      let reply;
      try {
        reply = { t: "reply", id: message.id, ok: true, value: await this.#dispatch(message) };
      } catch (error) {
        reply = { t: "reply", id: message.id, ok: false, error: String(error?.message ?? error) };
      }
      this.#frame?.contentWindow?.postMessage(reply, "*");
    }
  }

  /**
   * One `mach.*` call, checked and then performed.
   *
   * The capability check happens before the implementation is even looked up,
   * so a method the manifest did not license is indistinguishable from a method
   * that does not exist — which is the behaviour a plugin author should get, and
   * which leaks nothing about what else the host can do.
   */
  async #dispatch({ method, args }) {
    const denial = capabilityDenial(this.#manifest, method, args);
    if (denial) throw new Error(denial);

    const implementation = this.#api[method];
    if (!implementation) throw new Error(`no such method: ${method}`);
    return implementation(...args);
  }
}

/**
 * Why this call is not allowed, or `null` if it is.
 *
 * Returned as a sentence rather than a boolean because the message goes
 * straight to the plugin author, and "quick-file did not declare
 * commands: [\"trash\"]" is a bug report that fixes itself.
 */
export function capabilityDenial(manifest, method, args) {
  const caps = manifest?.capabilities ?? {};
  const id = manifest?.id ?? "plugin";

  if (method === "run") {
    const kind = args?.[0]?.kind;
    const granted = caps.commands ?? [];
    if (!granted.includes(kind)) {
      return `${id} did not declare commands: ["${kind}"]`;
    }
    return null;
  }

  if (method.startsWith("read.")) {
    const need = READ_CAPABILITY[method];
    const granted = caps.read ?? [];
    // "threads" implies "threads.metadata"; nothing else implies anything.
    const ok = granted.includes(need) || (need === "threads.metadata" && granted.includes("threads"));
    if (!ok) return `${id} did not declare read: ["${need}"]`;
    return null;
  }

  if (method.startsWith("store.") && !caps.store) {
    return `${id} did not declare store`;
  }

  if (method.startsWith("ask.") && !(caps.ui ?? []).length) {
    return `${id} declared no ui capability, so it cannot prompt`;
  }

  return null;
}

const READ_CAPABILITY = {
  "read.threads": "threads.metadata",
  "read.thread": "threads",
  "read.events": "calendar",
  "read.labels": "labels",
  "read.accounts": "accounts",
};

/**
 * The guest document.
 *
 * `srcdoc` rather than a served file so the host owns every byte of it,
 * including the policy — there is no server configuration to get wrong and no
 * second file that could drift out of step with this one.
 */
function guestDocument(sandboxSource, csp) {
  return `<!doctype html><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<title>mach plugin sandbox</title>
<script>${sandboxSource}<\/script>`;
}
