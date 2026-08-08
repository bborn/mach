/**
 * Registration into the two frontend registries.
 *
 * A plugin action has to become a ⌘K entry and a keybinding **from the manifest
 * alone**, before any plugin code has run — that is what makes activation lazy
 * and what makes a plugin cost nothing until it is used. These tests drive the
 * real resolver chain, so they also pin the two things that would otherwise
 * regress silently: that a plugin cannot outrank a core command, and that a
 * plugin's binding sits below every core binding.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { resolve, type PaletteContext } from "@/lib/palette/resolver";
import { createKeymap } from "@/lib/keymap";
import { clearPaletteActions, setPaletteActions } from "./palette";
import type { InstalledPlugin } from "./types";

const plugin: InstalledPlugin = {
  id: "quick-file",
  status: { state: "ready" },
  directory: "",
  manifest: {
    id: "quick-file",
    name: "Quick File",
    version: "1.0.0",
    machApi: "1",
    description: "",
    author: "",
    main: "main.js",
    machApiProposed: [],
    runtime: "sandbox",
    capabilities: {
      read: ["labels"],
      commands: ["label", "archive"],
      ui: ["palette"],
      events: [],
      store: true,
      agent: false,
    },
    contributes: {
      actions: [
        {
          id: "file",
          title: "File to label…",
          keywords: "move folder sort",
          key: "alt+f",
          context: "threads",
          params: [],
        },
      ],
      views: [],
    },
  },
};

function context(query: string): PaletteContext {
  return {
    query,
    threads: [],
    events: [],
    people: [],
    mailboxes: [],
    commands: [
      { id: "archive", title: "Archive conversation", hint: "E", keywords: "archive done" },
    ],
    actions: {
      openThread: () => {},
      openEvent: () => {},
      openMailbox: () => {},
      runCommand: () => {},
      composeTo: () => {},
    },
  };
}

afterEach(() => clearPaletteActions());

describe("⌘K", () => {
  it("offers a plugin action from the manifest, without running the plugin", () => {
    const run = vi.fn();
    setPaletteActions([{ plugin, action: plugin.manifest.contributes.actions[0] }], run);

    const results = resolve(context("file to"));
    const entry = results.find((r) => r.id === "plugin:quick-file:file");
    expect(entry).toBeDefined();
    // Attributed, so the user can see whose code they are about to run.
    expect(entry?.subtitle).toBe("Quick File");

    entry?.run();
    expect(run).toHaveBeenCalledWith("quick-file", "file");
  });

  it("finds it by its declared keywords", () => {
    setPaletteActions([{ plugin, action: plugin.manifest.contributes.actions[0] }], () => {});
    expect(resolve(context("folder")).some((r) => r.id === "plugin:quick-file:file")).toBe(true);
  });

  /** A plugin's "Archive everything" must never outrank the real Archive. */
  it("ranks below the core command with the same word", () => {
    const archivePlugin = {
      ...plugin,
      manifest: {
        ...plugin.manifest,
        contributes: {
          views: [],
          actions: [
            {
              id: "archive",
              title: "Archive conversation",
              keywords: "archive",
              context: "threads" as const,
              params: [],
            },
          ],
        },
      },
    };
    setPaletteActions(
      [{ plugin: archivePlugin, action: archivePlugin.manifest.contributes.actions[0] }],
      () => {},
    );

    const results = resolve(context("archive"));
    const core = results.findIndex((r) => r.id === "command:archive");
    const fromPlugin = results.findIndex((r) => r.id === "plugin:quick-file:archive");
    expect(core).toBeGreaterThanOrEqual(0);
    expect(fromPlugin).toBeGreaterThan(core);
  });

  it("contributes nothing when no plugin is installed", () => {
    expect(resolve(context("file")).some((r) => r.id.startsWith("plugin:"))).toBe(false);
  });
});

describe("the keymap", () => {
  /**
   * The priority band. Core shell bindings sit at 0, so a plugin at -10 can
   * never take `e` — and the registry decides that, not the order components
   * happened to mount in.
   */
  it("gives a core binding the key when a plugin wants the same one", () => {
    const keymap = createKeymap("meta");
    const core = vi.fn();
    const fromPlugin = vi.fn();

    // The plugin registers *last*, which under the registry's tie-break would
    // otherwise win.
    keymap.register({ keys: "e", handler: core, priority: 0 });
    keymap.register({ keys: "e", handler: fromPlugin, priority: -10 });

    keymap.handle({ key: "e", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false });
    expect(core).toHaveBeenCalled();
    expect(fromPlugin).not.toHaveBeenCalled();
  });

  it("still fires a plugin binding nothing else claims", () => {
    const keymap = createKeymap("meta");
    const handler = vi.fn();
    keymap.register({ keys: "alt+f", handler, priority: -10 });

    keymap.handle({
      key: "f",
      code: "KeyF",
      metaKey: false,
      ctrlKey: false,
      altKey: true,
      shiftKey: false,
    });
    expect(handler).toHaveBeenCalled();
  });

  /** Two plugins wanting ⌥F is a conflict the existing reporting surfaces. */
  it("reports two plugins claiming one key", () => {
    const keymap = createKeymap("meta");
    keymap.register({ keys: "alt+f", handler: () => {}, priority: -10, description: "a" });
    keymap.register({ keys: "alt+f", handler: () => {}, priority: -10, description: "b" });
    expect(keymap.conflicts().map((c) => c.keys)).toEqual(["alt+f"]);
  });
});
