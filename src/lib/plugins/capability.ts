/**
 * The capability check — the one piece of policy on the host side of the wall.
 *
 * Every `mach.*` call a plugin makes arrives as a method name and an argument
 * list, and is checked here *before* the implementation is even looked up. That
 * ordering matters: a method the manifest did not license is indistinguishable
 * from a method that does not exist, which is the behaviour a plugin author
 * should get and which leaks nothing about what else the host can do.
 *
 * The refusal is a *sentence*, not a boolean, because the message goes straight
 * to a plugin author: `quick-file did not declare commands: ["trash"]` is a bug
 * report that fixes itself.
 *
 * This runs in the app's window, never in the guest. The guest is never trusted
 * to enforce anything about itself; it is only trusted to be unable to do
 * anything else, which is what the conformance probe exists to verify. The Rust
 * side checks the same grant a second time on `execute_command`, because the
 * command layer is the trust boundary that actually matters.
 */

import type { PluginManifest } from "./types";

/** Every method the worker shim can name. Anything else is not a method. */
export const MACH_METHODS = [
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
] as const;

export type MachMethod = (typeof MACH_METHODS)[number];

/** Which read capability each read method needs. */
const READ_CAPABILITY: Record<string, string> = {
  "read.threads": "threads.metadata",
  "read.thread": "threads",
  "read.events": "calendar",
  "read.labels": "labels",
  "read.accounts": "accounts",
};

/**
 * Why this call is not allowed, or `null` if it is.
 */
export function capabilityDenial(
  manifest: PluginManifest,
  method: string,
  args: unknown[],
): string | null {
  const id = manifest.id;
  const caps = manifest.capabilities;

  if (!(MACH_METHODS as readonly string[]).includes(method)) {
    return `no such method: ${method}`;
  }

  if (method === "run") {
    const command = args[0] as { kind?: string } | undefined;
    const kind = command?.kind;
    if (typeof kind !== "string") {
      return `${id} called mach.run without a command`;
    }
    if (!caps.commands.includes(kind)) {
      return `${id} did not declare commands: ["${kind}"]`;
    }
    return null;
  }

  if (method.startsWith("read.")) {
    const need = READ_CAPABILITY[method];
    // "threads" implies "threads.metadata"; nothing else implies anything, and
    // in particular metadata never implies bodies.
    const ok =
      caps.read.includes(need) ||
      (need === "threads.metadata" && caps.read.includes("threads"));
    return ok ? null : `${id} did not declare read: ["${need}"]`;
  }

  if (method.startsWith("store.") && !caps.store) {
    return `${id} did not declare store`;
  }

  if (method.startsWith("ask.") && caps.ui.length === 0) {
    return `${id} declared no ui capability, so it cannot prompt`;
  }

  return null;
}

/** Whether the agent may call one of this plugin's actions. Opt-*out*. */
export function agentMayCall(manifest: PluginManifest, actionId: string): boolean {
  const grant = manifest.capabilities.agent;
  if (Array.isArray(grant)) return grant.includes(actionId);
  return grant !== false;
}
