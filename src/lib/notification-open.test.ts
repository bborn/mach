import { describe, expect, it, vi } from "vitest";

import { connectNotificationOpen, type NotificationOpen } from "./notification-open";

const TARGET: NotificationOpen = {
  accountId: 2,
  threadId: 41,
  gmailThreadId: "18f",
  atMs: 1_800_000_000_000,
};

/** A push channel whose `off()` genuinely detaches. */
function channel() {
  let emit: ((target: NotificationOpen) => void) | null = null;
  const off = vi.fn(() => {
    emit = null;
  });
  return {
    subscribe: async (handler: (target: NotificationOpen) => void) => {
      emit = handler;
      return off;
    },
    off,
    fire: (target: NotificationOpen) => emit?.(target),
  };
}

describe("connectNotificationOpen", () => {
  it("opens the conversation the push event names", () => {
    const ch = channel();
    const onOpen = vi.fn();
    connectNotificationOpen(onOpen, {
      subscribe: ch.subscribe,
      claimPending: async () => null,
    });

    ch.fire(TARGET);
    expect(onOpen).toHaveBeenCalledWith(TARGET);
  });

  it("claims a banner clicked before anything was listening", async () => {
    // The startup case the push event cannot cover: the click fires
    // OPEN_EVENT while the window is still booting, so the claim would sit
    // there until it expired.
    const onOpen = vi.fn();
    connectNotificationOpen(onOpen, {
      subscribe: async () => () => {},
      claimPending: async () => TARGET,
    });

    await Promise.resolve();
    await Promise.resolve();
    expect(onOpen).toHaveBeenCalledWith(TARGET);
  });

  it("does nothing on the usual launch, when no banner was clicked", async () => {
    const onOpen = vi.fn();
    connectNotificationOpen(onOpen, {
      subscribe: async () => () => {},
      claimPending: async () => null,
    });

    await Promise.resolve();
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("survives the pending claim failing", async () => {
    // Worst case is a banner that does not open its thread; it must not throw
    // during the provider's mount effect.
    const onOpen = vi.fn();
    expect(() =>
      connectNotificationOpen(onOpen, {
        subscribe: async () => () => {},
        claimPending: async () => {
          throw new Error("no such command");
        },
      }),
    ).not.toThrow();

    await Promise.resolve();
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("refuses a malformed claim rather than selecting a thread that cannot exist", async () => {
    // Dispatching a nonsense row id would leave the reading pane on a
    // permanent spinner.
    const onOpen = vi.fn();
    const ch = channel();
    connectNotificationOpen(onOpen, {
      subscribe: ch.subscribe,
      claimPending: async () => null,
    });

    ch.fire({ ...TARGET, threadId: 0 });
    ch.fire({ ...TARGET, threadId: -1 });
    ch.fire({ ...TARGET, threadId: 1.5 });
    ch.fire({ ...TARGET, accountId: 0 });

    expect(onOpen).not.toHaveBeenCalled();
  });

  it("stops delivering once disconnected", async () => {
    const ch = channel();
    const onOpen = vi.fn();
    const disconnect = connectNotificationOpen(onOpen, {
      subscribe: ch.subscribe,
      claimPending: async () => null,
    });

    await Promise.resolve();
    disconnect();

    expect(ch.off).toHaveBeenCalled();
    ch.fire(TARGET);
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("does not deliver a claim that resolves after teardown", async () => {
    // The provider can unmount while the command is still in flight.
    let resolve: (target: NotificationOpen | null) => void = () => {};
    const onOpen = vi.fn();
    const disconnect = connectNotificationOpen(onOpen, {
      subscribe: async () => () => {},
      claimPending: () => new Promise((r) => (resolve = r)),
    });

    disconnect();
    resolve(TARGET);
    await Promise.resolve();

    expect(onOpen).not.toHaveBeenCalled();
  });
});
