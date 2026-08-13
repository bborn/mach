// @vitest-environment jsdom

/**
 * Handing off before the sentence exists.
 *
 * The palette puts "Hand off to …" on top from the first letter of the word
 * that finds it, so ⏎ regularly arrives while the query is still `hando` and
 * the instruction is empty. That used to end in a panel headed "Nothing was
 * launched" with a warning in it and a Done button — the app naming exactly
 * what was missing and then offering no way to supply it.
 *
 * So: the field, with the caret in it. What these pin is that the empty
 * invocation opens that field rather than the refusal, that ⏎ from inside it
 * launches with what was typed, that an empty ⏎ launches nothing at all, and
 * that a handoff which already had its sentence is untouched by any of it.
 *
 * Driven through the real window: `__TAURI_INTERNALS__` is what
 * `@tauri-apps/api` invokes through and what `isTauri()` looks for, so the
 * assertions below are about the command that actually left the frontend.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { MachProvider } from "@/hooks/useMach";
import { closeHandoff, openHandoff, setTargets, type HandoffTarget } from "@/lib/handoff";
import { HandoffDialog } from "./HandoffDialog";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

/** Proven and inline, so a handoff with a sentence runs without confirming. */
const TARGET: HandoffTarget = {
  id: "t1",
  name: "OfferLab",
  dir: "~/Projects/offerlab",
  run: 'claude "{{prompt}}"',
  mode: "inline",
  lastRunAt: 1_700_000_000_000,
};

interface Call {
  command: string;
  args: Record<string, unknown>;
}

let container: HTMLDivElement;
let root: Root;
let calls: Call[];

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  if (!window.matchMedia) {
    window.matchMedia = ((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener() {},
      removeEventListener() {},
      addListener() {},
      removeListener() {},
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia;
  }

  calls = [];
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
    // The provider attaches its event listeners through this; nothing in these
    // assertions comes back down one, so an id is the whole of the answer.
    transformCallback: () => 1,
    invoke: (command: string, args: Record<string, unknown> = {}) => {
      calls.push({ command, args });
      if (command.startsWith("plugin:")) return Promise.resolve(1);
      if (command === "handoff_targets") return Promise.resolve([TARGET]);
      if (command === "handoff_run" || command === "handoff_preview") {
        // What `LaunchPlan::prepare` does, and still does: an empty note is
        // refused at the boundary. Nothing below should ever reach it.
        if (!String(args.note ?? "").trim()) {
          return Promise.reject({ kind: "handoff", message: "no instruction" });
        }
      }
      if (command === "handoff_run") {
        return Promise.resolve({
          targetName: TARGET.name,
          mode: "inline",
          dir: TARGET.dir,
          command: "claude …",
          contextFile: "/tmp/context.txt",
          message: "claude answered",
          status: 0,
          stdout: "on it",
          stderr: "",
        });
      }
      return Promise.reject(new Error(`unexpected command ${command}`));
    },
  };
  (window as unknown as Record<string, unknown>).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: () => Promise.resolve(),
  };

  setTargets([TARGET]);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  closeHandoff();
  setTargets([]);
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  delete (window as unknown as Record<string, unknown>).__TAURI_EVENT_PLUGIN_INTERNALS__;
  globalThis.IS_REACT_ACT_ENVIRONMENT = undefined;
});

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function mount() {
  await act(async () => {
    root.render(
      <KeymapProvider>
        <MachProvider>
          <HandoffDialog />
        </MachProvider>
      </KeymapProvider>,
    );
  });
  await flush();
}

/** ⌘K chose a target with `note` as the instruction. */
async function invokeHandoff(note: string) {
  await act(async () => openHandoff("t1", note));
  await flush();
}

function field(): HTMLInputElement | null {
  return document.querySelector<HTMLInputElement>("#handoff-note");
}

function type(value: string) {
  const input = field();
  if (!input) throw new Error("the handoff field is not on screen");
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** A real keystroke at the element under the caret. The registry listens on the window. */
async function press(key: string) {
  const at = (document.activeElement as HTMLElement | null) ?? document.body;
  await act(async () => {
    at.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  });
  await flush();
}

function text(): string {
  return document.body.textContent ?? "";
}

function launches(): Call[] {
  return calls.filter((call) => call.command === "handoff_run" || call.command === "handoff_preview");
}

describe("a handoff invoked with no instruction", () => {
  it("opens the field instead of the refusal, with the caret already in it", async () => {
    await mount();
    await invokeHandoff("");

    const input = field();
    expect(input).not.toBeNull();
    expect(document.activeElement).toBe(input);
    expect(input?.value).toBe("");

    // The refusal is gone, and so is the sentence it used to lecture with.
    expect(text()).not.toContain("Nothing was launched");
    expect(text()).not.toContain("a handoff is your sentence");
    // Nothing has been asked of Rust: there is nothing to plan yet.
    expect(launches()).toEqual([]);
  });

  it("launches with the sentence typed into it when Return is pressed", async () => {
    await mount();
    await invokeHandoff("");

    type("reschedule the standups");
    await press("Enter");

    const run = calls.filter((call) => call.command === "handoff_run");
    expect(run).toHaveLength(1);
    expect(run[0].args).toMatchObject({ targetId: "t1", note: "reschedule the standups" });

    // And it visibly became the running handoff — the field is gone and what
    // came back is on screen.
    expect(field()).toBeNull();
    expect(text()).toContain("claude answered");
  });

  it("launches with the sentence when the button is used instead", async () => {
    await mount();
    await invokeHandoff("");

    type("draft a reply to Katie");
    const button = [...document.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "Hand off",
    );
    expect(button?.disabled).toBe(false);
    await act(async () => button?.click());
    await flush();

    const run = calls.filter((call) => call.command === "handoff_run");
    expect(run).toHaveLength(1);
    expect(run[0].args).toMatchObject({ note: "draft a reply to Katie" });
  });

  it("stays put on an empty Return: nothing launched, nothing said about it", async () => {
    await mount();
    await invokeHandoff("");

    await press("Enter");

    expect(launches()).toEqual([]);
    expect(field()).not.toBeNull();
    expect(document.activeElement).toBe(field());
    expect(text()).not.toContain("Nothing was launched");

    // The submit control says the same thing by being unavailable.
    const button = [...document.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "Hand off",
    );
    expect(button?.disabled).toBe(true);
  });

  it("closes on Escape without launching anything", async () => {
    await mount();
    await invokeHandoff("");

    await press("Escape");

    expect(field()).toBeNull();
    expect(launches()).toEqual([]);
  });

  it("only trims what it hands over", async () => {
    await mount();
    await invokeHandoff("");

    type("   ");
    await press("Enter");
    expect(launches()).toEqual([]);

    type("  read this and reply  ");
    await press("Enter");

    const run = calls.filter((call) => call.command === "handoff_run");
    expect(run).toHaveLength(1);
    expect(run[0].args).toMatchObject({ note: "read this and reply" });
  });
});

describe("a handoff that already has its sentence", () => {
  it("launches straight away, with no field in the way", async () => {
    await mount();
    await invokeHandoff("implement the feature Katie asked for");

    expect(field()).toBeNull();
    const run = calls.filter((call) => call.command === "handoff_run");
    expect(run).toHaveLength(1);
    expect(run[0].args).toMatchObject({
      targetId: "t1",
      note: "implement the feature Katie asked for",
    });
    expect(text()).toContain("claude answered");
  });
});
