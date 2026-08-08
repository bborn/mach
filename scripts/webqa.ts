#!/usr/bin/env bun
/**
 * Headless QA for the Mach frontend. No window, no focus, ever.
 *
 * Launching the real app to look at it takes the keyboard away from whoever is
 * using the machine: a new Tauri window activates on macOS whether you wanted
 * it to or not. So agents do not launch the app. They drive the same frontend
 * here instead — Vite serves it on :1420, and outside Tauri `src/main.tsx`
 * swaps in the fixture data source, so it renders fully with no backend.
 *
 * Chrome is already installed and speaks CDP over a websocket, and Bun has a
 * websocket client built in, so this needs nothing installed.
 *
 *   bun scripts/webqa.ts shoot out.png
 *   bun scripts/webqa.ts eval 'document.querySelectorAll("[data-thread-id]").length'
 *   bun scripts/webqa.ts click '[data-thread-id]'
 *   bun scripts/webqa.ts key 'j'
 *
 * What it cannot do: anything that needs real mail or the Rust backend. Use
 * `scripts/qa state` for that — it reads the database and needs no UI.
 */

const CHROME =
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const URL_UNDER_TEST = process.env.MACH_WEB_URL ?? "http://localhost:1420";
const PORT = Number(process.env.MACH_CDP_PORT ?? 9333);
const VIEWPORT = process.env.MACH_VIEWPORT ?? "1440,900";

async function ensureChrome(): Promise<void> {
  // Reuse a live instance so a sequence of commands shares one page.
  if (await probe()) return;

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
    if (await probe()) return;
    await Bun.sleep(250);
  }
  throw new Error(`headless Chrome did not come up on :${PORT}`);
}

async function probe(): Promise<boolean> {
  try {
    const r = await fetch(`http://127.0.0.1:${PORT}/json/version`);
    return r.ok;
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

    case "reload": {
      await cdp("Page.navigate", { url: URL_UNDER_TEST });
      await evaluate("await new Promise(r => setTimeout(r, 1200)); return 1;");
      console.log("reloaded");
      break;
    }

    case "console": {
      const logs = await evaluate(`return (window.__machLogs ?? []).slice(-50);`);
      console.log(JSON.stringify(logs, null, 1));
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
