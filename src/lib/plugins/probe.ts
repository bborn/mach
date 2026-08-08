/**
 * The step-0 harness: run the conformance probe and report the verdict.
 *
 * Loaded instead of the app when `globalThis.__MACH_PLUGIN_PROBE__` is set,
 * which only `src-tauri/src/bin/plugin_probe.rs` does. It renders a plain
 * table because a human will read it once and a machine reads the JSON the Rust
 * side writes.
 *
 * Nothing here is test scaffolding that the app does not also run:
 * `runConformance` is the same function the plugin host calls at boot, over the
 * same protocol, with the same guest and the same worker. The only difference is
 * that this binary has no mailbox and never shows its window.
 */

import { runConformance, describeConformance } from "./conformance";
import { createPluginBackend } from "./backend";
import { PluginManager } from "./manager";
import { tauriTransport } from "@/lib/ipc";
import { fixtureSource } from "@/lib/data";
import type { ConformanceReport } from "./types";

/** Trace back to the probe binary's stderr. There is no console to read here. */
async function say(message: string): Promise<void> {
  try {
    await tauriTransport.invoke("probe_log", { message });
  } catch {
    /* not running under the probe binary */
  }
}

export async function runProbeAndReport(): Promise<ConformanceReport> {
  const backend = createPluginBackend(tauriTransport);

  /*
   * Anything that goes wrong here is still a verdict.
   *
   * A probe that throws and writes nothing is indistinguishable from a probe
   * that never ran, and those need different fixes — so every failure becomes
   * a report saying so.
   */
  let report: ConformanceReport;
  try {
    const assets = await backend.sandboxAssets();
    report = await runConformance({
      workerSource: assets.workerSource,
      canarySource: assets.canarySource,
    });
  } catch (error) {
    report = {
      ok: false,
      at: Date.now(),
      appOrigin: window.location.origin,
      guestOrigin: "",
      rows: [],
      failures: ["the probe itself failed"],
      error: error instanceof Error ? `${error.message}` : String(error),
    };
  }

  render(report);

  // With the boundary verified, run a real plugin through it — the same
  // `main.js` the design's worked example publishes, in the real iframe, on the
  // real origin, through the real worker. The conformance probe proves what a
  // plugin *cannot* do; this proves the channel still carries what it should.
  if (report.ok && (globalThis as Record<string, unknown>).__MACH_PLUGIN_DEMO__) {
    await runDemo(backend);
  }

  // The report file is the artefact: it outlives the window, and the probe
  // binary is waiting for it.
  await backend.reportConformance(report);
  return report;
}

/**
 * Worked example 1, end to end, in WKWebView.
 *
 * The mailbox is the fixture data source rather than the real store: the
 * command layer has its own 397 tests and a probe window has no Google
 * account, so pointing this at SQLite would prove less and cost more. What it
 * does prove is the part that only exists in a window — protocol handler,
 * guest, worker, module import, `mach.*` round trips, capability refusal,
 * grouped undo.
 */
async function runDemo(backend: ReturnType<typeof createPluginBackend>): Promise<void> {
  const paths = String((globalThis as Record<string, unknown>).__MACH_PLUGIN_DEMO__ ?? "")
    .split(",")
    .map((p) => p.trim())
    .filter(Boolean);
  const trace: string[] = [];

  try {
    for (const path of paths) {
      const candidate = await backend.inspect(path, true);
      trace.push(`inspect: ${candidate.manifest.name} ${candidate.manifest.version}`);
      for (const line of candidate.consent) trace.push(`consent[${line.severity}]: ${line.text}`);
      try {
        await backend.install(path, true);
        trace.push(`install ${candidate.manifest.id}: ok`);
      } catch (error) {
        trace.push(`install: ${error instanceof Error ? error.message : String(error)} (reused)`);
      }
    }

    const notices: string[] = [];
    const undoGroups: number[] = [];
    const manager = new PluginManager({
      backend,
      source: fixtureSource,
      ask: {
        // Nobody is looking at this window, so the picker answers itself —
        // with the *first* item, which is the ranking the plugin produced.
        async pick(o) {
          trace.push(`ask.pick "${o.title}" from ${o.pluginName}: ${o.items.length} items`);
          return o.items[0]?.value ?? null;
        },
        async text() {
          return null;
        },
        async confirm() {
          return true;
        },
      },
      notify: (message) => notices.push(message),
      log: () => {},
      onUndoGroup: (label, inverses) => {
        trace.push(`undo group "${label}": ${inverses.length} inverses`);
        undoGroups.push(inverses.length);
      },
    });

    await manager.start();
    trace.push(`installed: ${manager.list().map((p) => p.installed.id).join(", ") || "none"}`);

    await manager.run("quick-file", "file", { threadIds: [1, 2] });
    trace.push(`notify: ${notices.join(" | ")}`);
    trace.push(
      undoGroups[0] === 2
        ? "PASS: two commands, one undo group"
        : `FAIL: expected 2 inverses, got ${undoGroups[0]}`,
    );

    // And the refusal, through the same channel: an action asking for a
    // command its manifest never declared.
    try {
      await manager.run("quick-file", "nope", {});
      trace.push("FAIL: an action that does not exist was run");
    } catch (error) {
      trace.push(`refused unknown action: ${error instanceof Error ? error.message : error}`);
    }

    // Worked example 2, if it was installed: the reading-pane view, and the
    // snooze it resolves to.
    if (manager.list().some((entry) => entry.installed.id === "snooze-until-free")) {
      const node = (await manager.view("snooze-until-free", "next-free", { threadId: 1 })) as {
        type?: string;
        children?: { type?: string; label?: string; value?: string; action?: string }[];
      } | null;
      trace.push(
        node?.type === "section" && node.children?.[0]?.label === "Next free"
          ? `PASS: reading-pane view says "${node.children[0].value}"`
          : `FAIL: the view returned ${JSON.stringify(node)}`,
      );

      notices.length = 0;
      await manager.run("snooze-until-free", "snooze", { threadIds: [3], params: {} });
      trace.push(`snooze: ${notices.join(" | ")}`);
    }

    manager.destroy();
  } catch (error) {
    trace.push(`demo failed: ${error instanceof Error ? error.message : String(error)}`);
  }

  for (const line of trace) await say(`demo | ${line}`);
}

function render(report: ConformanceReport): void {
  const rows = report.rows
    .map(
      (row) =>
        `<tr><td>${escape(row.scope)}</td><td>${escape(row.name)}</td>` +
        `<td style="color:${row.allowed ? "#b00" : "#070"}">${row.allowed ? "ALLOWED" : "BLOCKED"}</td>` +
        `<td>${escape(row.detail)}</td></tr>`,
    )
    .join("");

  document.title = report.ok ? "sandbox verified" : "SANDBOX FAILED";
  document.body.innerHTML =
    `<main style="font:13px/1.5 ui-monospace,monospace;padding:24px">` +
    `<h1 style="font-size:15px">${escape(describeConformance(report))}</h1>` +
    `<p>app origin <code>${escape(report.appOrigin)}</code> · ` +
    `guest origin <code>${escape(report.guestOrigin)}</code></p>` +
    `<table cellpadding="4" style="border-collapse:collapse"><tbody>${rows}</tbody></table>` +
    `</main>`;
}

function escape(value: string): string {
  return String(value).replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c] ?? c,
  );
}
