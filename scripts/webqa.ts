#!/usr/bin/env bun
/**
 * Headless QA for the Mach frontend in Chrome. No window, no focus, ever.
 *
 * Vite serves the frontend on :1420, and outside Tauri `src/main.tsx` swaps in
 * the fixture data source, so it renders fully with no backend. Chrome is
 * already installed and speaks CDP over a websocket, and Bun has a websocket
 * client built in, so this needs nothing installed.
 *
 *   bun scripts/webqa.ts shoot out.png
 *   bun scripts/webqa.ts eval 'document.querySelectorAll("[data-thread-id]").length'
 *   bun scripts/webqa.ts click '[data-thread-id]'
 *   bun scripts/webqa.ts key 'j'
 *
 * # This is not the app
 *
 * It is Blink, in a tab, against fixtures. That makes it right for component
 * and logic work and wrong for anything the OS also draws or WebKit decides.
 * Four defects shipped verified here: preferences with its title under the
 * traffic lights, which a browser does not have; trackpad period navigation,
 * which could not be exercised in WebKit at all; discard-draft and send-draft,
 * because opening a composer needs a keystroke the real window could not be
 * given; and a dead link, because WebKit will not fire a listener in a
 * scripting-disabled document and Blink will.
 *
 * The real window can now be driven without touching anybody's focus, so it is
 * the answer for all of those:
 *
 *   MACH_QA_INSTANCE=agent scripts/qa up
 *   MACH_QA_INSTANCE=agent scripts/qa key 'mod+,'
 *   MACH_QA_INSTANCE=agent scripts/qa shoot prefs
 *   MACH_QA_INSTANCE=agent scripts/qa ui
 *
 * What this cannot do at all: anything that needs real mail or the Rust
 * backend. `scripts/qa state` reads the database and needs no UI.
 */

import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";

/**
 * The browser binary, and why it is still full Chrome unless you say otherwise.
 *
 * Since Chrome 132 `--headless=new` *is* Chrome — the whole browser, windowless
 * — and the stripped-down renderer that used to be `--headless` ships
 * separately as `chrome-headless-shell`, which wants a few hundred megabytes
 * less. This harness asks for DOM, keystrokes and screenshots and nothing else,
 * so the shell would do — but its screenshots are not the same picture. Asked
 * for the same `--window-size=1440,900`, Chrome captures 1440x813 and the shell
 * captures the full 1440x900, so every baseline under `.qa/` would have to be
 * retaken. Worth the memory if somebody wants it; not worth doing to a working
 * harness by default.
 *
 * It is opt-in through WEBQA_CHROME rather than found automatically, and the
 * first attempt at finding it automatically is why. `Bun.which` turned up a
 * `chrome-headless-shell` on PATH that was a symlink into Playwright's
 * versioned cache; Chrome resolves `icudtl.dat` against the directory it was
 * launched from, that directory held a symlink and nothing else, and every
 * command died with `icudtl.dat not found in bundle`. A binary somebody put on
 * PATH is not necessarily a binary sitting next to its own resources, and
 * guessing costs more than it saves.
 */
const CHROME =
  process.env.WEBQA_CHROME ??
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const IS_SHELL = /chrome-headless-shell/.test(CHROME);

/**
 * The dev server this drives, and the debugger port it drives Chrome on.
 *
 * Both used to be constants — :1420 and :9333 — and both were wrong on a
 * machine where several agents work at once.
 *
 * :1420 is the *owner's* dev server, feeding the window he reads mail in. An
 * agent running this from a worktree was reading and clicking his frontend
 * rather than its own. `scripts/qa` now gives every instance a dev server of
 * its own and records the port in `.qa/<instance>/vite.json`, so when
 * MACH_QA_INSTANCE names an instance that is up, that is the server to use.
 *
 * :9333 is worse, because `ensureChrome` puts down a browser it thinks is
 * wedged with `pkill -f remote-debugging-port=9333` — which is every other
 * agent's browser too. Derived from the dev-server port, one debugger per
 * instance, so nobody kills anybody.
 *
 * MACH_WEB_URL and MACH_CDP_PORT still win when set.
 */
function instanceDevPort(): number | null {
  const instance = process.env.MACH_QA_INSTANCE;
  if (!instance || instance === "main") return null;
  try {
    const repo = new URL("..", import.meta.url).pathname;
    const state = JSON.parse(
      readFileSync(`${repo}.qa/${instance}/vite.json`, "utf8"),
    );
    return typeof state.port === "number" ? state.port : null;
  } catch {
    // No instance up, or no dev server recorded for it. :1420 below is then the
    // only thing there is, and it is the owner's — so say so rather than
    // quietly driving his window.
    return null;
  }
}

const DEV_PORT = instanceDevPort();
const URL_UNDER_TEST =
  process.env.MACH_WEB_URL ?? `http://localhost:${DEV_PORT ?? 1420}`;
const PORT = Number(
  process.env.MACH_CDP_PORT ?? (DEV_PORT ? 9000 + (DEV_PORT % 1000) : 9333),
);
const VIEWPORT = process.env.MACH_VIEWPORT ?? "1440,900";

// Unmissable, because the failure it prevents is silent: reading and clicking
// the owner's live frontend while believing you are looking at your own.
if (!process.env.MACH_WEB_URL && DEV_PORT === null && URL_UNDER_TEST.includes(":1420")) {
  console.error(
    "webqa: driving http://localhost:1420 — that is the OWNER'S dev server.\n" +
      "       Bring your own instance up first (MACH_QA_INSTANCE=agent scripts/qa up)\n" +
      "       and this follows it, or set MACH_WEB_URL explicitly.",
  );
}

/* -------------------------------------------------------------------------- */
/* Whose job it is to close the browser                                        */
/* -------------------------------------------------------------------------- */

/**
 * Nobody's, until now — and a headless Chrome was found alive after a day and
 * a half, reparented to launchd, listening on its debugger port with nothing
 * connected to it. It was one of several, and together they filled 24GB of RAM
 * and pinned swap.
 *
 * The browser is deliberately long-lived: a sequence of commands shares one
 * page, which is the whole reason `ensureChrome` reuses a running instance
 * rather than starting one per command. That part is right and stays. What was
 * missing is an owner for its *death*. The only two things that ever ended it
 * were an explicit `webqa stop` and the next run finding it wedged, and an
 * agent whose session ends — the ordinary case — does neither.
 *
 * So the browser now outlives its caller on a lease rather than forever:
 *
 *  1. **An idle timer**, held by a watchdog process rather than by this CLI,
 *     because this CLI exits in a second and the leak is measured in days. Every
 *     CDP round trip stamps `lastActivity`; the watchdog closes the browser once
 *     that goes stale. This is the part that works without anyone remembering
 *     anything.
 *  2. **A hard maximum age**, checked on reuse. A browser can be kept alive
 *     indefinitely by a script that pokes it every few minutes, and an
 *     eight-hour-old renderer is worth restarting whatever it says about itself.
 *  3. **A recorded pid**, so both of those can put down *this* browser by name
 *     rather than by `pkill -f remote-debugging-port=…`, which matches every
 *     other checkout's instance on the machine.
 */
const STATE_DIR = `${process.cwd()}/.qa`;
const STATE_FILE = `${STATE_DIR}/chrome-${PORT}.json`;

/** Minutes of no CDP traffic before the watchdog closes the browser. */
const IDLE_MINUTES = Number(process.env.WEBQA_IDLE_TIMEOUT ?? 30);
/** Hours after which a running browser is replaced on the next command. */
const MAX_AGE_HOURS = Number(process.env.WEBQA_MAX_AGE ?? 4);

interface ChromeState {
  pid: number;
  port: number;
  startedAt: number;
  lastActivity: number;
}

function readState(): ChromeState | null {
  try {
    return JSON.parse(readFileSync(STATE_FILE, "utf8")) as ChromeState;
  } catch {
    return null;
  }
}

function writeState(state: ChromeState): void {
  try {
    mkdirSync(STATE_DIR, { recursive: true });
    writeFileSync(STATE_FILE, JSON.stringify(state));
  } catch {
    // A browser we cannot write a note about is still a browser that works.
    // The watchdog is the thing that suffers, and it fails safe: no state file
    // reads as "gone", and it exits rather than killing something at random.
  }
}

function forgetState(): void {
  try {
    rmSync(STATE_FILE, { force: true });
  } catch {
    /* nothing to forget */
  }
}

/** Stamp the browser as in use. Called on every CDP round trip. */
function touchState(): void {
  const state = readState();
  if (state) writeState({ ...state, lastActivity: Date.now() });
}

/** Is this pid still ours to talk to? */
function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/**
 * Ask the browser to exit, the way the browser wants to be asked.
 *
 * `Browser.close` on the browser-level socket — not the page socket every other
 * command here uses — shuts the whole thing down cleanly, flushing the profile.
 * The pid is the fallback for a browser too wedged to answer, and it is a
 * fallback rather than the mechanism precisely because SIGKILL on a Chrome
 * mid-write is how a user-data-dir gets corrupted.
 */
async function closeBrowser(): Promise<boolean> {
  const state = readState();
  let closed = false;
  try {
    const version = (await (
      await fetch(`http://127.0.0.1:${PORT}/json/version`)
    ).json()) as { webSocketDebuggerUrl?: string };
    if (version.webSocketDebuggerUrl) {
      const ws = new WebSocket(version.webSocketDebuggerUrl);
      await new Promise<void>((resolve) => {
        const done = () => resolve();
        const timer = setTimeout(done, 3000);
        ws.onopen = () => ws.send(JSON.stringify({ id: 1, method: "Browser.close" }));
        ws.onmessage = () => {
          clearTimeout(timer);
          done();
        };
        ws.onerror = () => {
          clearTimeout(timer);
          done();
        };
        ws.onclose = () => {
          clearTimeout(timer);
          done();
        };
      });
      closed = true;
    }
  } catch {
    // Not answering. The pid below is what is left.
  }

  if (state?.pid && alive(state.pid)) {
    for (const signal of ["SIGTERM", "SIGKILL"] as const) {
      try {
        process.kill(state.pid, signal);
      } catch {
        break;
      }
      for (let i = 0; i < 20; i++) {
        if (!alive(state.pid)) break;
        await Bun.sleep(100);
      }
      if (!alive(state.pid)) break;
    }
    closed = true;
  }

  forgetState();
  return closed;
}

/**
 * The watchdog: one detached process per browser, polling the note the CLI
 * leaves behind.
 *
 * It is a re-exec of this same file rather than a second script because it has
 * to agree with the CLI about the port, the state file and how to close a
 * browser, and the cheapest way to guarantee that is to be the same code.
 *
 * It exits when the browser does, so it cannot become the orphan it exists to
 * prevent.
 */
async function watchdog(): Promise<void> {
  const idleMs = Math.max(1, IDLE_MINUTES) * 60_000;
  for (;;) {
    await Bun.sleep(15_000);
    const state = readState();
    // Somebody else already put it down, or the note is gone. Either way this
    // watchdog no longer has anything to watch.
    if (!state || !alive(state.pid)) {
      forgetState();
      return;
    }
    if (Date.now() - state.lastActivity < idleMs) continue;
    await closeBrowser();
    return;
  }
}

function startWatchdog(): void {
  const child = Bun.spawn([process.execPath, import.meta.path, "__watchdog"], {
    stdout: "ignore",
    stderr: "ignore",
    stdin: "ignore",
    env: { ...process.env, MACH_CDP_PORT: String(PORT) },
  });
  // The CLI is about to exit and must not wait for a process designed to
  // outlive it.
  child.unref();
}

async function ensureChrome(): Promise<void> {
  // Reuse a live instance so a sequence of commands shares one page — but only
  // if it is actually alive.
  //
  // `probe()` asks the HTTP endpoint, and a Chrome whose renderer has wedged
  // still answers that perfectly while every CDP command hangs to its timeout.
  // A stale browser left over from an earlier session cost 120 seconds a
  // command and looked, from the outside, exactly like a broken frontend. So
  // reuse requires a real round trip, and anything less gets put down.
  if (await probe()) {
    const state = readState();
    const ageMs = state ? Date.now() - state.startedAt : null;
    const tooOld = ageMs !== null && ageMs > Math.max(1, MAX_AGE_HOURS) * 3_600_000;
    if (!tooOld && (await responsive())) {
      touchState();
      return;
    }
    console.error(
      tooOld
        ? `webqa: the running browser is ${Math.round(ageMs! / 3_600_000)}h old — restarting it`
        : "webqa: the running browser is wedged — restarting it",
    );
    await closeBrowser();
    await Bun.sleep(500);
  }

  const child = Bun.spawn(
    [
      CHROME,
      // The shell binary is headless by construction and rejects the flag.
      ...(IS_SHELL ? [] : ["--headless=new"]),
      `--remote-debugging-port=${PORT}`,
      `--window-size=${VIEWPORT}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-gpu",
      // Its own profile, so it cannot touch the user's Chrome session — and
      // one per debugger port, because Chrome takes an exclusive lock on a
      // profile directory. Every instance shared `.qa/chrome`, so the careful
      // work above to give each one its own port bought nothing: the second
      // browser to start could not start at all, and said only "headless
      // Chrome did not come up".
      `--user-data-dir=${STATE_DIR}/chrome-${PORT}`,
      URL_UNDER_TEST,
    ],
    // Its stderr goes to a file rather than to /dev/null. A browser that fails
    // to start says why, once, on the way out, and throwing that away left
    // `headless Chrome did not come up on :9444` as the only evidence — which
    // is a description of the silence, not of the fault.
    { stdout: "ignore", stderr: Bun.file(`${STATE_DIR}/chrome-${PORT}.log`) },
  );
  // Outliving this command is the point; not outliving the week is the fix.
  child.unref();
  const now = Date.now();
  writeState({ pid: child.pid, port: PORT, startedAt: now, lastActivity: now });
  startWatchdog();

  for (let i = 0; i < 60; i++) {
    if (await probe()) {
      await installLogCapture();
      return;
    }
    await Bun.sleep(250);
  }
  throw new Error(`headless Chrome did not come up on :${PORT}`);
}

/**
 * Record console output and uncaught errors into the page.
 *
 * Registered to run before any of the app's own script on every navigation, so
 * an exception thrown while the module graph is still loading — the kind that
 * leaves a blank page and no other trace — is still caught.
 */
async function installLogCapture(): Promise<void> {
  const script = `
    (() => {
      if (window.__machLogs) return;
      const logs = window.__machLogs = [];
      const keep = (line) => { logs.push(line); if (logs.length > 200) logs.shift(); };
      for (const level of ["log", "info", "warn", "error", "debug"]) {
        const original = console[level].bind(console);
        console[level] = (...args) => {
          keep("[" + level + "] " + args.map((a) =>
            a instanceof Error ? (a.stack || a.message) : typeof a === "string" ? a : (() => {
              try { return JSON.stringify(a); } catch { return String(a); }
            })()
          ).join(" "));
          original(...args);
        };
      }
      addEventListener("error", (e) =>
        keep("[uncaught] " + (e.error?.stack || e.message)));
      addEventListener("unhandledrejection", (e) =>
        keep("[unhandled promise] " + (e.reason?.stack || e.reason)));
    })();
  `;
  try {
    await cdp("Page.addScriptToEvaluateOnNewDocument", { source: script });
    // Also arm the page that is already open, so `console` is useful without
    // a reload for anything logged from here on.
    await evaluate(script + " return 1;");
  } catch {
    // Capture is a convenience; never let it stop the command you asked for.
  }
}

async function probe(): Promise<boolean> {
  try {
    const r = await fetch(`http://127.0.0.1:${PORT}/json/version`);
    return r.ok;
  } catch {
    return false;
  }
}

/**
 * Does it still answer CDP, or does it merely still answer HTTP?
 *
 * The loser of the race has to be cancelled. It used to be a bare
 * `Bun.sleep(4000)`, and a promise that has lost a race is still a pending
 * timer: every command that reused a healthy browser answered in milliseconds
 * and then sat there for the rest of the four seconds with nothing to do,
 * because the process cannot exit while a timer is armed. Three commands in a
 * row cost twelve seconds of doing nothing.
 */
async function responsive(): Promise<boolean> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const result = await Promise.race([
      cdp("Runtime.evaluate", { expression: "1+1", returnByValue: true }),
      new Promise<null>((resolve) => {
        timer = setTimeout(() => resolve(null), 4000);
      }),
    ]);
    return (result as { result?: { value?: unknown } } | null)?.result?.value === 2;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

async function pageSocket(): Promise<string> {
  const targets = (await (
    await fetch(`http://127.0.0.1:${PORT}/json`)
  ).json()) as { type: string; url: string; webSocketDebuggerUrl: string }[];
  const page = targets.find(
    (t) => t.type === "page" && !t.url.startsWith("devtools://"),
  );
  if (!page) throw new Error("no page target in headless Chrome");
  return page.webSocketDebuggerUrl;
}

/** One CDP round trip. */
async function cdp(method: string, params: unknown = {}): Promise<any> {
  const ws = new WebSocket(await pageSocket());
  const id = 1;
  const result = await new Promise<any>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${method} timed out`)), 30_000);
    ws.onopen = () => ws.send(JSON.stringify({ id, method, params }));
    ws.onmessage = (event) => {
      const msg = JSON.parse(String(event.data));
      if (msg.id !== id) return;
      clearTimeout(timer);
      msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
    };
    ws.onerror = () => {
      clearTimeout(timer);
      reject(new Error("cdp socket error"));
    };
  });
  ws.close();
  // The watchdog reads this. A command that talks to the browser is the only
  // evidence there is that anyone still wants it.
  touchState();
  return result;
}

/**
 * Reloads the page and reports the errors and warnings the load produced.
 *
 * Returns console errors, uncaught exceptions and rejected promises — not the
 * ordinary chatter, which is mostly Vite's connect/hot-update log and drowns
 * the one line that matters.
 */
async function reloadAndWatch(): Promise<string[]> {
  const ws = new WebSocket(await pageSocket());
  const found: string[] = [];
  let id = 0;
  const send = (method: string, params: unknown = {}) =>
    ws.send(JSON.stringify({ id: ++id, method, params }));

  await new Promise<void>((resolve, reject) => {
    ws.onopen = () => resolve();
    ws.onerror = () => reject(new Error("cdp socket error"));
    setTimeout(() => reject(new Error("cdp did not connect")), 10_000);
  });

  ws.onmessage = (event) => {
    const msg = JSON.parse(String(event.data));
    if (msg.method === "Runtime.exceptionThrown") {
      const d = msg.params.exceptionDetails;
      found.push(`[exception] ${d.exception?.description ?? d.text}`);
    }
    if (msg.method === "Runtime.consoleAPICalled" && ["error", "warning"].includes(msg.params.type)) {
      const text = (msg.params.args ?? [])
        .map((a: { value?: unknown; description?: string }) => a.description ?? String(a.value))
        .join(" ");
      found.push(`[${msg.params.type}] ${text}`);
    }
  };

  send("Runtime.enable");
  send("Page.enable");
  send("Page.navigate", { url: URL_UNDER_TEST });

  // Long enough for the module graph, the fixtures and the first render.
  await Bun.sleep(2500);
  ws.close();
  return found;
}

async function evaluate(expression: string): Promise<unknown> {
  const r = await cdp("Runtime.evaluate", {
    expression: `(async () => { ${expression} })()`,
    awaitPromise: true,
    returnByValue: true,
  });
  if (r.exceptionDetails) {
    throw new Error(r.exceptionDetails.exception?.description ?? "evaluation failed");
  }
  return r.result?.value;
}

const [command, ...rest] = process.argv.slice(2);

// The watchdog is this file re-executed, and it must not do what this file
// normally does: `ensureChrome` would start a browser, which is the opposite
// of the job. It also never returns, so it is handled before the switch.
if (command === "__watchdog") {
  await watchdog();
  process.exit(0);
}

try {
  // `stop` is the one command that must work when there is nothing running,
  // and starting a browser in order to close it is a strange way to spend
  // four seconds.
  if (command !== "stop") await ensureChrome();

  switch (command) {
    case "shoot": {
      const out = rest[0] ?? ".qa/web.png";
      // Settle: fonts, fixtures and the first paint.
      await evaluate("await new Promise(r => setTimeout(r, 800)); return 1;");
      const { data } = await cdp("Page.captureScreenshot", { format: "png" });
      await Bun.write(out, Buffer.from(data, "base64"));
      console.log(`${out}  (headless, no window, no focus)`);
      break;
    }

    case "eval": {
      const expr = rest.join(" ");
      if (!expr) throw new Error("usage: webqa eval '<javascript>'");
      const value = await evaluate(
        expr.includes("return") ? expr : `return (${expr});`,
      );
      console.log(typeof value === "string" ? value : JSON.stringify(value, null, 1));
      break;
    }

    case "click": {
      const selector = rest[0];
      if (!selector) throw new Error("usage: webqa click '<css selector>'");
      // Dispatched in-page rather than as an OS event, so nothing is focused.
      const hit = await evaluate(`
        const el = document.querySelector(${JSON.stringify(selector)});
        if (!el) return false;
        el.scrollIntoView({ block: "center" });
        el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, view: window }));
        return true;
      `);
      if (!hit) throw new Error(`no element matched ${selector}`);
      console.log(`clicked ${selector}`);
      break;
    }

    case "key": {
      const key = rest[0];
      if (!key) throw new Error("usage: webqa key '<key>' [--meta] [--shift]");
      const mods = rest.slice(1);
      await evaluate(`
        window.dispatchEvent(new KeyboardEvent("keydown", {
          key: ${JSON.stringify(key)},
          code: ${JSON.stringify(/^[0-9]$/.test(key) ? `Digit${key}` : `Key${key.toUpperCase()}`)},
          metaKey: ${mods.includes("--meta")},
          ctrlKey: ${mods.includes("--ctrl")},
          altKey: ${mods.includes("--alt")},
          shiftKey: ${mods.includes("--shift")},
          bubbles: true, cancelable: true,
        }));
        await new Promise(r => setTimeout(r, 250));
        return 1;
      `);
      console.log(`sent ${[...mods, key].join(" ")}`);
      break;
    }

    /*
     * Typing, as the engine understands it.
     *
     * `key` above dispatches a KeyboardEvent from script, which every listener
     * sees and which edits nothing: a synthetic event is not trusted, so no
     * character is inserted, no character is deleted, and Enter makes no line.
     * That is fine for asking what a *binding* does and useless for asking
     * whether the composer can be written in — the question that arrived as "I
     * can't even use it, backspace doesn't work" and that no harness here could
     * answer.
     *
     * `Input.dispatchKeyEvent` goes in at the browser's own input layer, so the
     * editor is edited for real. Text goes in through `Input.insertText`, which
     * is what the IME path does and is far faster than a keystroke per letter.
     *
     *   bun scripts/webqa.ts type 'hello'
     *   bun scripts/webqa.ts press Enter
     *   bun scripts/webqa.ts press Backspace
     */
    case "type": {
      const text = rest.join(" ");
      if (!text) throw new Error("usage: webqa type '<text>'");
      await cdp("Input.insertText", { text });
      await Bun.sleep(150);
      console.log(`typed ${JSON.stringify(text)}`);
      break;
    }

    /*
     * The same text, one keystroke at a time.
     *
     * `type` above is one `Input.insertText`, which is the IME path: fast, and
     * a single edit. An editor that keeps its own idea of where the caret is —
     * Squire does — updates that idea from the events a *keystroke* produces,
     * so a bug that depends on the editor's cached selection will not reproduce
     * under `insertText` and will under this. Use this one before believing
     * anything about caret behaviour.
     */
    case "keys": {
      const text = rest.join(" ");
      if (!text) throw new Error("usage: webqa keys '<text>'");
      for (const ch of text) {
        await cdp("Input.dispatchKeyEvent", { type: "keyDown", text: ch, key: ch });
        await cdp("Input.dispatchKeyEvent", { type: "keyUp", key: ch });
      }
      await Bun.sleep(200);
      console.log(`keyed ${JSON.stringify(text)}`);
      break;
    }

    case "press": {
      const name = rest[0];
      if (!name) throw new Error("usage: webqa press <Enter|Backspace|Tab|…>");
      // The pairs Blink needs to treat a key as pressed. `text` is what makes
      // Enter insert a line rather than merely being observed.
      const known: Record<string, { code: string; vk: number; text?: string }> = {
        Enter: { code: "Enter", vk: 13, text: "\r" },
        Backspace: { code: "Backspace", vk: 8 },
        Tab: { code: "Tab", vk: 9 },
        Delete: { code: "Delete", vk: 46 },
        Escape: { code: "Escape", vk: 27 },
        ArrowLeft: { code: "ArrowLeft", vk: 37 },
        ArrowRight: { code: "ArrowRight", vk: 39 },
      };
      const spec = known[name];
      if (!spec) throw new Error(`press does not know ${name}`);
      for (const type of ["keyDown", "keyUp"] as const) {
        await cdp("Input.dispatchKeyEvent", {
          type,
          key: name,
          code: spec.code,
          windowsVirtualKeyCode: spec.vk,
          nativeVirtualKeyCode: spec.vk,
          ...(type === "keyDown" && spec.text ? { text: spec.text } : {}),
        });
      }
      await Bun.sleep(150);
      console.log(`pressed ${name}`);
      break;
    }

    case "reload": {
      // Reload holds one socket open across the navigation, because that is
      // the only way to hear the load itself. Every other command opens a
      // socket, asks one question and closes it — fine for a settled page,
      // useless for "why is this page blank", where the exception is thrown
      // before anything you could later ask has a chance to exist.
      const noise = await reloadAndWatch();
      console.log("reloaded");
      if (noise.length) {
        console.log("");
        console.log(noise.join("\n"));
      }
      break;
    }

    case "console": {
      const logs = (await evaluate(`return (window.__machLogs ?? null);`)) as
        | string[]
        | null;
      if (logs === null) {
        // The hook is installed on navigation, so a page loaded before this
        // ran has no record. Saying so beats printing "[]", which reads as
        // "the page is clean" — the failure mode this command exists to catch.
        console.log("no capture on this page yet — run `webqa reload` first");
        break;
      }
      console.log(logs.length ? logs.join("\n") : "(nothing logged since load)");
      break;
    }

    case "stop": {
      const running = (await probe()) || readState() !== null;
      const closed = await closeBrowser();
      console.log(running && closed ? "stopped" : "not running");
      break;
    }

    default:
      console.log(
        [
          "headless QA — no window, no focus",
          "",
          "  bun scripts/webqa.ts shoot [out.png]",
          "  bun scripts/webqa.ts eval '<javascript>'",
          "  bun scripts/webqa.ts click '<css selector>'",
          "  bun scripts/webqa.ts key '<key>' [--meta --shift --alt --ctrl]",
          "  bun scripts/webqa.ts reload",
          "  bun scripts/webqa.ts stop",
          "",
          `The browser is shared between commands and closes itself after ${IDLE_MINUTES}`,
          `minutes idle (WEBQA_IDLE_TIMEOUT), or is replaced once it is ${MAX_AGE_HOURS}h`,
          "old (WEBQA_MAX_AGE).",
          "",
          `Drives ${URL_UNDER_TEST} — the same frontend, with fixture data.`,
          "For real mail, use `scripts/qa state`, which reads the database.",
        ].join("\n"),
      );
      process.exit(1);
  }
} catch (error) {
  console.error(`webqa: ${error instanceof Error ? error.message : error}`);
  process.exit(1);
}
