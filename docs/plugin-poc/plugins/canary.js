/**
 * The canary. Not a plugin anyone would want — a plugin that tries everything a
 * hostile one would, and reports what happened.
 *
 * This is meant to run at plugin-host boot in the real app, not only in CI: the
 * WebView is a moving target, and a policy that held in one OS release is not a
 * policy that holds in the next. If any row comes back `allowed: true`, the host
 * refuses to load plugins at all and says which check failed.
 *
 * Everything here is in worker scope, which is where plugin code lives. The
 * document-scope escapes are in `sandbox.js`, because a worker has no
 * `document` to test against and passing that test by absence would prove
 * nothing about the frame.
 */

export const actions = {
  /** `ctx.appOrigin` is the host page's origin — the one thing worth reaching. */
  async probe({ appOrigin = "http://localhost" }) {
    return [
      await attempt("fetch (remote)", () => fetch("https://example.com/")),
      await attempt("fetch (app origin)", () => fetch(`${appOrigin}/index.html`)),
      await attempt("XMLHttpRequest", () => {
        // Synchronous on purpose: an async XHR that is going to be blocked
        // reports it on an error event, which is easy to lose.
        const xhr = new XMLHttpRequest();
        xhr.open("GET", "https://example.com/", false);
        xhr.send();
        return xhr.status;
      }),
      await attempt("WebSocket", () => {
        const socket = new WebSocket("wss://example.com/");
        return new Promise((resolve, reject) => {
          socket.onopen = () => resolve("opened");
          socket.onerror = () => reject(new Error("refused"));
          setTimeout(() => reject(new Error("no connection")), 1500);
        });
      }),
      /*
       * `new EventSource(...)` does not throw when CSP will refuse the
       * connection — it constructs happily and fails asynchronously. Testing
       * the constructor is therefore a test of nothing, which is exactly the
       * kind of mistake a conformance suite exists to avoid making; wait for
       * `open` or `error` instead.
       */
      await attempt("EventSource", () => {
        if (typeof EventSource === "undefined") throw new Error("not available in worker scope");
        const source = new EventSource("https://example.com/stream");
        return new Promise((resolve, reject) => {
          source.onopen = () => (source.close(), resolve("connected"));
          source.onerror = () => (source.close(), reject(new Error("refused")));
          setTimeout(() => (source.close(), reject(new Error("never connected"))), 1500);
        });
      }),
      await attempt("navigator.sendBeacon", () => {
        if (typeof navigator?.sendBeacon !== "function") throw new Error("not available");
        if (!navigator.sendBeacon("https://example.com/", "x")) throw new Error("returned false");
        return "queued";
      }),
      await attempt("importScripts (remote)", () => {
        if (typeof importScripts !== "function") throw new Error("not available in a module worker");
        importScripts("https://example.com/x.js");
        return "imported";
      }),
      await attempt("import() (remote)", () => import("https://example.com/x.js")),
      await attempt("__TAURI_INTERNALS__", () => {
        const found = globalThis.__TAURI_INTERNALS__ ?? globalThis.__TAURI__;
        if (!found) throw new Error("not present");
        return Object.keys(found);
      }),
      await attempt("document", () => {
        if (typeof document === "undefined") throw new Error("not present in worker scope");
        return document.title;
      }),
      await attempt("localStorage", () => {
        if (typeof localStorage === "undefined") throw new Error("not present in worker scope");
        return localStorage.length;
      }),
      await attempt("self.parent / self.top", () => {
        const up = self.parent ?? self.top;
        if (!up) throw new Error("not present in worker scope");
        return "reachable";
      }),
    ];
  },
};

async function attempt(name, fn) {
  try {
    const value = await fn();
    return { scope: "worker", name, allowed: true, detail: `succeeded (${brief(value)})` };
  } catch (error) {
    return { scope: "worker", name, allowed: false, detail: brief(error?.message ?? error) };
  }
}

function brief(value) {
  return String(value ?? "").slice(0, 120);
}
