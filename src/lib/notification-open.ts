/**
 * Opening the conversation a notification banner was about.
 *
 * Both of the Rust-side paths arrive here, and the difference between them
 * matters only there. `notify::mac` sends each banner on a thread that blocks
 * until macOS reports the interaction, so an ordinary click comes back to the
 * process on a thread that already knows which conversation its own banner was
 * about — exact, with nothing guessed. `notify::PendingOpen` is the fallback
 * for the banners that cannot have that: it remembers the newest target and
 * hands it over on the next activation, within two minutes, because an
 * activation long after a banner is more likely to be someone clicking the Dock
 * icon and opening a conversation nobody asked for is worse than opening none.
 *
 * Either way what reaches this file is a conversation to select.
 *
 * # Why there are two channels
 *
 * The push event is the normal path. The one-shot command exists for the case
 * the event cannot cover: a banner clicked while the window is still starting
 * fires `OPEN_EVENT` before anything is listening, and the claim would sit
 * there unclaimed until it expired. So the frontend also asks once on mount.
 * Both funnel through the same handler, and the Rust side consumes the claim,
 * so whichever arrives first wins and the other finds nothing.
 */

import { isTauri } from "./ipc";

/** Must match `notify::host::OPEN_EVENT`. */
export const NOTIFICATION_OPEN_EVENT = "mach://notification-open";

/** Mirrors `notify::PendingOpen`. */
export interface NotificationOpen {
  accountId: number;
  threadId: number;
  gmailThreadId: string;
  atMs: number;
}

export interface NotificationOpenOptions {
  /** Injected in tests; defaults to the Tauri event channel. */
  subscribe?: (
    handler: (target: NotificationOpen) => void,
  ) => Promise<() => void>;
  /** Injected in tests; defaults to the `notification_pending_open` command. */
  claimPending?: () => Promise<NotificationOpen | null>;
}

/**
 * Calls `onOpen` when a notification click should open a conversation.
 *
 * Outside Tauri there are no notifications, so this does nothing and costs
 * nothing.
 */
export function connectNotificationOpen(
  onOpen: (target: NotificationOpen) => void,
  options: NotificationOpenOptions = {},
): () => void {
  const injected = options.subscribe ?? options.claimPending;
  if (!injected && !isTauri()) return () => {};

  const subscribe = options.subscribe ?? defaultSubscribe;
  const claimPending = options.claimPending ?? defaultClaimPending;

  let live = true;
  let unsubscribe: (() => void) | null = null;

  const deliver = (target: NotificationOpen | null) => {
    if (live && target && isUsable(target)) onOpen(target);
  };

  void subscribe((target) => deliver(target)).then((off) => {
    if (live) unsubscribe = off;
    else off();
  });

  // The startup case. A rejection here is not worth surfacing: the worst
  // outcome is that a banner clicked during launch does not open its thread.
  void claimPending()
    .then(deliver)
    .catch(() => {});

  return () => {
    live = false;
    unsubscribe?.();
  };
}

/**
 * Guards against a malformed claim reaching the reducer.
 *
 * `threadId` is a local row id, and dispatching a nonsense one would select a
 * conversation that does not exist and leave the reading pane wedged on a
 * permanent spinner.
 */
function isUsable(target: NotificationOpen): boolean {
  return (
    Number.isInteger(target.threadId) &&
    target.threadId > 0 &&
    Number.isInteger(target.accountId) &&
    target.accountId > 0
  );
}

async function defaultSubscribe(
  handler: (target: NotificationOpen) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  const off = await listen<NotificationOpen>(NOTIFICATION_OPEN_EVENT, (event) =>
    handler(event.payload),
  );
  return () => void off();
}

async function defaultClaimPending(): Promise<NotificationOpen | null> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<NotificationOpen | null>("notification_pending_open");
}
