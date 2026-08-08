/**
 * The guest half of the plugin sandbox. Runs inside the hidden iframe, on the
 * plugin's own origin, under the CSP the protocol handler set.
 *
 * It is a relay and nothing else: it owns no policy, makes no decisions, and
 * holds no state beyond the worker and the in-flight call table. Everything it
 * forwards is checked on the host side. That asymmetry is deliberate — a guest
 * that enforced its own rules would be a guest asking to be tampered with.
 *
 * It also runs a handful of escape attempts *in document scope*, because the
 * worker's own scope trivially lacks `document` and `localStorage` and would
 * therefore prove nothing about the frame containing it.
 */

(() => {
  let worker = null;
  const pending = new Map();
  let next = 1;

  const post = (message) => parent.postMessage(message, "*");

  window.addEventListener("message", async (event) => {
    // Only the embedder can reach this window, and it is the only correspondent.
    if (event.source !== parent) return;
    const message = event.data;

    switch (message?.t) {
      /*
       * Stand the worker up.
       *
       * `new Worker(blobUrl)` *constructs* without complaint in an opaque
       * origin and then fails to fetch its own script, because the blob URL is
       * `blob:null/…` and nothing can load that. The constructor succeeding is
       * therefore not evidence of anything; the first `error` event is. That
       * is reported as `fatal` rather than swallowed, because a guest that
       * cannot run a worker is a guest that cannot run a plugin, and the host
       * needs to say so rather than time out.
       */
      case "boot": {
        try {
          const url = URL.createObjectURL(
            new Blob([message.workerSource], { type: "text/javascript" }),
          );
          worker = new Worker(url, { type: "module" });
          worker.addEventListener("message", (e) => onWorker(e.data));
          worker.addEventListener("error", (e) =>
            post({
              t: "fatal",
              error: `worker failed to start (${e.message || "no message"}) — origin is ${location.origin}`,
            }),
          );
          // Revoking immediately is safe: the worker already holds the script.
          URL.revokeObjectURL(url);
          post({ t: "booted", origin: location.origin });
        } catch (error) {
          post({ t: "fatal", error: `worker could not be created: ${error?.message ?? error}` });
        }
        return;
      }

      case "load":
      case "invoke":
        return worker?.postMessage(message);

      case "reply": {
        const waiting = pending.get(message.id);
        pending.delete(message.id);
        return waiting?.(message);
      }

      /*
       * Kill a spinning plugin.
       *
       * `Worker.terminate()` is the only thing that stops JS that will not
       * yield; a per-call timeout on the host abandons the *call* but leaves
       * the loop burning a core. Everything in flight is failed rather than
       * left to time out one by one.
       */
      case "terminate": {
        worker?.terminate();
        worker = null;
        for (const waiting of pending.values()) {
          waiting({ ok: false, error: "the plugin was terminated" });
        }
        pending.clear();
        return post({ t: "terminated", id: message.id });
      }

      case "guest-probe":
        return post({
          t: "result",
          id: message.id,
          ok: true,
          value: await probeDocumentScope(message.ctx ?? {}),
        });
    }
  });

  /** Worker → host, with `mach.*` calls given an id the guest can match a reply to. */
  function onWorker(message) {
    if (message?.t !== "call") return post(message);

    const id = next++;
    pending.set(id, (reply) => worker?.postMessage({ t: "reply", id: message.id, ...reply }));
    post({ t: "call", id, method: message.method, args: message.args });
  }

  /**
   * The escapes that only make sense from a document.
   *
   * The storage rows deserve a note, because the obvious version of this test
   * is wrong and the PoC caught it. Asking "does `localStorage` exist here?"
   * proves nothing: on a distinct origin it exists and is *empty*, which is the
   * whole point of using a distinct origin. The question worth asking is
   * whether the app's own storage is reachable, so the host plants a sentinel
   * and this looks for it.
   */
  async function probeDocumentScope({ appOrigin = "", sentinelKey = "", sentinel = "" }) {
    return [
      await attempt("guest", "fetch (remote)", () => fetch("https://example.com/")),
      await attempt("guest", "fetch (app origin)", () => fetch(`${appOrigin}/index.html`)),
      await attempt("guest", "XMLHttpRequest (remote)", () => {
        const xhr = new XMLHttpRequest();
        xhr.open("GET", "https://example.com/", false);
        xhr.send();
        return xhr.status;
      }),
      await attempt("guest", "<img> to a remote host", () => {
        const img = new Image();
        return new Promise((resolve, reject) => {
          img.onload = () => resolve("loaded");
          img.onerror = () => reject(new Error("blocked"));
          img.src = "https://example.com/favicon.ico";
          setTimeout(() => reject(new Error("never loaded")), 1500);
        });
      }),
      await attempt("guest", "origin is not the app's", () => {
        if (location.origin === appOrigin) return `same origin as the app (${appOrigin})`;
        throw new Error(`${location.origin} ≠ ${appOrigin}`);
      }),
      await attempt("guest", "read app localStorage", () => {
        const found = localStorage.getItem(sentinelKey);
        if (found === sentinel) return "sentinel visible";
        throw new Error("own partition, sentinel absent");
      }),
      await attempt("guest", "read app cookies", () => {
        if (sentinel && document.cookie.includes(sentinel)) return "sentinel visible";
        throw new Error("own partition, sentinel absent");
      }),
      await attempt("guest", "read parent.document", () => parent.document.title),
      await attempt("guest", "globalThis.__TAURI_INTERNALS__", () => {
        const found = globalThis.__TAURI_INTERNALS__ ?? globalThis.__TAURI__;
        if (found) return Object.keys(found).join(",");
        throw new Error("not present");
      }),
      await attempt("guest", "remove own sandbox attribute", () => {
        // The reason `allow-same-origin` would be fatal on the app's own
        // origin: with it, this succeeds and the frame reloads unsandboxed.
        // Here the cross-origin barrier is what stops it.
        const self = parent.document.querySelector("iframe");
        self.removeAttribute("sandbox");
        return "removed";
      }),
    ];
  }

  async function attempt(scope, name, fn) {
    try {
      await fn();
      return { scope, name, allowed: true, detail: "succeeded" };
    } catch (error) {
      return { scope, name, allowed: false, detail: String(error?.message ?? error).slice(0, 160) };
    }
  }

  post({ t: "ready", origin: location.origin });
})();
