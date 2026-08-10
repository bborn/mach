import { useCallback, useEffect, useState } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { HELD_EVENT, HELLO_EVENT, TAKE_EVENT, type HeldNotice } from "@/lib/hmr-hold";
import { ToastCard, type ToastAction } from "./Toast";

/**
 * "There is new frontend code — take it when you like."
 *
 * Mach is written while it is being used to read mail, from a dev build, by
 * agents committing into the same checkout Vite is serving. Before this, every
 * commit landed in the window immediately: a module swap at best, a full page
 * reload at worst, taking the open conversation, the scroll position, the
 * selection and any open composer with it. The complaint was never about the
 * code arriving. It was about not choosing when.
 *
 * So the dev server holds updates back and says so, and this is the saying so.
 * The withholding is in `lib/hmr-hold.ts` and the plugin in `vite.config.ts`;
 * this file is one card and one binding.
 *
 * Taking it reloads the page — see `hmr-hold.ts` for why it is not the module
 * swap Vite had planned. So the offer is a real question, and it stays on
 * screen until it is answered rather than timing out like a status message.
 *
 * # Development only, and absent rather than inert
 *
 * `App` mounts it as `import.meta.env.DEV && <HeldUpdate />`. Vite substitutes
 * the literal `false` in a production build, the whole expression folds away,
 * and the import goes with it — the shipped bundle has no card, no binding and
 * no listener, the same claim `lib/qa-bridge.ts` makes about the QA port. A
 * production build has no Vite behind it and therefore nothing to hold.
 *
 * # Why not a status message
 *
 * `ui.status` is timed out by `useMach` within a few seconds and replaced by
 * the next thing the app has to say, which is right for "Archived 3
 * conversations" and wrong for an offer nobody has answered yet. This sits in
 * the same layer, wearing the same card, and stays.
 */

/** Gmail has no reload; ⌘R is the one every window on this machine already has. */
export const TAKE_KEYS = "mod+r";

/**
 * The card, with nothing wired to it.
 *
 * Separate so it can be rendered and asserted on without a keymap, a dev
 * server or a DOM — the same reason `ToastLayer` is separate from `Toast`.
 */
export function UpdateOffer({
  onTake,
  onDismiss,
}: {
  onTake: () => void;
  onDismiss: () => void;
}) {
  const action: ToastAction = {
    word: "Update",
    title: "Update to the new version",
    keys: TAKE_KEYS,
    run: onTake,
  };
  return (
    <ToastCard message="New version" tone="info" action={action} onDismiss={onDismiss} />
  );
}

export function HeldUpdate() {
  /** What the server is holding. */
  const [waiting, setWaiting] = useState(false);
  /** Whether the card is up. Dismissing lowers this and not the offer. */
  const [showing, setShowing] = useState(false);

  useEffect(() => {
    const hot = import.meta.hot;
    if (!hot) return;

    const heard = (notice: HeldNotice) => {
      setWaiting(notice.waiting);
      // A fresh hold re-raises a card that was dismissed; there is something
      // new to say, and the old dismissal was about the old news.
      if (notice.waiting) setShowing(true);
    };
    hot.on(HELD_EVENT, heard);

    // This window has just loaded, so it already has whatever was being held
    // for its predecessor. Without this, taking an update that ends in a
    // reload would come back to a toast offering the update it had just taken.
    hot.send(HELLO_EVENT);

    return () => hot.off(HELD_EVENT, heard);
  }, []);

  const take = useCallback(() => {
    setShowing(false);
    setWaiting(false);
    import.meta.hot?.send(TAKE_EVENT);
  }, []);

  useKeyBindings([
    {
      keys: TAKE_KEYS,
      group: "Global",
      description: "Update to the new version",
      // Reachable from inside the composer: deciding to take an update while
      // half way through a reply is exactly when the keystroke has to work.
      allowInInput: true,
      when: () => waiting,
      handler: take,
    },
  ]);

  return (
    // Mounted whether or not it has anything in it — a live region added to the
    // page at the same moment as its content is not reliably announced.
    <div role="status" aria-live="polite" aria-atomic="true">
      {waiting && showing ? (
        // Dismissing hides the card and leaves the offer standing, so ⌘R still
        // takes it — and the `?` sheet still lists it.
        <UpdateOffer onTake={take} onDismiss={() => setShowing(false)} />
      ) : null}
    </div>
  );
}
