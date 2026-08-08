/**
 * The canary — run before any plugin is allowed to load.
 *
 * `docs/plugins.md` §2 rests on one empirical claim: that a plugin in an iframe
 * on its own origin, under `connect-src 'none'`, actually loses the network, the
 * app's DOM, the app's storage and the Tauri IPC surface — **in WKWebView**, not
 * only in Chrome. HTML §7.1.7 requires a `blob:` worker to inherit the creating
 * document's policy container, so the design is asking the engine for something
 * the spec mandates rather than for a favour. Engines have been known to
 * disagree with the spec, and this is precisely the kind of gap that fails
 * silently: a plugin exfiltrates and nothing looks broken.
 *
 * So this is not a one-off measurement. It runs **at plugin-host boot, every
 * boot**, and if any attempt succeeds the host refuses to load plugins at all
 * and says which check failed. Loading untrusted code behind an unverified
 * boundary is worse than not loading it.
 *
 * # Two mistakes this file is written to avoid
 *
 * **Testing a constructor is not testing a connection.** `new EventSource(…)`
 * does not throw when CSP will refuse the connection; it constructs happily and
 * fails asynchronously. Every network probe waits for the outcome.
 *
 * **Asking "does `localStorage` exist here?" proves nothing.** On a distinct
 * origin it exists and is empty, which is the point of the distinct origin. The
 * question worth asking is whether the *app's* storage is reachable, so the host
 * plants a sentinel and the guest goes looking for it.
 *
 * And the third, which the first run of this probe would have fallen for:
 * **every check here is a negative.** An unplugged network blocks all of them
 * and would be reported as a verified sandbox. So the host fetches the same URL
 * the guest was refused, and a failing control makes the run inconclusive rather
 * than a pass.
 */

import type {
  ConformanceControl,
  ConformanceReport,
  ConformanceRow,
  PluginManifest,
} from "./types";
import { PluginSandbox, iframeTransport } from "./sandbox";

/** The id the probe runs under, and therefore the origin it runs on. */
export const CONFORMANCE_ID = "conformance";

const SENTINEL_KEY = "mach.plugin-conformance.sentinel";

/** Generous: the probe waits out several 1.5s network timeouts in sequence. */
const PROBE_TIMEOUT_MS = 20_000;

/**
 * A manifest that grants nothing.
 *
 * The canary needs no capability — every call it makes is supposed to fail —
 * and running it under an empty grant means a bug in the probe cannot become a
 * capability the probe quietly had.
 */
const CANARY_MANIFEST: PluginManifest = {
  id: CONFORMANCE_ID,
  name: "Sandbox conformance",
  version: "1.0.0",
  machApi: "1",
  description: "Tries every escape a hostile plugin would, and reports.",
  author: "mach",
  main: "canary.js",
  machApiProposed: [],
  runtime: "sandbox",
  capabilities: { read: [], commands: [], ui: [], events: [], store: false, agent: false },
  contributes: { actions: [], views: [] },
};

export interface ConformanceOptions {
  workerSource: string;
  canarySource: string;
  timeoutMs?: number;
  now?: () => number;
}

/**
 * Run every escape attempt. Resolves with a report; never throws.
 *
 * A thrown error would be indistinguishable from "the sandbox is fine but
 * something else broke", and the caller has to be able to tell those apart —
 * one refuses to load plugins, the other refuses to load plugins *and* is a bug
 * in Mach.
 */
export async function runConformance(options: ConformanceOptions): Promise<ConformanceReport> {
  const now = options.now ?? Date.now;
  const appOrigin = window.location.origin;
  const sentinel = `sentinel-${Math.random().toString(36).slice(2)}`;

  plantSentinel(sentinel);

  const sandbox = new PluginSandbox({
    manifest: CANARY_MANIFEST,
    transport: iframeTransport(CONFORMANCE_ID),
    workerSource: options.workerSource,
    api: {},
    timeoutMs: options.timeoutMs ?? PROBE_TIMEOUT_MS,
  });

  try {
    const guestOrigin = await sandbox.start();
    await sandbox.load(options.canarySource);

    const guestRows = (await sandbox.probeGuest({
      appOrigin,
      sentinelKey: SENTINEL_KEY,
      sentinel,
    })) as ConformanceRow[];
    const workerRows = (await sandbox.invoke("actions", "probe", {
      appOrigin,
    })) as ConformanceRow[];

    const rows = [...guestRows, ...workerRows];
    const failures = rows.filter((row) => row.allowed).map((row) => `${row.scope}: ${row.name}`);
    const control = await runControl(appOrigin);
    if (!control.succeeded) {
      failures.push(`the control failed (${control.detail}), so nothing was proved`);
    }

    return {
      ok: failures.length === 0 && rows.length > 0,
      at: now(),
      appOrigin,
      guestOrigin,
      rows,
      control,
      failures,
    };
  } catch (error) {
    return {
      ok: false,
      at: now(),
      appOrigin,
      guestOrigin: "",
      rows: [],
      failures: ["the sandbox could not be stood up"],
      error: error instanceof Error ? error.message : String(error),
    };
  } finally {
    clearSentinel();
    sandbox.destroy();
  }
}

/**
 * The same request the guest was refused, from a page that should be allowed.
 *
 * `${appOrigin}/index.html` rather than a remote host on purpose: the app's own
 * CSP is `connect-src 'self'`, so a remote control would fail in a production
 * build for a reason that has nothing to do with the sandbox. Same URL, two
 * origins, opposite outcomes — that is the whole argument.
 */
async function runControl(appOrigin: string): Promise<ConformanceControl> {
  const name = "host page can fetch what the guest could not";
  try {
    const response = await fetch(`${appOrigin}/index.html`, { cache: "no-store" });
    return {
      name,
      succeeded: response.ok,
      detail: `HTTP ${response.status}`,
    };
  } catch (error) {
    return {
      name,
      succeeded: false,
      detail: error instanceof Error ? error.message : String(error),
    };
  }
}

/**
 * Something for the guest to fail to find.
 *
 * Both a `localStorage` key and a cookie, because they are partitioned by
 * different machinery and a WebView could plausibly get one right and the other
 * wrong.
 */
function plantSentinel(sentinel: string): void {
  try {
    window.localStorage.setItem(SENTINEL_KEY, sentinel);
  } catch {
    /* private mode, or storage disabled: the row will report it */
  }
  try {
    document.cookie = `${SENTINEL_KEY}=${sentinel}; path=/; SameSite=Lax`;
  } catch {
    /* custom protocols do not always carry cookies; the row reports that too */
  }
}

function clearSentinel(): void {
  try {
    window.localStorage.removeItem(SENTINEL_KEY);
  } catch {
    /* nothing to clean up */
  }
  try {
    document.cookie = `${SENTINEL_KEY}=; path=/; Max-Age=0`;
  } catch {
    /* nothing to clean up */
  }
}

/** The one-line verdict, for the log and for the plugin list. */
export function describeConformance(report: ConformanceReport): string {
  if (report.error) {
    return `Plugin sandbox conformance could not run: ${report.error}`;
  }
  if (report.ok) {
    return `Plugin sandbox verified: ${report.rows.length} of ${report.rows.length} escape ` +
      `attempts blocked (guest origin ${report.guestOrigin}).`;
  }
  return (
    `Plugin sandbox FAILED: ${report.failures.join(", ")} succeeded. ` +
    `Plugins will not be loaded.`
  );
}
