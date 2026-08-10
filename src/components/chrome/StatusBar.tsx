import { usePendingSequence } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import { formatBinding } from "@/lib/keymap";
import { SyncIndicator } from "./SyncIndicator";

/**
 * The bottom rail carries what is *true*, not what just happened: how much is
 * in front of you, where a half-typed sequence stands, and the bindings for the
 * current mode.
 *
 * It used to carry the transient status message as well, and that was the whole
 * problem — an acknowledgement of something that just moved rows off screen,
 * set in 11px on a 24px rail beside the sync spinner. `chrome/Toast.tsx` shows
 * it now, loudly and with the button attached.
 *
 * # Nothing here is a copy of a message
 *
 * The rail kept one piece of that for a while: a permanent "⌘Z Undo archived 1
 * conversation", on the argument that a key nobody can name is a key nobody
 * presses. The toast already names it, at the moment it means something, with
 * the button attached — so the line was a second copy of a transient message,
 * sitting in the corner of the window long after the archive it described. ⌘Z
 * works exactly as it did; the stack still never expires. It simply is not
 * announced by furniture any more.
 *
 * What is left passes the same test: every item is a fact about the current
 * view that nothing else on screen states. The pending `g …`, the fixture
 * warning, and the sync indicator — which renders nothing at all unless a sync
 * is running or has failed.
 *
 * # Nor was the selection
 *
 * The rail counted the selection too, and stopped when the count acquired a
 * job. `mail/SelectionBar.tsx` says what the ticked rows can be done with, and
 * a count of them belongs beside the verbs that act on it rather than in the
 * opposite corner of the window from the checkboxes it describes. Two copies of
 * "6 selected" was already one too many before either of them meant anything.
 *
 * # The keymap is not one of those facts
 *
 * The rail also carried a permanent legend of the current mode's bindings —
 * `j k move · ⏎ open · x select · e archive · b snooze · r reply · c new` in
 * mail, the three view keys in calendar. It failed the same test twice over.
 * In mail it sat directly under the composer strip, which offers `r` and `c`
 * again as buttons; in calendar every one of its three rows was a second copy
 * of a button in the calendar header. And a legend of the whole keymap is not
 * a fact about the current view at all — it says the same thing in an empty
 * inbox as in an open thread, which is what makes it furniture.
 *
 * `?` opens the reference card. That is where a legend belongs, and the sheet
 * is one key away rather than buried in settings. Bindings are untouched.
 *
 * # Neither was the count
 *
 * The rail also carried `${n} conversations` in mail mode and `${view} view`
 * in calendar mode. Both failed the same test: the mail list header already
 * shows `Inbox · All accounts · 48` above the column being counted, and the
 * calendar header already highlights the active view in its Day/Week/Month
 * control. Both were a second copy of a number already on screen.
 */
export function StatusBar() {
  const { live } = useMach();
  const pending = usePendingSequence();

  return (
    // No border of its own: the container in `App.tsx` draws the one edge the
    // bottom of the window needs, whichever of these strips is topmost.
    <footer className="flex h-6 shrink-0 items-center gap-3 overflow-hidden bg-surface px-3">
      {pending && (
        <span className="shrink-0 whitespace-nowrap font-mono text-micro text-accent">
          {formatBinding(pending)} …
        </span>
      )}

      {/* Fixture data looks exactly like real mail, so say when it isn't. */}
      {!live && (
        <span
          title="Fixture data"
          className="shrink-0 whitespace-nowrap font-mono text-micro uppercase tracking-[0.06em] text-warning"
        >
          fixtures
        </span>
      )}

      <span className="ml-auto flex min-w-0 shrink items-center justify-end">
        <SyncIndicator />
      </span>
    </footer>
  );
}
