/**
 * The worker shim: where plugin code actually runs.
 *
 * It imports the plugin as a module from a blob: URL, builds the `mach` object,
 * and turns every method on it into a round trip to the host. There is no
 * ambient anything — if a plugin wants to affect the world it has to ask, and
 * asking goes through a channel the host reads.
 *
 * The plugin never sees this file. It sees `ctx.mach` and its own exports.
 */

let plugin = null;
let nextCall = 1;
const pending = new Map();

/** The `mach.*` surface, as flat method names the host can check one by one. */
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

/** Ask the host to do something. Every one of these can be refused. */
function call(method, args) {
  const id = nextCall++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    self.postMessage({ t: "call", id, method, args });
  });
}

/**
 * `mach`, built from the method list.
 *
 * Nested by the dots in the names, so `read.labels` becomes `mach.read.labels`
 * and the host still sees one flat string to check a capability against. One
 * list, one place to add a method, no chance of the two representations
 * disagreeing.
 */
function makeMach() {
  const mach = {
    now: () => Date.now(),
  };
  for (const method of METHODS) {
    const [head, tail] = method.split(".");
    if (tail === undefined) {
      mach[head] = (...args) => call(method, args);
    } else {
      (mach[head] ??= {})[tail] = (...args) => call(method, args);
    }
  }
  return Object.freeze(mach);
}

const mach = makeMach();

self.addEventListener("message", async (event) => {
  const message = event.data;

  if (message?.t === "reply") {
    const waiting = pending.get(message.id);
    pending.delete(message.id);
    if (!waiting) return;
    return message.ok ? waiting.resolve(message.value) : waiting.reject(new Error(message.error));
  }

  if (message?.t === "load") {
    try {
      const url = URL.createObjectURL(new Blob([message.source], { type: "text/javascript" }));
      plugin = await import(url);
      URL.revokeObjectURL(url);
      self.postMessage({
        t: "result",
        id: message.id,
        ok: true,
        value: {
          actions: Object.keys(plugin.actions ?? {}),
          views: Object.keys(plugin.views ?? {}),
          events: Object.keys(plugin.events ?? {}),
        },
      });
    } catch (error) {
      self.postMessage({ t: "result", id: message.id, ok: false, error: String(error) });
    }
    return;
  }

  if (message?.t === "invoke") {
    try {
      const table = plugin?.[message.kind];
      const handler = table?.[message.name];
      if (!handler) throw new Error(`no ${message.kind} named "${message.name}"`);
      const value = await handler({ ...message.ctx, mach });
      self.postMessage({ t: "result", id: message.id, ok: true, value: value ?? null });
    } catch (error) {
      self.postMessage({
        t: "result",
        id: message.id,
        ok: false,
        error: String(error?.message ?? error),
      });
    }
  }
});
