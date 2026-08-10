/**
 * One sign-in, start to finish.
 *
 * The dialog is the same one "Add account" opens; what these pin is the part
 * "Sign in again" added — that the address a repair was started for reaches
 * Google, and that a refusal comes back as something to render rather than as
 * an unchanged dialog. `authorize` resolves for every outcome, so a failure
 * that is not surfaced would show up here as a rejected promise.
 */

import { describe, expect, it, vi } from "vitest";
import type { MachDataSource } from "@/lib/data";
import type { Account } from "@/types";
import { authorize } from "./AddAccountDialog";

const REPAIRING = "bruno.bornsztein@gmail.com";

function account(email: string): Account {
  return { id: 1, email, name: email, colorIndex: 1, kind: "personal" };
}

function source(overrides: Partial<MachDataSource> = {}) {
  return {
    beginAddAccount: vi.fn(async () => ({ url: "https://consent.example", pendingId: "p1" })),
    completeAddAccount: vi.fn(async () => account(REPAIRING)),
    openExternal: vi.fn(async () => {}),
    ...overrides,
  } as unknown as MachDataSource & {
    beginAddAccount: ReturnType<typeof vi.fn>;
    openExternal: ReturnType<typeof vi.fn>;
  };
}

describe("authorize", () => {
  it("starts the flow for the address being repaired", async () => {
    const src = source();
    const waiting = vi.fn();

    const phase = await authorize(src, REPAIRING, waiting);

    expect(src.beginAddAccount).toHaveBeenCalledWith(REPAIRING);
    expect(src.openExternal).toHaveBeenCalledWith("https://consent.example");
    // The consent page is up and the loopback listener is bound; the dialog has
    // something to say for however long the browser holds the user.
    expect(waiting).toHaveBeenCalledWith("https://consent.example");
    expect(phase).toEqual({ step: "done", email: REPAIRING });
  });

  it("starts with no address at all when adding an account", async () => {
    const src = source();
    await authorize(src);
    expect(src.beginAddAccount).toHaveBeenCalledWith(undefined);
  });

  it("ends as a message when Google refuses", async () => {
    // What Tauri throws: `IpcError` serialized as `{ kind, message }`.
    const src = source({
      completeAddAccount: vi.fn(async () => {
        throw { kind: "auth", message: "the consent window closed before Google answered" };
      }),
    });

    const phase = await authorize(src, REPAIRING);

    expect(phase).toEqual({
      step: "failed",
      message: "the consent window closed before Google answered",
    });
  });

  it("says which account signed in when it was not the one asked for", async () => {
    // `complete_add_account` checks the identity against the address the
    // handshake was started for and writes nothing when they differ, so the
    // account is still broken and the row has to say why rather than claim a
    // success or quietly connect somebody else.
    const src = source({
      completeAddAccount: vi.fn(async () => {
        throw {
          kind: "wrongAccount",
          message: `signed in as someone.else@gmail.com, not ${REPAIRING}`,
        };
      }),
    });

    const phase = await authorize(src, REPAIRING);

    expect(phase).toEqual({
      step: "failed",
      message: `signed in as someone.else@gmail.com, not ${REPAIRING}`,
    });
  });

  it("surfaces a failure from the very first step too", async () => {
    const src = source({
      beginAddAccount: vi.fn(async () => {
        throw { kind: "notConfigured", message: "no Google OAuth client is configured" };
      }),
    });

    const phase = await authorize(src, REPAIRING);

    expect(phase).toEqual({ step: "failed", message: "no Google OAuth client is configured" });
  });
});
