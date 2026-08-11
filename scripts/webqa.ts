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

const CHROME =
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const URL_UNDER_TEST = process.env.MACH_WEB_URL ?? "http://localhost:1420";
const PORT = Number(process.env.MACH_CDP_PORT ?? 9333);
const VIEWPORT = process.env.MACH_VIEWPORT ?? "1440,900";

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
    if (await responsive()) return;
    console.error("webqa: the running browser is wedged — restarting it");
    Bun.spawnSync(["pkill", "-f", `remote-debugging-port=${PORT}`]);
    await Bun.sleep(500);
  }

  Bun.spawn(
    [
      CHROME,
      "--headless=new",
      `--remote-debugging-port=${PORT}`,
      `--window-size=${VIEWPORT}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-gpu",
      // Its own profile, so it cannot touch the user's Chrome session.
      `--user-data-dir=${process.cwd()}/.qa/chrome`,
      URL_UNDER_TEST,
    ],
    { stdout: "ignore", stderr: "ignore" },
  );

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

/** Does it still answer CDP, or does it merely still answer HTTP? */
async function responsive(): Promise<boolean> {
  try {
    const result = await Promise.race([
      cdp("Runtime.evaluate", { expression: "1+1", returnByValue: true }),
      Bun.sleep(4000).then(() => null),
    ]);
    return (result as { result?: { value?: unknown } } | null)?.result?.value === 2;
  } catch {
    return false;
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

try {
  await ensureChrome();

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
      await fetch(`http://127.0.0.1:${PORT}/json/close`).catch(() => {});
      const found = Bun.spawnSync(["pkill", "-f", `remote-debugging-port=${PORT}`]);
      console.log(found.exitCode === 0 ? "stopped" : "not running");
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
